//! Axum HTTP surface for the Rust gateway.
//!
//! Implements the OpenAI-compatible endpoints the rest of OmniRoute (and any
//! OpenAI SDK client) talks to. Streaming is emitted as SSE in Chat
//! Completions shape, so an `openai-responses` upstream just needs a
//! translation layer in front of [`backend::ChatBackend`] later.

pub mod backend;
pub mod ids;

use std::convert::Infallible;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures::StreamExt;
use omniroute_core::{ChatCompletionRequest, GatewayError, StreamChunk};
use serde_json::{Value, json};

use backend::ChatBackend;

/// Shared gateway state.
#[derive(Clone)]
pub struct AppState {
    backend: Arc<dyn ChatBackend>,
}

impl AppState {
    /// Build state around a backend.
    pub fn new(backend: Arc<dyn ChatBackend>) -> Self {
        Self { backend }
    }
}

/// Build the router for a backend.
pub fn build_router(backend: Arc<dyn ChatBackend>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .with_state(AppState::new(backend))
}

async fn healthz(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({ "status": "ok", "backend": state.backend.name() }))
}

async fn list_models() -> impl IntoResponse {
    Json(json!({
        "object": "list",
        "data": [
            { "id": "grok-4.6", "object": "model", "owned_by": "omniroute" },
            { "id": "grok-4.5", "object": "model", "owned_by": "omniroute" },
            { "id": "grok-composer-2.5-fast", "object": "model", "owned_by": "omniroute" }
        ]
    }))
}

async fn chat_completions(
    State(state): State<AppState>,
    Json(request): Json<ChatCompletionRequest>,
) -> Response {
    if request.stream {
        return stream_response(state, request).into_response();
    }

    match state.backend.complete(request).await {
        Ok(response) => Json(response).into_response(),
        Err(error) => error_response(error),
    }
}

fn stream_response(
    state: AppState,
    request: ChatCompletionRequest,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let id = ids::completion_id();
    let created = ids::unix_seconds();
    let model = request.model.clone();
    let stream = state.backend.stream(request);

    let events = stream.map(move |item| {
        Ok(match item {
            Ok(chunk) => chunk_event(&id, created, &model, &chunk),
            Err(error) => error_event(&error),
        })
    });

    let with_done = events.chain(futures::stream::once(async {
        Ok(Event::default().data("[DONE]"))
    }));

    Sse::new(with_done).keep_alive(KeepAlive::default())
}

fn chunk_event(id: &str, created: i64, model: &str, chunk: &StreamChunk) -> Event {
    let mut delta = serde_json::Map::new();
    if let Some(content) = &chunk.content {
        delta.insert("content".into(), json!(content));
    }
    if let Some(reasoning) = &chunk.reasoning {
        delta.insert("reasoning_content".into(), json!(reasoning));
    }

    let payload = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": Value::Object(delta),
            "finish_reason": chunk.finish_reason,
        }],
    });

    Event::default().data(payload.to_string())
}

fn error_event(error: &GatewayError) -> Event {
    Event::default().data(json!({ "error": { "message": error.to_string() } }).to_string())
}

fn error_response(error: GatewayError) -> Response {
    let status = match error {
        GatewayError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
        GatewayError::UnknownModel(_) => StatusCode::NOT_FOUND,
        GatewayError::Upstream(_) => StatusCode::BAD_GATEWAY,
    };
    (
        status,
        Json(json!({ "error": { "message": error.to_string() } })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_router() -> Router {
        build_router(Arc::new(backend::EchoBackend))
    }

    #[tokio::test]
    async fn healthz_reports_ok() {
        let response = test_router()
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn chat_completions_returns_echo() {
        let body = json!({
            "model": "grok-4.6",
            "messages": [{"role": "user", "content": "hello"}]
        })
        .to_string();

        let response = test_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            value["choices"][0]["message"]["content"],
            json!("echo: hello")
        );
    }

    #[tokio::test]
    async fn streaming_emits_content_then_done() {
        let body = json!({
            "model": "grok-4.6",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        })
        .to_string();

        let response = test_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.contains("echo:"));
        assert!(text.contains("\"finish_reason\":\"stop\""));
        assert!(text.contains("[DONE]"));
    }
}
