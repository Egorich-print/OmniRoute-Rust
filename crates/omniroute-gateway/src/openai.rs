//! Generic OpenAI-compatible backend (no translation).
//!
//! Serves the ~149 `default`-executor providers that speak OpenAI Chat
//! Completions natively: the request body goes through verbatim and SSE
//! frames map directly via the shared OpenAI chunk helper.

use async_trait::async_trait;
use futures::StreamExt;
use omniroute_core::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, GatewayError,
    StreamChunk, Usage,
};
use omniroute_http::{HttpClient, SseDecoder};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::backend::{ChatBackend, ChunkStream, openai_chunk_to_stream_chunk, openai_request_body};
use crate::ids;

/// OpenAI-compatible executor over HTTP.
#[derive(Debug, Clone)]
pub struct OpenAiBackend {
    http: HttpClient,
    base_url: String,
    api_key: Option<String>,
}

impl OpenAiBackend {
    /// Build against `base_url` (e.g. `https://api.openai.com/v1`) with an
    /// optional Bearer `api_key`.
    pub fn new(
        base_url: String,
        api_key: Option<String>,
    ) -> Result<Self, omniroute_http::client::HttpError> {
        Ok(Self {
            http: HttpClient::new(600)?,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
        })
    }

    fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    fn bearer(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    async fn drive_stream(
        &self,
        request: ChatCompletionRequest,
        tx: mpsc::Sender<Result<StreamChunk, GatewayError>>,
    ) -> Result<(), GatewayError> {
        if request.messages.is_empty() {
            return Err(GatewayError::InvalidRequest("messages is empty".into()));
        }
        let mut body = openai_request_body(&request);
        body["stream"] = Value::from(true);
        let response = self
            .http
            .post_json(&self.chat_url(), self.bearer(), &body)
            .await
            .map_err(|error| GatewayError::Upstream(error.to_string()))?;

        let mut bytes = response.bytes_stream();
        let mut decoder = SseDecoder::new();
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
                if data.get("error").is_some() {
                    let message = data
                        .get("error")
                        .and_then(|error| error.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("upstream error");
                    return Err(GatewayError::Upstream(message.to_string()));
                }
                if let Some(stream_chunk) = openai_chunk_to_stream_chunk(&data) {
                    if stream_chunk.finish_reason.is_some() {
                        finish_seen = true;
                    }
                    if tx.send(Ok(stream_chunk)).await.is_err() {
                        return Ok(());
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
impl ChatBackend for OpenAiBackend {
    fn name(&self) -> &'static str {
        "openai-compatible"
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        if request.messages.is_empty() {
            return Err(GatewayError::InvalidRequest("messages is empty".into()));
        }
        let mut body = openai_request_body(&request);
        body["stream"] = Value::from(false);
        let response = self
            .http
            .post_json(&self.chat_url(), self.bearer(), &body)
            .await
            .map_err(|error| GatewayError::Upstream(error.to_string()))?;
        let payload: Value = response
            .json()
            .await
            .map_err(|error| GatewayError::Upstream(error.to_string()))?;
        if let Some(message) = payload
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
        {
            return Err(GatewayError::Upstream(message.to_string()));
        }
        Ok(openai_payload_to_completion(&request.model, &payload))
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

/// Extract message text, accepting both string and part-array content.
fn message_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                if part.get("type").and_then(Value::as_str) == Some("text") {
                    part.get("text").and_then(Value::as_str)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Convert an OpenAI completion object to the typed response.
fn openai_payload_to_completion(model: &str, payload: &Value) -> ChatCompletionResponse {
    let id = payload
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("chatcmpl-unknown")
        .to_string();
    let choice = payload.get("choices").and_then(|choices| choices.get(0));
    let message = choice.and_then(|choice| choice.get("message"));
    let content = message.map(message_text).unwrap_or_default();
    let finish = choice
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(Value::as_str)
        .unwrap_or("stop")
        .to_string();
    let usage = payload.get("usage");
    let prompt = usage
        .and_then(|usage| usage.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let completion = usage
        .and_then(|usage| usage.get("completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let total = usage
        .and_then(|usage| usage.get("total_tokens"))
        .and_then(Value::as_u64)
        .map(|total| total as u32)
        .unwrap_or_else(|| prompt + completion);

    ChatCompletionResponse {
        id,
        object: "chat.completion",
        created: ids::unix_seconds(),
        model: model.to_string(),
        choices: vec![ChatChoice {
            index: 0,
            message: ChatMessage {
                role: "assistant".to_string(),
                content,
            },
            finish_reason: finish,
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
        matchers::{method, path},
    };

    fn request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "test-model".to_string(),
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
    async fn streams_openai_sse_to_finish() {
        let server = MockServer::start().await;
        let sse = concat!(
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(sse)
                    .insert_header("content-type", "text/event-stream"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let backend = OpenAiBackend::new(server.uri(), Some("k".to_string())).unwrap();
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
        assert_eq!(text, "Hello");
        assert_eq!(
            chunks.last().unwrap().finish_reason.as_deref(),
            Some("stop")
        );
    }

    #[tokio::test]
    async fn complete_returns_typed_completion() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-1",
                "object": "chat.completion",
                "created": 1,
                "model": "m",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "Hi there"},
                    "finish_reason": "stop",
                }],
                "usage": {"prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5},
            })))
            .expect(1)
            .mount(&server)
            .await;

        let backend = OpenAiBackend::new(server.uri(), None).unwrap();
        let mut req = request();
        req.stream = false;
        let response = backend.complete(req).await.unwrap();

        assert_eq!(response.choices[0].message.content, "Hi there");
        assert_eq!(response.usage.total_tokens, 5);
    }

    #[tokio::test]
    async fn upstream_error_surfaces() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": {"message": "bad key", "code": "invalid_api_key"}
            })))
            .mount(&server)
            .await;

        let backend = OpenAiBackend::new(server.uri(), None).unwrap();
        let mut req = request();
        req.stream = false;
        let error = backend.complete(req).await.unwrap_err();
        assert!(error.to_string().contains("bad key"));
    }
}
