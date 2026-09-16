use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use omniroute_config::GatewayConfig;
use omniroute_db::Db;
use omniroute_gateway::{
    AppState, backend::ChatBackend, backend::EchoBackend, build_router_with_state,
    grok_cli::GrokCliBackend, openai::OpenAiBackend, routing::RoutingBackend,
};
use omniroute_routing::{RouteRule, Router};

/// Select the serving backend. Builds every configured backend and routes by
/// model: `grok-*` prefers grok-cli, everything else prefers an explicit
/// OpenAI-compatible endpoint, and echo is the ultimate fallback.
/// Connection-driven selection arrives with full combo support later.
fn select_backend() -> Result<Arc<dyn ChatBackend>, Box<dyn std::error::Error>> {
    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    backends.insert("echo".to_string(), Arc::new(EchoBackend));

    let mut grok_present = false;
    if let Ok(token) = std::env::var("GROK_CLI_TOKEN")
        && !token.trim().is_empty()
    {
        let base_url = std::env::var("GROK_CLI_BASE_URL")
            .unwrap_or_else(|_| "https://cli-chat-proxy.grok.com/v1".to_string());
        backends.insert(
            "grok-cli".to_string(),
            Arc::new(GrokCliBackend::new(base_url.clone(), token)?),
        );
        grok_present = true;
        tracing::info!("backend available: grok-cli ({base_url})");
    }

    let mut openai_present = false;
    if let Ok(base_url) = std::env::var("OPENAI_COMPAT_BASE_URL")
        && !base_url.trim().is_empty()
    {
        let api_key = std::env::var("OPENAI_COMPAT_API_KEY").ok();
        backends.insert(
            "openai-compatible".to_string(),
            Arc::new(OpenAiBackend::new(base_url.clone(), api_key)?),
        );
        openai_present = true;
        tracing::info!("backend available: openai-compatible ({base_url})");
    }

    let mut rules = Vec::new();
    if grok_present {
        rules.push(RouteRule {
            model_prefix: "grok-".to_string(),
            backends: vec!["grok-cli".to_string(), "echo".to_string()],
        });
    }
    let mut default = Vec::new();
    if openai_present {
        default.push("openai-compatible".to_string());
    }
    default.push("echo".to_string());

    tracing::info!("backend available: echo (fallback)");
    Ok(Arc::new(RoutingBackend::new(
        backends,
        Router::new(rules, default),
    )))
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
