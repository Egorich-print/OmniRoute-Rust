use std::net::SocketAddr;
use std::sync::Arc;

use omniroute_gateway::{backend::EchoBackend, build_router};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let port: u16 = std::env::var("OMNIROUTE_RUST_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(20_129);

    let app = build_router(Arc::new(EchoBackend));
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;

    tracing::info!("omniroute-gateway listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
