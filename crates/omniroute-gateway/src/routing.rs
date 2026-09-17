//! Routing backend: model-based selection with failover.
//!
//! Phase 4 slice. A [`RoutingBackend`] holds named backends, asks the
//! [`Router`] which to try for a model, skips tripped breakers, and falls
//! back on failure for non-streaming requests. Streaming uses the first
//! allowed backend (no mid-stream failover yet); per-attempt usage
//! attribution is a follow-up, so usage currently records `routing`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use futures::StreamExt;
use omniroute_core::{ChatCompletionRequest, ChatCompletionResponse, GatewayError};
use omniroute_db::Db;
use omniroute_providers::ProviderRegistry;
use omniroute_routing::{BreakerSet, Router};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::backend::{ChatBackend, ChunkStream};
use crate::combos::{load_combos, plan_steps};

/// Backend router with failover.
#[derive(Clone)]
pub struct RoutingBackend {
    backends: Arc<HashMap<String, Arc<dyn ChatBackend>>>,
    router: Arc<Router>,
    breakers: Arc<Mutex<BreakerSet>>,
    db: Option<Arc<Db>>,
    registry: Option<Arc<ProviderRegistry>>,
}

impl RoutingBackend {
    /// Build with default breaker settings (3 failures, 60s cooldown).
    pub fn new(backends: HashMap<String, Arc<dyn ChatBackend>>, router: Router) -> Self {
        Self::with_breaker_settings(backends, router, 3, 60)
    }

    /// Build with explicit breaker settings.
    pub fn with_breaker_settings(
        backends: HashMap<String, Arc<dyn ChatBackend>>,
        router: Router,
        threshold: u32,
        cooldown_secs: u64,
    ) -> Self {
        Self {
            backends: Arc::new(backends),
            router: Arc::new(router),
            breakers: Arc::new(Mutex::new(BreakerSet::new(threshold, cooldown_secs))),
            db: None,
            registry: None,
        }
    }

    /// Attach a database + registry for combo-name expansion.
    pub fn with_combo_source(mut self, db: Arc<Db>, registry: Arc<ProviderRegistry>) -> Self {
        self.db = Some(db);
        self.registry = Some(registry);
        self
    }

    fn lock_breakers(&self) -> std::sync::MutexGuard<'_, BreakerSet> {
        self.breakers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Ordered `(backend, model)` attempts for a requested model name.
    ///
    /// A combo name expands to its servable entries (with rewritten model
    /// ids); anything else falls back to prefix routing with the model
    /// unchanged. An empty plan means "nothing can serve this".
    fn plan(&self, model: &str) -> Vec<(String, String)> {
        if let (Some(db), Some(registry)) = (self.db.as_ref(), self.registry.as_ref()) {
            let guard = db.connection();
            let combos = load_combos(&guard);
            if let Some(combo) = combos.get(model) {
                let available: HashSet<String> = self.backends.keys().cloned().collect();
                let steps = plan_steps(combo, registry, &available);
                if !steps.is_empty() {
                    return steps;
                }
                // No servable entry: fall through to prefix routing so an
                // under-provisioned combo degrades like an unknown model.
            }
        }
        self.router
            .route(model)
            .into_iter()
            .map(|backend| (backend, model.to_string()))
            .collect()
    }
}

#[async_trait]
impl ChatBackend for RoutingBackend {
    fn name(&self) -> &'static str {
        "routing"
    }

    async fn complete(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, GatewayError> {
        let model = request.model.clone();
        let mut last_error: Option<GatewayError> = None;

        for (name, target_model) in self.plan(&model) {
            if !self.lock_breakers().allow(&name, Instant::now()) {
                continue;
            }
            let Some(backend) = self.backends.get(&name) else {
                continue;
            };
            let mut attempt = request.clone();
            attempt.model = target_model;
            match backend.complete(attempt).await {
                Ok(response) => {
                    self.lock_breakers().record_success(&name);
                    return Ok(response);
                }
                Err(error) => {
                    self.lock_breakers().record_failure(&name, Instant::now());
                    last_error = Some(error);
                }
            }
        }

        Err(last_error.unwrap_or(GatewayError::UnknownModel(model)))
    }

    fn stream(&self, request: ChatCompletionRequest) -> ChunkStream {
        let this = self.clone();
        let model = request.model.clone();
        let (tx, rx) = mpsc::channel(64);

        tokio::spawn(async move {
            let mut last_error: Option<GatewayError> = None;

            for (name, target_model) in this.plan(&model) {
                if !this.lock_breakers().allow(&name, Instant::now()) {
                    continue;
                }
                let Some(backend) = this.backends.get(&name).cloned() else {
                    continue;
                };
                let mut attempt = request.clone();
                attempt.model = target_model;
                let mut stream = backend.stream(attempt);

                // Pull the first item before committing to this backend: a
                // free tier answering 503 has emitted nothing, so the combo
                // can still hop to the next model instead of failing the
                // whole request.
                match stream.next().await {
                    Some(Ok(first)) => {
                        this.lock_breakers().record_success(&name);
                        if tx.send(Ok(first)).await.is_err() {
                            return; // client gone
                        }
                        while let Some(item) = stream.next().await {
                            if tx.send(item).await.is_err() {
                                return;
                            }
                        }
                        return; // stream finished normally
                    }
                    Some(Err(error)) => {
                        this.lock_breakers().record_failure(&name, Instant::now());
                        last_error = Some(error);
                        continue;
                    }
                    None => {
                        // Empty stream: acceptable, nothing to fall back to.
                        this.lock_breakers().record_success(&name);
                        return;
                    }
                }
            }

            let error = last_error.unwrap_or(GatewayError::UnknownModel(model));
            let _ = tx.send(Err(error)).await;
        });

        Box::pin(ReceiverStream::new(rx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::EchoBackend;
    use futures::StreamExt;
    use omniroute_core::ChatMessage;
    use omniroute_routing::RouteRule;

    #[derive(Debug, Clone)]
    struct FailBackend;
    #[async_trait]
    impl ChatBackend for FailBackend {
        fn name(&self) -> &'static str {
            "fail"
        }
        async fn complete(
            &self,
            _request: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, GatewayError> {
            Err(GatewayError::Upstream("boom".to_string()))
        }
        fn stream(&self, _request: ChatCompletionRequest) -> ChunkStream {
            Box::pin(futures::stream::once(async {
                Err(GatewayError::Upstream("boom".to_string()))
            }))
        }
    }

    fn request(model: &str) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: model.to_string(),
            messages: vec![ChatMessage::plain("user".to_string(), "hi".to_string())],
            stream: false,
            max_tokens: None,
            temperature: None,
            top_p: None,
            tools: None,
            tool_choice: None,
        }
    }

    fn routing() -> RoutingBackend {
        let mut backends: HashMap<String, std::sync::Arc<dyn ChatBackend>> = HashMap::new();
        backends.insert("fail".to_string(), std::sync::Arc::new(FailBackend));
        backends.insert("echo".to_string(), std::sync::Arc::new(EchoBackend));
        let router = Router::new(
            vec![RouteRule {
                model_prefix: "x-".to_string(),
                backends: vec!["fail".to_string(), "echo".to_string()],
            }],
            vec!["echo".to_string()],
        );
        RoutingBackend::with_breaker_settings(backends, router, 1, 3600)
    }

    #[tokio::test]
    async fn falls_back_on_primary_failure() {
        let routing = routing();
        let response = routing.complete(request("x-model")).await.unwrap();
        assert_eq!(response.choices[0].message.text(), "echo: hi");
    }

    #[tokio::test]
    async fn breaker_skips_tripped_primary() {
        let routing = routing();
        // First call trips "fail" (threshold 1) then serves via echo.
        routing.complete(request("x-model")).await.unwrap();
        // Second call must not touch "fail" at all (breaker open, long cooldown).
        let response = routing.complete(request("x-model")).await.unwrap();
        assert_eq!(response.choices[0].message.text(), "echo: hi");
    }

    #[tokio::test]
    async fn unknown_model_without_route_errors() {
        let mut backends: HashMap<String, std::sync::Arc<dyn ChatBackend>> = HashMap::new();
        backends.insert("echo".to_string(), std::sync::Arc::new(EchoBackend));
        let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]));
        let error = routing.complete(request("x-model")).await.unwrap_err();
        assert!(matches!(error, GatewayError::UnknownModel(_)));
    }

    #[tokio::test]
    async fn stream_uses_first_allowed_backend() {
        let routing = routing();
        let chunks: Vec<_> = routing
            .stream(request("other"))
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();
        let text: String = chunks.iter().filter_map(|c| c.content.clone()).collect();
        assert_eq!(text, "echo: hi");
    }

    #[tokio::test]
    async fn stream_fails_over_when_first_backend_errors() {
        let routing = routing();
        // The `x-` rule tries "fail" first; streaming must now hop to echo
        // instead of surfacing the error, which is what free tiers need.
        let chunks: Vec<_> = routing.stream(request("x-model")).collect::<Vec<_>>().await;
        let text: String = chunks
            .iter()
            .map(|item| item.as_ref().expect("no error after failover"))
            .filter_map(|chunk| chunk.content.clone())
            .collect();
        assert_eq!(text, "echo: hi");
    }

    #[tokio::test]
    async fn combo_name_expands_and_rewrites_model() {
        use omniroute_db::{Db, repos};
        use omniroute_providers::ProviderRegistry;

        let db = std::sync::Arc::new(Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        {
            let guard = db.connection();
            repos::upsert_combo(
                &guard,
                &repos::ComboRow {
                    id: "c1".to_string(),
                    name: "test-combo".to_string(),
                    data: serde_json::json!({
                        "strategy": "priority",
                        "models": [
                            {"kind": "model", "model": "grok-cli/grok-4.6", "providerId": "grok-cli"},
                        ],
                    })
                    .to_string(),
                    sort_order: 0,
                },
            )
            .unwrap();
        }

        let mut backends: HashMap<String, std::sync::Arc<dyn ChatBackend>> = HashMap::new();
        // Named like the real backend so the plan selects it; echo lets us
        // observe the rewritten model id in the response.
        backends.insert("grok-cli".to_string(), std::sync::Arc::new(EchoBackend));
        backends.insert("echo".to_string(), std::sync::Arc::new(EchoBackend));
        let routing = RoutingBackend::new(backends, Router::new(vec![], vec![]))
            .with_combo_source(db, std::sync::Arc::new(ProviderRegistry::load()));

        let response = routing.complete(request("test-combo")).await.unwrap();
        assert_eq!(response.model, "grok-4.6");
        assert_eq!(response.choices[0].message.text(), "echo: hi");
    }
}
