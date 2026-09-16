use std::net::SocketAddr;
use std::sync::Arc;

use omniroute_config::GatewayConfig;
use omniroute_db::Db;
use omniroute_gateway::{
    AppState, backend::ChatBackend, backend::EchoBackend, build_router_with_state,
    grok_cli::GrokCliBackend, openai::OpenAiBackend,
};

/// Select the serving backend. `GROK_CLI_TOKEN` wins, then an explicit
/// OpenAI-compatible endpoint, otherwise the deterministic echo backend.
/// Connection-driven selection arrives with routing (Phase 4).
fn select_backend() -> Result<Arc<dyn ChatBackend>, Box<dyn std::error::Error>> {
    match std::env::var("GROK_CLI_TOKEN") {
        Ok(token) if !token.trim().is_empty() => {
            let base_url = std::env::var("GROK_CLI_BASE_URL")
                .unwrap_or_else(|_| "https://cli-chat-proxy.grok.com/v1".to_string());
            tracing::info!("backend: grok-cli ({base_url})");
            Ok(Arc::new(GrokCliBackend::new(base_url, token)?))
        }
        _ => match std::env::var("OPENAI_COMPAT_BASE_URL") {
            Ok(base_url) if !base_url.trim().is_empty() => {
                let api_key = std::env::var("OPENAI_COMPAT_API_KEY").ok();
                tracing::info!("backend: openai-compatible ({base_url})");
                Ok(Arc::new(OpenAiBackend::new(base_url, api_key)?))
            }
            _ => {
                tracing::info!(
                    "backend: echo (set GROK_CLI_TOKEN or OPENAI_COMPAT_BASE_URL for live upstreams)"
                );
                Ok(Arc::new(EchoBackend))
            }
        },
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = GatewayConfig::from_env();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(config.log_filter.clone())),
        )
        .with_target(false)
        .init();

    let db_path = config.data_dir.join("storage.sqlite");
    let db = Db::open(&db_path)?;
    let applied = db.migrate()?;
    if applied.is_empty() {
        tracing::info!("database ready at {}", db_path.display());
    } else {
        tracing::info!(
            "database migrated ({}) at {}",
            applied.join(","),
            db_path.display()
        );
    }

    let backend = select_backend()?;
    let state = AppState::with_db(backend, Arc::new(db)).with_require_auth(config.require_auth);
    let app = build_router_with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], config.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;

    tracing::info!("omniroute-gateway listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
