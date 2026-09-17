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
    signatures: std::sync::Arc<ThoughtSignatures>,
}

/// Tool-call id → Gemini thought signature, kept across a tool round trip.
///
/// The signature leaves in `extra_content` for clients that preserve unknown
/// fields. Clients that rebuild the assistant message from their own schema
/// drop it, and Gemini then rejects the replayed `functionCall`, so the id we
/// minted is used as a fallback key.
///
/// The store is persisted: an agent conversation outlives a gateway restart,
/// and a signature lost that way turns the next turn into a hard 400.
/// Bounded: the oldest entries fall out first.
#[derive(Debug, Default)]
pub struct ThoughtSignatures {
    path: Option<std::path::PathBuf>,
    state: std::sync::Mutex<SignatureState>,
}

#[derive(Debug, Default)]
struct SignatureState {
    order: std::collections::VecDeque<String>,
    values: std::collections::HashMap<String, String>,
}

/// One persisted `id` → `signature` pair.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SignatureRecord {
    id: String,
    signature: String,
}

/// Signatures kept before the oldest are evicted.
const SIGNATURE_CAPACITY: usize = 4096;

impl ThoughtSignatures {
    /// Open a persistent store, reusing signatures recorded before a restart.
    pub fn open(path: std::path::PathBuf) -> Self {
        let store = Self {
            path: Some(path),
            state: std::sync::Mutex::new(SignatureState::default()),
        };
        store.load();
        store
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SignatureState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Read the persisted pairs back, then rewrite the file trimmed so it does
    /// not grow without bound across restarts.
    fn load(&self) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let Ok(content) = std::fs::read_to_string(path) else {
            return;
        };
        {
            let mut state = self.lock();
            for line in content.lines() {
                let Ok(record) = serde_json::from_str::<SignatureRecord>(line) else {
                    continue;
                };
                if record.id.is_empty() || record.signature.is_empty() {
                    continue;
                }
                if state
                    .values
                    .insert(record.id.clone(), record.signature)
                    .is_none()
                {
                    state.order.push_back(record.id);
                }
            }
            while state.order.len() > SIGNATURE_CAPACITY {
                if let Some(oldest) = state.order.pop_front() {
                    state.values.remove(&oldest);
                }
            }
        }
        self.rewrite();
    }

    /// Replace the file with the in-memory contents (used once at load).
    fn rewrite(&self) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let state = self.lock();
        let mut body = String::new();
        for id in &state.order {
            let Some(signature) = state.values.get(id) else {
                continue;
            };
            if let Ok(line) = serde_json::to_string(&SignatureRecord {
                id: id.clone(),
                signature: signature.clone(),
            }) {
                body.push_str(&line);
                body.push('\n');
            }
        }
        let _ = std::fs::write(path, body);
    }

    fn record(&self, id: &str, signature: &str) {
        {
            let mut state = self.lock();
            if state
                .values
                .insert(id.to_string(), signature.to_string())
                .is_none()
            {
                state.order.push_back(id.to_string());
            }
            while state.order.len() > SIGNATURE_CAPACITY {
                if let Some(oldest) = state.order.pop_front() {
                    state.values.remove(&oldest);
                }
            }
        }
        self.append(id, signature);
    }

    /// Append one pair; a failed write only costs the fallback for that call.
    fn append(&self, id: &str, signature: &str) {
        use std::io::Write;
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let Ok(line) = serde_json::to_string(&SignatureRecord {
            id: id.to_string(),
            signature: signature.to_string(),
        }) else {
            return;
        };
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{line}");
        }
    }

    fn get(&self, id: &str) -> Option<String> {
        self.lock().values.get(id).cloned()
    }
}

/// Remember the signatures carried by translated tool calls.
fn record_signatures(chunk: &Value, signatures: &ThoughtSignatures) {
    let calls = chunk["choices"][0]["delta"]["tool_calls"]
        .as_array()
        .into_iter()
        .flatten();
    for call in calls {
        let (Some(id), Some(signature)) = (
            call.get("id").and_then(Value::as_str),
            call.pointer("/extra_content/google/thought_signature")
                .and_then(Value::as_str),
        ) else {
            continue;
        };
        signatures.record(id, signature);
    }
}

/// Fill in signatures a client dropped from replayed tool calls.
fn inject_signatures(request: &mut ChatCompletionRequest, signatures: &ThoughtSignatures) {
    for message in &mut request.messages {
        if message.role != "assistant" {
            continue;
        }
        let Some(calls) = message.tool_calls.as_mut().and_then(Value::as_array_mut) else {
            continue;
        };
        for call in calls {
            let carried = call
                .pointer("/extra_content/google/thought_signature")
                .or_else(|| call.get("thoughtSignature"))
                .is_some();
            let Some(id) = call.get("id").and_then(Value::as_str) else {
                continue;
            };
            if carried {
                continue;
            }
            let Some(signature) = signatures.get(id) else {
                continue;
            };
            if let Some(object) = call.as_object_mut() {
                object.insert(
                    "extra_content".to_string(),
                    json!({ "google": { "thought_signature": signature } }),
                );
            }
        }
    }
}

impl GeminiBackend {
    /// Build against the models base URL with a rotation pool of API keys.
    pub fn new(
        base_url: String,
        tokens: Vec<String>,
    ) -> Result<Self, omniroute_http::client::HttpError> {
        Self::with_signature_store(
            base_url,
            tokens,
            std::sync::Arc::new(ThoughtSignatures::default()),
        )
    }

    /// Build with a persistent thought-signature store.
    ///
    /// Agents span longer than a process: a signature dropped by a restart
    /// makes Gemini reject the next replayed function call.
    pub fn with_signature_store(
        base_url: String,
        tokens: Vec<String>,
        signatures: std::sync::Arc<ThoughtSignatures>,
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
            signatures,
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
        mut request: ChatCompletionRequest,
        tx: mpsc::Sender<Result<StreamChunk, GatewayError>>,
    ) -> Result<(), GatewayError> {
        if request.messages.is_empty() {
            return Err(GatewayError::InvalidRequest("messages is empty".into()));
        }
        inject_signatures(&mut request, &self.signatures);
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
                    record_signatures(&chunk_json, &self.signatures);
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
        mut request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        if request.messages.is_empty() {
            return Err(GatewayError::InvalidRequest("messages is empty".into()));
        }
        inject_signatures(&mut request, &self.signatures);
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
            return Ok(collapse_chunks(&request.model, &payload, &self.signatures));
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
fn collapse_chunks(
    model: &str,
    payload: &Value,
    signatures: &ThoughtSignatures,
) -> ChatCompletionResponse {
    let mut state = GeminiState::default();
    let chunks = convert_event(&mut state, payload, ids::unix_seconds());
    for chunk in &chunks {
        record_signatures(chunk, signatures);
    }

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

    #[test]
    fn thought_signatures_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("omni-sig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("signatures.jsonl");

        let first = ThoughtSignatures::open(path.clone());
        first.record("chatcmpl-1-0", "c2ln");
        drop(first);

        // A new process opens the same file: the conversation keeps working.
        let second = ThoughtSignatures::open(path.clone());
        assert_eq!(second.get("chatcmpl-1-0").as_deref(), Some("c2ln"));

        // Loading trims the file back to the retained pairs.
        let reloaded = ThoughtSignatures::open(path.clone());
        assert_eq!(reloaded.get("chatcmpl-1-0").as_deref(), Some("c2ln"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn replays_thought_signature_the_client_dropped() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/models/gemini-3.8-flash:generateContent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "candidates": [{
                    "content": {"parts": [{
                        "functionCall": {"name": "bash", "args": {"command": "ls"}},
                        "thoughtSignature": "c2ln"
                    }]},
                    "finishReason": "STOP"
                }]
            })))
            .mount(&server)
            .await;

        let backend =
            GeminiBackend::new(format!("{}/models", server.uri()), vec!["k1".to_string()]).unwrap();

        let first = backend.complete(request()).await.unwrap();
        let call = &first.choices[0].message.tool_calls.as_ref().unwrap()[0];
        assert_eq!(call["extra_content"]["google"]["thought_signature"], "c2ln");

        // The client rebuilds the assistant turn from its own schema, so the
        // extension field is gone by the time the tool result comes back.
        let mut follow_up = request();
        follow_up.messages = vec![
            ChatMessage::plain("user", "list files"),
            ChatMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(json!([{
                    "id": call["id"],
                    "type": "function",
                    "function": {"name": "bash", "arguments": "{\"command\":\"ls\"}"}
                }])),
                tool_call_id: None,
                name: None,
            },
            ChatMessage {
                role: "tool".to_string(),
                content: Some(Value::from("a.txt")),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: call["id"].as_str().map(str::to_string),
                name: None,
            },
        ];
        backend.complete(follow_up).await.unwrap();

        let sent = server.received_requests().await.unwrap();
        let body = sent.last().unwrap().body_json::<Value>().unwrap();
        let model_turn = body["contents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|turn| turn["role"] == "model")
            .expect("model turn replayed");
        assert_eq!(model_turn["parts"][0]["thoughtSignature"], "c2ln");
        assert_eq!(model_turn["parts"][0]["functionCall"]["name"], "bash");
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
