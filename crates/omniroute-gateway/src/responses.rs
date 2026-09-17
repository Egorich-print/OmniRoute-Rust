//! Generic OpenAI Responses (`openai-responses` format) backend over HTTP.
//!
//! Handles providers like `deepseek` that expose a `/responses` API endpoint
//! rather than `/chat/completions`. Translates OpenAI chat requests to
//! Responses requests using `omniroute_translate::requests::openai_to_responses`
//! and responses back via `omniroute_translate::responses`.

use async_trait::async_trait;
use futures::StreamExt;
use omniroute_core::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError,
    StreamChunk, Usage,
};
use omniroute_http::{HttpClient, SseDecoder};
use omniroute_translate::{
    requests::openai_to_responses,
    responses::{Converted, ResponsesState, convert_event},
};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::backend::{ChatBackend, ChunkStream, openai_chunk_to_stream_chunk, openai_request_body};
use crate::ids;

/// Generic Responses API executor over HTTP with token rotation.
#[derive(Debug, Clone)]
pub struct ResponsesBackend {
    http: HttpClient,
    provider: String,
    endpoint_url: String,
    tokens: Vec<String>,
    next: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl ResponsesBackend {
    /// Build for `provider` against `endpoint_url` with rotating tokens.
    pub fn new(
        provider: String,
        endpoint_url: String,
        tokens: Vec<String>,
    ) -> Result<Self, omniroute_http::client::HttpError> {
        Ok(Self {
            http: HttpClient::new(600)?,
            provider,
            endpoint_url: endpoint_url.trim_end_matches('/').to_string(),
            tokens,
            next: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
    }

    /// Pool size (number of rotated credentials).
    pub fn token_count(&self) -> usize {
        self.tokens.len()
    }

    /// Round-robin token selection (None if keyless).
    fn next_token(&self) -> Option<&str> {
        if self.tokens.is_empty() {
            return None;
        }
        let index =
            self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % self.tokens.len();
        self.tokens.get(index).map(String::as_str)
    }

    async fn drive_stream(
        &self,
        request: ChatCompletionRequest,
        tx: mpsc::Sender<Result<StreamChunk, GatewayError>>,
    ) -> Result<(), GatewayError> {
        if request.messages.is_empty() {
            return Err(GatewayError::InvalidRequest("messages is empty".into()));
        }

        let mut upstream = openai_to_responses(&request.model, &openai_request_body(&request));
        upstream["stream"] = Value::from(true);
        let response = self
            .http
            .post_json(&self.endpoint_url, self.next_token(), &upstream)
            .await
            .map_err(|error| GatewayError::Upstream(error.to_string()))?;

        let mut bytes = response.bytes_stream();
        let mut decoder = SseDecoder::new();
        let mut translator = ResponsesState::new(
            "chatcmpl-rust".to_string(),
            ids::unix_seconds(),
            request.model.clone(),
        );
        let mut finish_seen = false;

        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(|error| GatewayError::Upstream(error.to_string()))?;
            let text = String::from_utf8_lossy(&chunk);
            for event in decoder.push(&text) {
                if event.data.trim() == "[DONE]" {
                    if !finish_seen {
                        let _ = tx.send(Ok(StreamChunk::done("stop"))).await;
                    }
                    return Ok(());
                }
                let data: Value = match serde_json::from_str(&event.data) {
                    Ok(data) => data,
                    Err(_) => continue,
                };
                let event_type = event
                    .event
                    .as_deref()
                    .or_else(|| data.get("type").and_then(Value::as_str))
                    .unwrap_or("");
                match convert_event(&mut translator, event_type, &data) {
                    Converted::Emit(chunk_json) => {
                        if let Some(stream_chunk) = openai_chunk_to_stream_chunk(&chunk_json) {
                            if stream_chunk.finish_reason.is_some() {
                                finish_seen = true;
                            }
                            if tx.send(Ok(stream_chunk)).await.is_err() {
                                return Ok(());
                            }
                        }
                    }
                    Converted::Ignore => {}
                    Converted::Fail(message) => {
                        return Err(GatewayError::Upstream(message));
                    }
                }
            }
        }

        if !finish_seen {
            let _ = tx.send(Ok(StreamChunk::done("stop"))).await;
        }
        Ok(())
    }
}

#[async_trait]
impl ChatBackend for ResponsesBackend {
    fn name(&self) -> &'static str {
        "responses"
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        if request.messages.is_empty() {
            return Err(GatewayError::InvalidRequest("messages is empty".into()));
        }
        let mut upstream = openai_to_responses(&request.model, &openai_request_body(&request));
        upstream["stream"] = Value::from(false);

        let mut last_error: Option<GatewayError> = None;
        for _ in 0..self.tokens.len().max(1) {
            let response = match self
                .http
                .post_json(&self.endpoint_url, self.next_token(), &upstream)
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    last_error = Some(GatewayError::Upstream(error.to_string()));
                    continue;
                }
            };
            let payload: Value = match response.json().await {
                Ok(payload) => payload,
                Err(error) => {
                    last_error = Some(GatewayError::Upstream(error.to_string()));
                    continue;
                }
            };
            if let Some(message) = payload
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
            {
                last_error = Some(GatewayError::Upstream(message.to_string()));
                continue;
            }
            return Ok(response_to_completion(&request.model, &payload));
        }
        Err(last_error.unwrap_or_else(|| {
            GatewayError::Upstream(format!("{}: all credentials failed", self.provider))
        }))
    }

    fn stream(&self, request: ChatCompletionRequest) -> ChunkStream {
        let (tx, rx) = mpsc::channel(64);
        let backend = self.clone();
        tokio::spawn(async move {
            if let Err(error) = backend.drive_stream(request, tx.clone()).await {
                let _ = tx.send(Err(error)).await;
            }
        });
        Box::pin(ReceiverStream::new(rx))
    }
}

/// Convert a completed Responses API object to a chat completion.
fn response_to_completion(model: &str, payload: &Value) -> ChatCompletionResponse {
    let id = payload
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("resp-unknown")
        .to_string();

    let mut text = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    if let Some(output) = payload.get("output").and_then(Value::as_array) {
        for item in output {
            match item.get("type").and_then(Value::as_str) {
                Some("message") => {
                    if let Some(content) = item.get("content").and_then(Value::as_array) {
                        for part in content {
                            if part.get("type").and_then(Value::as_str) == Some("output_text")
                                && let Some(part_text) = part.get("text").and_then(Value::as_str)
                            {
                                text.push_str(part_text);
                            }
                        }
                    }
                }
                Some("function_call") => {
                    let call_id = item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .or_else(|| item.get("id").and_then(Value::as_str))
                        .unwrap_or("");
                    let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                    if name.is_empty() {
                        continue;
                    }
                    tool_calls.push(json!({
                        "id": call_id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": item
                                .get("arguments")
                                .and_then(Value::as_str)
                                .unwrap_or("{}"),
                        },
                    }));
                }
                _ => {}
            }
        }
    }

    let usage = payload.get("usage");
    let prompt = usage
        .and_then(|usage| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let completion = usage
        .and_then(|usage| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let total = usage
        .and_then(|usage| usage.get("total_tokens"))
        .and_then(Value::as_u64)
        .map(|total| total as u32)
        .unwrap_or_else(|| prompt + completion);

    let has_calls = !tool_calls.is_empty();
    let message = ChatMessage {
        role: "assistant".to_string(),
        content: if text.is_empty() {
            None
        } else {
            Some(Value::from(text))
        },
        reasoning_content: None,
        tool_calls: has_calls.then_some(Value::Array(tool_calls)),
        tool_call_id: None,
        name: None,
    };

    ChatCompletionResponse {
        id: format!("chatcmpl-{id}"),
        object: "chat.completion",
        created: ids::unix_seconds(),
        model: model.to_string(),
        choices: vec![ChatChoice {
            index: 0,
            message,
            finish_reason: if has_calls { "tool_calls" } else { "stop" }.to_string(),
        }],
        usage: Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: total,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "deepseek-v4-pro".to_string(),
            messages: vec![ChatMessage::plain("user".to_string(), "hello".to_string())],
            stream: false,
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn completes_via_responses_api() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "resp-123",
                "output": [{
                    "type": "message",
                    "content": [{"type": "output_text", "text": "PONG"}]
                }],
                "usage": {"input_tokens": 5, "output_tokens": 2, "total_tokens": 7}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let backend = ResponsesBackend::new(
            "deepseek".to_string(),
            format!("{}/responses", server.uri()),
            vec!["test-key".to_string()],
        )
        .unwrap();

        let response = backend.complete(request()).await.unwrap();
        assert_eq!(response.choices[0].message.text(), "PONG");
        assert_eq!(response.usage.total_tokens, 7);
    }
}
