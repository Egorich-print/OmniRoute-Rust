use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use omniroute_config::GatewayConfig;
use omniroute_crypto::{FieldCrypto, load_secret};
use omniroute_db::Db;
use omniroute_gateway::{
    AppState,
    backend::ChatBackend,
    backend::EchoBackend,
    build_router_with_state,
    credentials::{active_providers, load_credentials},
    gemini::GeminiBackend,
    grok_cli::GrokCliBackend,
    openai::{OpenAiBackend, ProviderOpenAiBackend, resolve_chat_url},
    routing::RoutingBackend,
};
use omniroute_providers::ProviderRegistry;
use omniroute_routing::{RouteRule, Router};

/// Resolve grok-cli tokens: an explicit env token wins, otherwise every
/// active connection in the database (decrypted, priority-ordered) forms the
/// rotation pool.
fn grok_tokens(db: &Db, data_dir: &std::path::Path) -> Vec<String> {
    if let Ok(token) = std::env::var("GROK_CLI_TOKEN")
        && !token.trim().is_empty()
    {
        return vec![token];
    }
    let crypto = load_secret(Some(data_dir)).and_then(|secret| FieldCrypto::from_secret(&secret));
    let guard = db.connection();
    match load_credentials(&guard, "grok-cli", crypto.as_ref()) {
        Ok(credentials) => credentials
            .into_iter()
            .map(|credential| credential.token)
            .collect(),
        Err(error) => {
            tracing::warn!("grok-cli credential load failed: {error}");
            Vec::new()
        }
    }
}

/// Select the serving backend. Builds every configured backend and routes by
/// model: combo names expand from the database, `provider/model` ids map to
/// the provider backend, `grok-*` prefers grok-cli, and everything else uses
/// an explicit OpenAI-compatible endpoint. The echo backend is a test double
/// and is only registered when `OMNIROUTE_ENABLE_ECHO=1` — it must never
/// silently answer production traffic.
type Selected = (Arc<dyn ChatBackend>, Arc<ProviderRegistry>, Vec<String>);

fn select_backend(
    db: Arc<Db>,
    data_dir: &std::path::Path,
) -> Result<Selected, Box<dyn std::error::Error>> {
    let mut backends: HashMap<String, Arc<dyn ChatBackend>> = HashMap::new();
    let echo_enabled = std::env::var("OMNIROUTE_ENABLE_ECHO")
        .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "yes"));
    if echo_enabled {
        backends.insert("echo".to_string(), Arc::new(EchoBackend));
    }

    let mut grok_present = false;
    let tokens = grok_tokens(&db, data_dir);
    if !tokens.is_empty() {
        let base_url = std::env::var("GROK_CLI_BASE_URL")
            .unwrap_or_else(|_| "https://cli-chat-proxy.grok.com/v1".to_string());
        tracing::info!(
            "backend available: grok-cli ({base_url}, {} credential(s))",
            tokens.len()
        );
        backends.insert(
            "grok-cli".to_string(),
            Arc::new(GrokCliBackend::with_tokens(base_url, tokens)?),
        );
        grok_present = true;
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

    // Per-provider OpenAI-compatible backends from the registry + database.
    let crypto = load_secret(Some(data_dir)).and_then(|secret| FieldCrypto::from_secret(&secret));
    let registry = Arc::new(ProviderRegistry::load());
    let providers = {
        let guard = db.connection();
        active_providers(&guard).unwrap_or_default()
    };
    for provider in providers {
        if backends.contains_key(&provider) {
            continue;
        }
        let Some(base_url) = registry.base_url(&provider) else {
            continue;
        };

        // Gemini speaks its own protocol (`generateContent` + API-key header).
        if registry
            .get(&provider)
            .and_then(|entry| entry.format.as_deref())
            == Some("gemini")
        {
            let tokens: Vec<String> = {
                let guard = db.connection();
                load_credentials(&guard, &provider, crypto.as_ref())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|credential| credential.token)
                    .collect()
            };
            if tokens.is_empty() {
                continue;
            }
            let count = tokens.len();
            match GeminiBackend::new(base_url.to_string(), tokens) {
                Ok(backend) => {
                    tracing::info!(
                        "backend available: {provider} (gemini, {base_url}, {count} credential(s))"
                    );
                    backends.insert(provider, Arc::new(backend));
                }
                Err(error) => tracing::warn!("backend {provider} skipped: {error}"),
            }
            continue;
        }

        if !registry.is_openai_format(&provider) {
            continue;
        }
        let tokens: Vec<String> = {
            let guard = db.connection();
            load_credentials(&guard, &provider, crypto.as_ref())
                .unwrap_or_default()
                .into_iter()
                .map(|credential| credential.token)
                .collect()
        };
        // Public gateways (`auth_type: optional`) answer without a key, so an
        // empty pool is only fatal when the provider actually needs one.
        if tokens.is_empty() && !registry.allows_keyless(&provider) {
            continue;
        }
        let count = tokens.len();
        match ProviderOpenAiBackend::new(provider.clone(), resolve_chat_url(base_url), tokens) {
            Ok(backend) => {
                if count == 0 {
                    tracing::info!("backend available: {provider} ({base_url}, keyless)");
                } else {
                    tracing::info!(
                        "backend available: {provider} ({base_url}, {count} credential(s))"
                    );
                }
                backends.insert(provider, Arc::new(backend));
            }
            Err(error) => tracing::warn!("backend {provider} skipped: {error}"),
        }
    }

    let mut rules = Vec::new();
    if grok_present {
        rules.push(RouteRule {
            model_prefix: "grok-".to_string(),
            backends: vec!["grok-cli".to_string()],
        });
    }
    let mut default = Vec::new();
    if openai_present {
        default.push("openai-compatible".to_string());
    }
    if echo_enabled {
        default.push("echo".to_string());
        tracing::info!("backend available: echo (test double, opt-in)");
    }
    let registry = Arc::new(ProviderRegistry::load());
    let names: Vec<String> = backends.keys().cloned().collect();
    let routing = RoutingBackend::new(backends, Router::new(rules, default))
        .with_combo_source(db, registry.clone());
    Ok((Arc::new(routing), registry, names))
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

    let db = Arc::new(db);
    let (backend, registry, backend_names) = select_backend(db.clone(), &config.data_dir)?;
    let state = AppState::with_db(backend, db)
        .with_require_auth(config.require_auth)
        .with_catalog(registry, backend_names);
    let app = build_router_with_state(state);

    let ip: std::net::IpAddr = config.host.parse()?;
    let addr = SocketAddr::new(ip, config.port);
    let listener = tokio::net::TcpListener::bind(addr).await?;

    tracing::info!("omniroute-gateway listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
