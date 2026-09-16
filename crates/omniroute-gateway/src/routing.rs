//! Routing backend: model-based selection with failover.
//!
//! Phase 4 slice. A [`RoutingBackend`] holds named backends, asks the
//! [`Router`] which to try for a model, skips tripped breakers, and falls
//! back on failure for non-streaming requests. Streaming uses the first
//! allowed backend (no mid-stream failover yet); per-attempt usage
//! attribution is a follow-up, so usage currently records `routing`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use async_trait::async_trait;
use omniroute_core::{ChatCompletionRequest, ChatCompletionResponse, GatewayError};
use omniroute_routing::{BreakerSet, Router};

use crate::backend::{ChatBackend, ChunkStream};

/// Backend router with failover.
pub struct RoutingBackend {
    backends: HashMap<String, std::sync::Arc<dyn ChatBackend>>,
    router: Router,
    breakers: Mutex<BreakerSet>,
}

impl RoutingBackend {
    /// Build with default breaker settings (3 failures, 60s cooldown).
    pub fn new(backends: HashMap<String, std::sync::Arc<dyn ChatBackend>>, router: Router) -> Self {
        Self::with_breaker_settings(backends, router, 3, 60)
    }

    /// Build with explicit breaker settings.
    pub fn with_breaker_settings(
        backends: HashMap<String, std::sync::Arc<dyn ChatBackend>>,
        router: Router,
        threshold: u32,
        cooldown_secs: u64,
    ) -> Self {
        Self {
            backends,
            router,
            breakers: Mutex::new(BreakerSet::new(threshold, cooldown_secs)),
        }
    }

    fn lock_breakers(&self) -> std::sync::MutexGuard<'_, BreakerSet> {
        self.breakers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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

        for name in self.router.route(&model) {
            if !self.lock_breakers().allow(&name, Instant::now()) {
                continue;
            }
            let Some(backend) = self.backends.get(&name) else {
                continue;
            };
            match backend.complete(request.clone()).await {
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
        let model = request.model.clone();
        for name in self.router.route(&model) {
            if !self.lock_breakers().allow(&name, Instant::now()) {
                continue;
            }
            if let Some(backend) = self.backends.get(&name) {
                return backend.stream(request);
            }
        }
        Box::pin(futures::stream::once(async move {
            Err(GatewayError::UnknownModel(model))
        }))
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
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: "hi".to_string(),
            }],
            stream: false,
            max_tokens: None,
            temperature: None,
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
        assert_eq!(response.choices[0].message.content, "echo: hi");
    }

    #[tokio::test]
    async fn breaker_skips_tripped_primary() {
        let routing = routing();
        // First call trips "fail" (threshold 1) then serves via echo.
        routing.complete(request("x-model")).await.unwrap();
        // Second call must not touch "fail" at all (breaker open, long cooldown).
        let response = routing.complete(request("x-model")).await.unwrap();
        assert_eq!(response.choices[0].message.content, "echo: hi");
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
}
