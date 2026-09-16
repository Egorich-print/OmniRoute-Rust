//! Axum HTTP surface for the Rust gateway.
//!
//! Implements the OpenAI-compatible endpoints the rest of OmniRoute (and any
//! OpenAI SDK client) talks to. Streaming is emitted as SSE in Chat
//! Completions shape, so an `openai-responses` upstream just needs a
//! translation layer in front of [`backend::ChatBackend`] later.

pub mod auth;
pub mod backend;
pub mod combos;
pub mod grok_cli;
pub mod ids;
pub mod openai;
pub mod routing;

use std::convert::Infallible;
use std::sync::Arc;

use auth::Authenticated;
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
use omniroute_db::Db;
use omniroute_usage::{PriceTable, UsageRecord, record_usage};
use serde_json::{Value, json};

use backend::ChatBackend;

/// Shared gateway state.
#[derive(Clone)]
pub struct AppState {
    pub(crate) backend: Arc<dyn ChatBackend>,
    pub(crate) db: Option<Arc<Db>>,
    pub(crate) require_auth: bool,
    pub(crate) prices: PriceTable,
}

impl AppState {
    /// Build state around a backend, without persistence or auth.
    pub fn new(backend: Arc<dyn ChatBackend>) -> Self {
        Self {
            backend,
            db: None,
            require_auth: false,
            prices: PriceTable::with_defaults(),
        }
    }

    /// Build state backed by a database (enables auth + usage recording).
    pub fn with_db(backend: Arc<dyn ChatBackend>, db: Arc<Db>) -> Self {
        Self {
            backend,
            db: Some(db),
            require_auth: false,
            prices: PriceTable::with_defaults(),
        }
    }

    /// Force API-key auth even when the `api_keys` table is empty.
    pub fn with_require_auth(mut self, require: bool) -> Self {
        self.require_auth = require;
        self
    }
}

/// Build the router for a backend (no persistence, auth open).
pub fn build_router(backend: Arc<dyn ChatBackend>) -> Router {
    build_router_with_state(AppState::new(backend))
}

/// Build the router for full state. `/healthz` stays public; `/v1/*`
/// handlers each take the [`Authenticated`] extractor.
pub fn build_router_with_state(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .with_state(state)
}

async fn healthz(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({ "status": "ok", "backend": state.backend.name() }))
}

async fn list_models(_auth: Authenticated) -> impl IntoResponse {
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
    auth: Authenticated,
    Json(request): Json<ChatCompletionRequest>,
) -> Response {
    if request.stream {
        return stream_response(state, request).into_response();
    }

    let model = request.model.clone();
    let started = std::time::Instant::now();
    match state.backend.complete(request).await {
        Ok(response) => {
            let latency_ms = started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
            let usage = response.usage;
            record_completion_usage(&state, &auth, &model, &usage, true, latency_ms, None);

            let mut http = Json(response).into_response();
            if let Some(cost) = state.prices.cost_usd(
                &model,
                usage.prompt_tokens,
                usage.completion_tokens,
                0,
                0,
                0,
            ) && let Ok(value) = axum::http::HeaderValue::from_str(&format!("{cost:.6}"))
            {
                http.headers_mut().insert("x-omniroute-cost-usd", value);
            }
            http
        }
        Err(error) => {
            let latency_ms = started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
            record_completion_usage(
                &state,
                &auth,
                &model,
                &omniroute_core::Usage::default(),
                false,
                latency_ms,
                Some("upstream_error"),
            );
            error_response(error)
        }
    }
}
fn record_completion_usage(
    state: &AppState,
    auth: &Authenticated,
    model: &str,
    usage: &omniroute_core::Usage,
    success: bool,
    latency_ms: u32,
    error_code: Option<&str>,
) {
    let Some(db) = state.db.as_ref() else {
        return;
    };
    let guard = db.connection();
    let record = UsageRecord {
        provider: Some(state.backend.name().to_string()),
        model: Some(model.to_string()),
        connection_id: None,
        api_key_id: auth.key.as_ref().map(|key| key.id.clone()),
        api_key_name: auth.key.as_ref().map(|key| key.name.clone()),
        tokens_input: usage.prompt_tokens,
        tokens_output: usage.completion_tokens,
        tokens_cache_read: 0,
        tokens_cache_creation: 0,
        tokens_reasoning: 0,
        status: Some(if success { "succeeded" } else { "failed" }.to_string()),
        success,
        latency_ms,
        error_code: error_code.map(str::to_string),
    };
    if let Err(error) = record_usage(&guard, &record) {
        tracing::warn!("usage record failed: {error}");
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
    if let Some(tool_calls) = &chunk.tool_calls {
        delta.insert("tool_calls".into(), tool_calls.clone());
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

    fn open_state() -> (Router, Arc<omniroute_db::Db>) {
        let db = Arc::new(omniroute_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let router = build_router_with_state(AppState::with_db(
            Arc::new(backend::EchoBackend),
            db.clone(),
        ));
        (router, db)
    }

    fn keyed_state() -> (Router, Arc<omniroute_db::Db>) {
        let db = Arc::new(omniroute_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        {
            let guard = db.connection();
            omniroute_db::repos::insert_api_key(
                &guard,
                &omniroute_db::repos::ApiKeyRow {
                    id: "k1".to_string(),
                    name: "test".to_string(),
                    key: "sk-test-123".to_string(),
                    allowed_models: "[\"*\"]".to_string(),
                    no_log: false,
                },
            )
            .unwrap();
        }
        let router = build_router_with_state(AppState::with_db(
            Arc::new(backend::EchoBackend),
            db.clone(),
        ));
        (router, db)
    }

    fn completion_body(model: &str) -> String {
        json!({
            "model": model,
            "messages": [{"role": "user", "content": "hi"}]
        })
        .to_string()
    }

    async fn post_completions(router: Router, key: Option<&str>) -> Response {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json");
        if let Some(key) = key {
            builder = builder.header("authorization", format!("Bearer {key}"));
        }
        router
            .oneshot(
                builder
                    .body(Body::from(completion_body("grok-4.6")))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn auth_open_when_no_keys() {
        let (router, _) = open_state();
        let response = post_completions(router, None).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_blocks_anonymous_when_keys_exist() {
        let (router, _) = keyed_state();
        let response = post_completions(router, None).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_blocks_wrong_key() {
        let (router, _) = keyed_state();
        let response = post_completions(router, Some("sk-wrong")).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_allows_valid_key_records_usage_and_prices() {
        let (router, db) = keyed_state();
        let response = post_completions(router, Some("sk-test-123")).await;
        assert_eq!(response.status(), StatusCode::OK);

        let cost = response
            .headers()
            .get("x-omniroute-cost-usd")
            .expect("cost header")
            .to_str()
            .unwrap()
            .to_string();
        let cost_value: f64 = cost.parse().unwrap();
        assert!(cost_value > 0.0, "{cost}");

        let guard = db.connection();
        let (count, api_key_id): (i64, String) = guard
            .query_row(
                "SELECT COUNT(*), MAX(api_key_id) FROM usage_history WHERE model = 'grok-4.6'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(api_key_id, "k1");
    }

    #[tokio::test]
    async fn healthz_stays_public_with_keys() {
        let (router, _) = keyed_state();
        let response = router
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
}
