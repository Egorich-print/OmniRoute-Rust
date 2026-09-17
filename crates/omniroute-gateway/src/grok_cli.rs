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
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::backend::{ChatBackend, ChunkStream, openai_chunk_to_stream_chunk, openai_request_body};
use crate::ids;

/// Pinned Grok client version (mirrors `GROK_BUILD_DEFAULT_CLIENT_VERSION`).
/// The proxy rejects missing/outdated versions with 426, so this must track
/// the JS constant.
const GROK_CLIENT_VERSION: &str = "0.2.106";
const GROK_CLIENT_IDENTIFIER: &str = "grok-shell";

fn grok_user_agent() -> String {
    format!(
        "{GROK_CLIENT_IDENTIFIER}/{GROK_CLIENT_VERSION} ({}; {})",
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

/// Session headers the Grok proxy requires (mirrors
/// `getGrokBuildSessionHeaders`). Without `x-grok-client-version` the
/// upstream answers 426.
fn session_headers<'a>(
    model: &'a str,
    stream: bool,
    user_agent: &'a str,
) -> Vec<(&'static str, &'a str)> {
    vec![
        (
            "Accept",
            if stream {
                "text/event-stream"
            } else {
                "application/json"
            },
        ),
        ("x-grok-client-version", GROK_CLIENT_VERSION),
        ("x-grok-client-identifier", GROK_CLIENT_IDENTIFIER),
        ("x-grok-client-mode", "headless"),
        ("User-Agent", user_agent),
        ("X-XAI-Token-Auth", "xai-grok-cli"),
        ("x-authenticateresponse", "authenticate-response"),
        ("x-grok-model-override", model),
    ]
}

/// Grok Build executor over HTTP.
#[derive(Debug, Clone)]
pub struct GrokCliBackend {
    http: HttpClient,
    base_url: String,
    tokens: Vec<String>,
    next: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl GrokCliBackend {
    /// Build against `base_url` (e.g. `https://cli-chat-proxy.grok.com/v1`)
    /// with a Bearer `token`.
    pub fn new(base_url: String, token: String) -> Result<Self, omniroute_http::client::HttpError> {
        Self::with_tokens(base_url, vec![token])
    }

    /// Build with a rotation pool of tokens (ordered by priority). An empty
    /// pool yields an error: a credential-less executor cannot serve.
    pub fn with_tokens(
        base_url: String,
        tokens: Vec<String>,
    ) -> Result<Self, omniroute_http::client::HttpError> {
        if tokens.is_empty() {
            return Err(omniroute_http::client::HttpError::Status {
                status: 0,
                body: "grok-cli: no credentials".to_string(),
            });
        }
        Ok(Self {
            http: HttpClient::new(600)?,
            base_url: base_url.trim_end_matches('/').to_string(),
            tokens,
            next: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
    }

    /// Pool size (number of rotated credentials).
    pub fn token_count(&self) -> usize {
        self.tokens.len()
    }

    /// Round-robin token selection.
    fn next_token(&self) -> &str {
        let index =
            self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % self.tokens.len();
        self.tokens[index].as_str()
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
        let user_agent = grok_user_agent();
        let headers = session_headers(&request.model, true, &user_agent);
        let response = self
            .http
            .post_json_with_headers(
                &self.responses_url(),
                Some(self.next_token()),
                &headers,
                &upstream,
            )
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
        let user_agent = grok_user_agent();
        let headers = session_headers(&request.model, false, &user_agent);

        // Try each pooled credential once; a dead/exhausted token should not
        // fail the request while others are available.
        let mut last_error: Option<GatewayError> = None;
        for _ in 0..self.tokens.len() {
            let response = match self
                .http
                .post_json_with_headers(
                    &self.responses_url(),
                    Some(self.next_token()),
                    &headers,
                    &upstream,
                )
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
            // The upstream may answer errors as JSON with an `error` member
            // even on 200; surface those instead of an empty completion.
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
            GatewayError::Upstream("grok-cli: all credentials failed".to_string())
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
                // A Responses `function_call` item maps to an OpenAI tool call.
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
    use futures::StreamExt;
    use omniroute_core::{ChatCompletionRequest, ChatMessage};
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_string_contains, header, method, path},
    };

    fn request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "grok-4.6".to_string(),
            messages: vec![ChatMessage::plain("user".to_string(), "Hi".to_string())],
            stream: true,
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
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
            // The proxy rejects missing/outdated client versions with 426.
            .and(header("x-grok-client-version", "0.2.106"))
            .and(header("x-grok-client-identifier", "grok-shell"))
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

        assert_eq!(response.choices[0].message.text(), "Hi there");
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

#[cfg(test)]
mod tool_tests {
    use super::*;
    use futures::StreamExt;
    use omniroute_core::{ChatCompletionRequest, ChatMessage};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    #[tokio::test]
    async fn streams_incremental_tool_call_arguments() {
        let server = MockServer::start().await;
        let sse = concat!(
            "event: response.output_item.added\n",
            "data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"item_1\",\"call_id\":\"call_1\",\"name\":\"get_time\"}}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"{\\\"city\\\":\"}\n\n",
            "event: response.function_call_arguments.delta\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":0,\"delta\":\"\\\"SF\\\"}\"}\n\n",
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"item_1\",\"call_id\":\"call_1\",\"name\":\"get_time\",\"arguments\":\"\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
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
        let request = ChatCompletionRequest {
            model: "grok-4.6".to_string(),
            messages: vec![ChatMessage::plain("user".to_string(), "time?".to_string())],
            stream: true,
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
        };

        let chunks: Vec<StreamChunk> = backend
            .stream(request)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();

        let calls: Vec<&serde_json::Value> = chunks
            .iter()
            .filter_map(|c| c.tool_calls.as_ref())
            .collect();
        assert_eq!(calls.len(), 2, "announce + arguments");
        assert_eq!(
            calls[0][0]["function"]["name"],
            serde_json::json!("get_time")
        );
        assert_eq!(
            calls[1][0]["function"]["arguments"],
            serde_json::json!("{\"city\":\"SF\"}")
        );
        assert_eq!(
            chunks.last().unwrap().finish_reason.as_deref(),
            Some("tool_calls")
        );
    }
}

#[cfg(test)]
mod rotation_tests {
    use super::*;
    use omniroute_core::{ChatCompletionRequest, ChatMessage};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };

    #[tokio::test]
    async fn rotates_past_a_rejected_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("authorization", "Bearer bad"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_json(serde_json::json!({"error": {"message": "invalid token"}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("authorization", "Bearer good"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({
                    "id": "resp_ok",
                    "output": [{"type": "message", "content": [{"type": "output_text", "text": "rotated"}]}],
                    "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2},
                }),
            ))
            .expect(1)
            .mount(&server)
            .await;

        let backend =
            GrokCliBackend::with_tokens(server.uri(), vec!["bad".to_string(), "good".to_string()])
                .unwrap();
        assert_eq!(backend.token_count(), 2);

        let request = ChatCompletionRequest {
            model: "grok-4.6".to_string(),
            messages: vec![ChatMessage::plain("user".to_string(), "hi".to_string())],
            stream: false,
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
        };
        let response = backend.complete(request).await.unwrap();
        assert_eq!(response.choices[0].message.text(), "rotated");
    }

    #[test]
    fn empty_pool_is_rejected() {
        assert!(GrokCliBackend::with_tokens("http://x".to_string(), vec![]).is_err());
    }
}

#[cfg(test)]
mod tool_completion_tests {
    use super::*;
    use omniroute_core::{ChatCompletionRequest, ChatMessage};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    #[tokio::test]
    async fn nonstream_function_call_becomes_tool_calls() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "resp_1",
                "output": [
                    {"type": "function_call", "call_id": "call_1", "name": "get_time",
                     "arguments": "{\"city\":\"SF\"}"}
                ],
                "usage": {"input_tokens": 3, "output_tokens": 2, "total_tokens": 5},
            })))
            .mount(&server)
            .await;

        let backend = GrokCliBackend::new(server.uri(), "tok".to_string()).unwrap();
        let request = ChatCompletionRequest {
            model: "grok-4.6".to_string(),
            messages: vec![ChatMessage::plain("user", "time?")],
            stream: false,
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: Some(serde_json::json!([{
                "type": "function",
                "function": {"name": "get_time", "parameters": {"type": "object"}}
            }])),
            tool_choice: None,
        };
        let response = backend.complete(request).await.unwrap();
        assert_eq!(response.choices[0].finish_reason, "tool_calls");
        let calls = response.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(calls[0]["function"]["name"], serde_json::json!("get_time"));
        assert_eq!(
            calls[0]["function"]["arguments"],
            serde_json::json!("{\"city\":\"SF\"}")
        );
    }
}
