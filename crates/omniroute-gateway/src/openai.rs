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
    // Preserve the upstream assistant turn verbatim: tool_calls, null
    // content and the real finish_reason must survive, or agents never see
    // their function calls.
    let content = message
        .and_then(|message| message.get("content"))
        .cloned()
        .or_else(|| {
            message
                .map(message_text)
                .filter(|text| !text.is_empty())
                .map(Value::from)
        });
    let tool_calls = message
        .and_then(|message| message.get("tool_calls"))
        .cloned();
    let finish = choice
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            if tool_calls.is_some() {
                "tool_calls".to_string()
            } else {
                "stop".to_string()
            }
        });
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
                reasoning_content: message
                    .and_then(|message| message.get("reasoning_content"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                tool_calls,
                tool_call_id: None,
                name: None,
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
            messages: vec![ChatMessage::plain("user".to_string(), "Hi".to_string())],
            stream: true,
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
            ..Default::default()
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

        assert_eq!(response.choices[0].message.text(), "Hi there");
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

/// Resolve a full chat-completions URL from a registry `baseUrl`.
///
/// Registry entries vary: some declare the endpoint (`.../chat/completions`),
/// some only the API root. Append the path only when it is missing.
pub fn resolve_chat_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/chat/completions") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/chat/completions")
    }
}

/// Per-provider OpenAI-compatible backend.
///
/// Generalises [`OpenAiBackend`] over the registry: the URL comes from the
/// provider entry, credentials from `provider_connections`, and a pool is
/// rotated so one exhausted key does not fail the request.
#[derive(Debug, Clone)]
pub struct ProviderOpenAiBackend {
    http: HttpClient,
    provider: String,
    chat_url: String,
    tokens: Vec<String>,
    next: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl ProviderOpenAiBackend {
    /// Build for `provider` against `chat_url` with a credential pool.
    pub fn new(
        provider: String,
        chat_url: String,
        tokens: Vec<String>,
    ) -> Result<Self, omniroute_http::client::HttpError> {
        if tokens.is_empty() {
            return Err(omniroute_http::client::HttpError::Status {
                status: 0,
                body: format!("{provider}: no credentials"),
            });
        }
        Ok(Self {
            http: HttpClient::new(600)?,
            provider,
            chat_url,
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
            .post_json(&self.chat_url, Some(self.next_token()), &body)
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
                if let Some(error) = data.get("error") {
                    let message = error
                        .get("message")
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
impl ChatBackend for ProviderOpenAiBackend {
    fn name(&self) -> &'static str {
        // Static str required by the trait; the pool/URL carry the provider.
        "provider"
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

        let mut last_error: Option<GatewayError> = None;
        for _ in 0..self.tokens.len() {
            let response = match self
                .http
                .post_json(&self.chat_url, Some(self.next_token()), &body)
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
            return Ok(openai_payload_to_completion(&request.model, &payload));
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

#[cfg(test)]
mod provider_tests {
    use super::*;
    use omniroute_core::{ChatCompletionRequest, ChatMessage};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    #[test]
    fn chat_url_resolution_handles_both_shapes() {
        assert_eq!(
            resolve_chat_url("https://openrouter.ai/api/v1"),
            "https://openrouter.ai/api/v1/chat/completions"
        );
        assert_eq!(
            resolve_chat_url("https://openrouter.ai/api/v1/chat/completions"),
            "https://openrouter.ai/api/v1/chat/completions"
        );
    }

    #[tokio::test]
    async fn provider_backend_serves_completion() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "gen-1",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "hi"},
                    "finish_reason": "stop",
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
            })))
            .expect(1)
            .mount(&server)
            .await;

        let backend = ProviderOpenAiBackend::new(
            "openrouter".to_string(),
            resolve_chat_url(&format!("{}/v1", server.uri())),
            vec!["k".to_string()],
        )
        .unwrap();

        let response = backend
            .complete(ChatCompletionRequest {
                model: "some/model:free".to_string(),
                messages: vec![ChatMessage::plain("user".to_string(), "hi".to_string())],
                stream: false,
                max_tokens: None,
                temperature: None,
                top_p: None,
                tools: None,
                tool_choice: None,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(response.choices[0].message.text(), "hi");
    }
}

#[cfg(test)]
mod tool_passthrough_tests {
    use super::*;
    use omniroute_core::{ChatCompletionRequest, ChatMessage};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_string_contains, method, path},
    };

    #[test]
    fn completion_preserves_tool_calls_and_finish_reason() {
        let payload = serde_json::json!({
            "id": "c1",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "get_time", "arguments": "{\"city\":\"SF\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        });
        let response = openai_payload_to_completion("m", &payload);
        assert_eq!(response.choices[0].finish_reason, "tool_calls");
        let message = &response.choices[0].message;
        assert!(message.has_tool_calls());
        assert_eq!(message.text(), "");
    }

    #[tokio::test]
    async fn tool_definitions_reach_the_upstream() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_string_contains("\"get_time\""))
            .and(body_string_contains("\"tool_calls\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "c1",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "ok"},
                    "finish_reason": "stop",
                }],
            })))
            .expect(1)
            .mount(&server)
            .await;

        let backend = ProviderOpenAiBackend::new(
            "test".to_string(),
            resolve_chat_url(&format!("{}/v1", server.uri())),
            vec!["k".to_string()],
        )
        .unwrap();

        let request = ChatCompletionRequest {
            model: "m".to_string(),
            messages: vec![
                ChatMessage::plain("user", "time?"),
                ChatMessage {
                    role: "assistant".to_string(),
                    content: None,
                    reasoning_content: None,
                    tool_calls: Some(serde_json::json!([{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "get_time", "arguments": "{}"}
                    }])),
                    tool_call_id: None,
                    name: None,
                },
                ChatMessage {
                    role: "tool".to_string(),
                    content: Some(serde_json::Value::from("12:00")),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: Some("call_1".to_string()),
                    name: None,
                },
            ],
            stream: false,
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: Some(serde_json::json!([{
                "type": "function",
                "function": {"name": "get_time", "parameters": {"type": "object"}}
            }])),
            tool_choice: Some(serde_json::json!("auto")),
            ..Default::default()
        };

        let response = backend.complete(request).await.unwrap();
        assert_eq!(response.choices[0].message.text(), "ok");
    }
}
