use std::net::SocketAddr;
use std::sync::Arc;

use omniroute_config::GatewayConfig;
use omniroute_db::Db;
use omniroute_gateway::{AppState, backend::EchoBackend, build_router_with_state};

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

    let state = AppState::with_db(Arc::new(EchoBackend), Arc::new(db))
        .with_require_auth(config.require_auth);
    let app = build_router_with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], config.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;

    tracing::info!("omniroute-gateway listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
