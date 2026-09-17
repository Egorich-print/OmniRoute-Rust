//! Gemini (`generateContent`) backend.
//!
//! Gemini differs from the OpenAI-compatible providers in two ways: the chat
//! URL embeds the model (`.../models/{model}:generateContent`) and auth is an
//! `x-goog-api-key` header. Requests and responses are translated with the
//! ported `omniroute-translate` pair, so tools and streaming work like the
//! OpenAI path.

use async_trait::async_trait;
use futures::StreamExt;
use omniroute_core::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError,
    StreamChunk, Usage,
};
use omniroute_http::{HttpClient, SseDecoder};
use omniroute_translate::gemini::{GeminiState, convert_event};
use omniroute_translate::gemini_request::openai_to_gemini;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::backend::{ChatBackend, ChunkStream, openai_request_body};
use crate::ids;

/// Gemini executor over HTTP.
#[derive(Debug, Clone)]
pub struct GeminiBackend {
    http: HttpClient,
    /// `.../v1beta/models`, without a trailing slash.
    base_url: String,
    tokens: Vec<String>,
    next: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl GeminiBackend {
    /// Build against the models base URL with a rotation pool of API keys.
    pub fn new(
        base_url: String,
        tokens: Vec<String>,
    ) -> Result<Self, omniroute_http::client::HttpError> {
        if tokens.is_empty() {
            return Err(omniroute_http::client::HttpError::Status {
                status: 0,
                body: "gemini: no credentials".to_string(),
            });
        }
        Ok(Self {
            http: HttpClient::new(600)?,
            base_url: base_url.trim_end_matches('/').to_string(),
            tokens,
            next: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
    }

    /// Pool size.
    pub fn token_count(&self) -> usize {
        self.tokens.len()
    }

    fn next_token(&self) -> &str {
        let index =
            self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % self.tokens.len();
        self.tokens[index].as_str()
    }

    fn method_url(&self, model: &str, stream: bool) -> String {
        if stream {
            format!("{}/{}:streamGenerateContent?alt=sse", self.base_url, model)
        } else {
            format!("{}/{}:generateContent", self.base_url, model)
        }
    }

    fn gemini_body(request: &ChatCompletionRequest) -> Value {
        openai_to_gemini(&openai_request_body(request))
    }

    async fn drive_stream(
        &self,
        request: ChatCompletionRequest,
        tx: mpsc::Sender<Result<StreamChunk, GatewayError>>,
    ) -> Result<(), GatewayError> {
        if request.messages.is_empty() {
            return Err(GatewayError::InvalidRequest("messages is empty".into()));
        }
        let body = Self::gemini_body(&request);
        let url = self.method_url(&request.model, true);
        let key = self.next_token().to_string();

        let response = self
            .http
            .post_json_with_headers(&url, None, &[("x-goog-api-key", &key)], &body)
            .await
            .map_err(|error| GatewayError::Upstream(error.to_string()))?;

        let mut bytes = response.bytes_stream();
        let mut decoder = SseDecoder::new();
        let mut translator = GeminiState::default();

        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(|error| GatewayError::Upstream(error.to_string()))?;
            let text = String::from_utf8_lossy(&chunk);
            for event in decoder.push(&text) {
                if event.data.trim() == "[DONE]" {
                    return Ok(());
                }
                let data: Value = match serde_json::from_str(&event.data) {
                    Ok(data) => data,
                    Err(_) => continue,
                };
                for chunk_json in convert_event(&mut translator, &data, ids::unix_seconds()) {
                    if let Some(stream_chunk) = openai_chunk_to_stream_chunk(&chunk_json)
                        && tx.send(Ok(stream_chunk)).await.is_err()
                    {
                        return Ok(());
                    }
                }
                if let Some(error) = translator.upstream_error.take() {
                    return Err(GatewayError::Upstream(error));
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl ChatBackend for GeminiBackend {
    fn name(&self) -> &'static str {
        "gemini"
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        if request.messages.is_empty() {
            return Err(GatewayError::InvalidRequest("messages is empty".into()));
        }
        let body = Self::gemini_body(&request);
        let url = self.method_url(&request.model, false);

        let mut last_error: Option<GatewayError> = None;
        for _ in 0..self.tokens.len() {
            let key = self.next_token().to_string();
            let response = match self
                .http
                .post_json_with_headers(&url, None, &[("x-goog-api-key", &key)], &body)
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
            return Ok(collapse_chunks(&request.model, &payload));
        }
        Err(last_error.unwrap_or_else(|| {
            GatewayError::Upstream("gemini: all credentials failed".to_string())
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

/// Collapse a full Gemini response into one OpenAI completion by running it
/// through the streaming translator, so both paths share the same mapping.
fn collapse_chunks(model: &str, payload: &Value) -> ChatCompletionResponse {
    let mut state = GeminiState::default();
    let chunks = convert_event(&mut state, payload, ids::unix_seconds());

    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut finish = "stop".to_string();

    for chunk in chunks {
        let choice = &chunk["choices"][0];
        let delta = &choice["delta"];
        if let Some(part) = delta.get("content").and_then(Value::as_str) {
            text.push_str(part);
        }
        if let Some(part) = delta.get("reasoning_content").and_then(Value::as_str) {
            reasoning.push_str(part);
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            // Streaming assigns per-delta indices; a single completion needs
            // one flat array, so push the call objects through.
            for call in calls {
                tool_calls.push(call.clone());
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            finish = reason.to_string();
        }
    }

    let usage = state.usage.unwrap_or_default();
    let message = ChatMessage {
        role: "assistant".to_string(),
        content: if text.is_empty() {
            None
        } else {
            Some(Value::from(text))
        },
        reasoning_content: if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        },
        tool_calls: (!tool_calls.is_empty()).then_some(Value::Array(tool_calls)),
        tool_call_id: None,
        name: None,
    };

    ChatCompletionResponse {
        id: format!("chatcmpl-{}", ids::unix_seconds()),
        object: "chat.completion",
        created: ids::unix_seconds(),
        model: model.to_string(),
        choices: vec![ChatChoice {
            index: 0,
            message,
            finish_reason: finish,
        }],
        usage: Usage {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
        },
    }
}

/// Reuse the shared OpenAI chunk → `StreamChunk` mapping.
use crate::backend::openai_chunk_to_stream_chunk;

/// Keep the `json!` import meaningful for tests and future fields.
#[allow(dead_code)]
fn _touch() -> Value {
    json!({})
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_partial_json, header, method, path},
    };

    fn request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gemini-3.8-flash".to_string(),
            messages: vec![ChatMessage::plain("user", "hi")],
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
    async fn completes_via_generate_content() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-3.8-flash:generateContent"))
            .and(header("x-goog-api-key", "k1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{
                    "content": {"parts": [{"text": "PONG"}]},
                    "finishReason": "STOP"
                }],
                "usageMetadata": {"promptTokenCount": 3, "candidatesTokenCount": 2, "totalTokenCount": 5}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let backend =
            GeminiBackend::new(format!("{}/models", server.uri()), vec!["k1".to_string()]).unwrap();
        let response = backend.complete(request()).await.unwrap();
        assert_eq!(response.choices[0].message.text(), "PONG");
        assert_eq!(response.choices[0].finish_reason, "stop");
        assert_eq!(response.usage.total_tokens, 5);
    }

    #[tokio::test]
    async fn forwards_json_response_format_to_generation_config() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-3.8-flash:generateContent"))
            .and(body_partial_json(json!({
                "generationConfig": {
                    "responseMimeType": "application/json",
                    "responseSchema": {"type": "object"}
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{
                    "content": {"parts": [{"text": "{}"}]},
                    "finishReason": "STOP"
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let backend =
            GeminiBackend::new(format!("{}/models", server.uri()), vec!["k1".to_string()]).unwrap();
        let mut request = request();
        request.response_format = Some(json!({
            "type": "json_schema",
            "json_schema": {"schema": {
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "additionalProperties": false
            }}
        }));

        let response = backend.complete(request).await.unwrap();
        assert_eq!(response.choices[0].message.text(), "{}");
    }

    #[tokio::test]
    async fn maps_function_call_to_tool_calls() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-3.8-flash:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{
                    "content": {"parts": [{"functionCall": {"name": "bash", "args": {"command": "ls"}}}]},
                    "finishReason": "STOP"
                }]
            })))
            .mount(&server)
            .await;

        let backend =
            GeminiBackend::new(format!("{}/models", server.uri()), vec!["k1".to_string()]).unwrap();
        let response = backend.complete(request()).await.unwrap();
        assert_eq!(response.choices[0].finish_reason, "tool_calls");
        let calls = response.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(calls[0]["function"]["name"], json!("bash"));
        assert_eq!(
            calls[0]["function"]["arguments"],
            json!("{\"command\":\"ls\"}")
        );
    }

    #[tokio::test]
    async fn nonstream_thinking_is_surfaced_as_reasoning_content() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-3.8-flash:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{
                    "content": {"parts": [
                        {"text": "weighing options", "thought": true},
                        {"text": "PONG"}
                    ]},
                    "finishReason": "STOP"
                }]
            })))
            .mount(&server)
            .await;

        let backend =
            GeminiBackend::new(format!("{}/models", server.uri()), vec!["k1".to_string()]).unwrap();
        let response = backend.complete(request()).await.unwrap();
        let message = &response.choices[0].message;
        assert_eq!(message.text(), "PONG");
        assert_eq!(
            message.reasoning_content.as_deref(),
            Some("weighing options")
        );
    }

    #[tokio::test]
    async fn streams_sse_chunks() {
        let server = MockServer::start().await;
        let sse = concat!(
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hi\"}],\"role\":\"model\"}}],\"modelVersion\":\"gemini-3.8-flash\",\"responseId\":\"r1\"}\n\n",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"!\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":1,\"candidatesTokenCount\":1,\"totalTokenCount\":2}}\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/models/gemini-3.8-flash:streamGenerateContent"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(sse)
                    .insert_header("content-type", "text/event-stream"),
            )
            .mount(&server)
            .await;

        let backend =
            GeminiBackend::new(format!("{}/models", server.uri()), vec!["k1".to_string()]).unwrap();
        let mut req = request();
        req.stream = true;
        let chunks: Vec<StreamChunk> = backend
            .stream(req)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();
        let text: String = chunks.iter().filter_map(|c| c.content.clone()).collect();
        assert_eq!(text, "Hi!");
        assert_eq!(
            chunks.last().unwrap().finish_reason.as_deref(),
            Some("stop")
        );
    }
}
