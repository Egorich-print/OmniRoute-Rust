//! Grok Build (`grok-cli`) backend: real HTTP executor.
//!
//! First Phase 3a executor. It speaks the upstream `/v1/responses` endpoint
//! and translates both directions with the ported `omniroute-translate`
//! pair, so an OpenAI-shaped client request is served end to end:
//!
//! ```text
//! chat.request --openai_to_responses--> POST /responses --SSE/response-->
//!   --convert_event--> StreamChunk --gateway SSE--> chat.chunk
//! ```

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
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::backend::{ChatBackend, ChunkStream, openai_chunk_to_stream_chunk, openai_request_body};
use crate::ids;

/// Grok Build executor over HTTP.
#[derive(Debug, Clone)]
pub struct GrokCliBackend {
    http: HttpClient,
    base_url: String,
    token: String,
}

impl GrokCliBackend {
    /// Build against `base_url` (e.g. `https://cli-chat-proxy.grok.com/v1`)
    /// with a Bearer `token`.
    pub fn new(base_url: String, token: String) -> Result<Self, omniroute_http::client::HttpError> {
        Ok(Self {
            http: HttpClient::new(600)?,
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
        })
    }

    fn responses_url(&self) -> String {
        format!("{}/responses", self.base_url)
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
            .post_json(&self.responses_url(), Some(&self.token), &upstream)
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
impl ChatBackend for GrokCliBackend {
    fn name(&self) -> &'static str {
        "grok-cli"
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
        let response = self
            .http
            .post_json(&self.responses_url(), Some(&self.token), &upstream)
            .await
            .map_err(|error| GatewayError::Upstream(error.to_string()))?;
        let payload: Value = response
            .json()
            .await
            .map_err(|error| GatewayError::Upstream(error.to_string()))?;
        // The upstream may answer errors as JSON with an `error` member even
        // on 200; surface those instead of an empty completion.
        if let Some(message) = payload
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
        {
            return Err(GatewayError::Upstream(message.to_string()));
        }
        Ok(response_to_completion(&request.model, &payload))
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
    if let Some(output) = payload.get("output").and_then(Value::as_array) {
        for item in output {
            if item.get("type").and_then(Value::as_str) != Some("message") {
                continue;
            }
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

    ChatCompletionResponse {
        id: format!("chatcmpl-{id}"),
        object: "chat.completion",
        created: ids::unix_seconds(),
        model: model.to_string(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage {
                role: "assistant".to_string(),
                content: text,
            },
            finish_reason: "stop".to_string(),
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
    use futures::StreamExt;
    use omniroute_core::{ChatCompletionRequest, ChatMessage};
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_string_contains, method, path},
    };

    fn request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "grok-4.6".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: "Hi".to_string(),
            }],
            stream: true,
            max_tokens: None,
            temperature: None,
        }
    }

    #[tokio::test]
    async fn streams_translated_deltas_to_finish() {
        let server = MockServer::start().await;
        let sse = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello\"}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"!\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":3,\"output_tokens\":2,\"total_tokens\":5}}}\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(body_string_contains("\"input_text\""))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(sse)
                    .insert_header("content-type", "text/event-stream"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let backend = GrokCliBackend::new(server.uri(), "tok".to_string()).unwrap();
        let chunks: Vec<StreamChunk> = backend
            .stream(request())
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();

        let text: String = chunks
            .iter()
            .filter_map(|chunk| chunk.content.clone())
            .collect();
        assert_eq!(text, "Hello!");
        assert_eq!(
            chunks.last().unwrap().finish_reason.as_deref(),
            Some("stop")
        );
    }

    #[tokio::test]
    async fn complete_converts_response_object() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "resp_1",
                "model": "grok-4.6",
                "output": [
                    {"type": "message", "content": [{"type": "output_text", "text": "Hi there"}]}
                ],
                "usage": {"input_tokens": 4, "output_tokens": 2, "total_tokens": 6},
            })))
            .expect(1)
            .mount(&server)
            .await;

        let backend = GrokCliBackend::new(server.uri(), "tok".to_string()).unwrap();
        let mut req = request();
        req.stream = false;
        let response = backend.complete(req).await.unwrap();

        assert_eq!(response.choices[0].message.content, "Hi there");
        assert_eq!(response.usage.prompt_tokens, 4);
        assert_eq!(response.usage.total_tokens, 6);
        assert_eq!(response.choices[0].finish_reason, "stop");
    }

    #[tokio::test]
    async fn upstream_status_error_surfaces() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(429).set_body_json(json!({
                "error": {"message": "slow down", "code": "rate_limit"}
            })))
            .mount(&server)
            .await;

        let backend = GrokCliBackend::new(server.uri(), "tok".to_string()).unwrap();
        let mut req = request();
        req.stream = false;
        let error = backend.complete(req).await.unwrap_err();
        assert!(matches!(error, GatewayError::Upstream(_)));
        assert!(error.to_string().contains("slow down"));
    }

    #[tokio::test]
    async fn failed_event_errors_the_stream() {
        let server = MockServer::start().await;
        let sse = concat!(
            "event: response.failed\n",
            "data: {\"type\":\"response.failed\",\"error\":{\"message\":\"boom\"}}\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(sse)
                    .insert_header("content-type", "text/event-stream"),
            )
            .mount(&server)
            .await;

        let backend = GrokCliBackend::new(server.uri(), "tok".to_string()).unwrap();
        let results: Vec<_> = backend.stream(request()).collect::<Vec<_>>().await;
        assert_eq!(results.len(), 1);
        let error = results.into_iter().next().unwrap().unwrap_err();
        assert!(error.to_string().contains("boom"));
    }
}
