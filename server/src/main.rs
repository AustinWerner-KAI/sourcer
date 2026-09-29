use std::net::SocketAddr;

use sourcer_server::{app, config::Config, db, worker::Worker};
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Config::from_env()?;
    tracing::info!(?config, "starting");
    let pool = db::connect(&config.database_url).await?;
    db::migrate(&pool).await?;

    // Background jobs. Handlers are registered as features land.
    let (stop, stopped) = watch::channel(false);
    let worker = tokio::spawn(Worker::new(pool.clone()).run(stopped));

    let addr: SocketAddr = config.bind_addr.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "sourcer server listening");

    let auth = config.auth();
    if auth.is_none() {
        tracing::warn!("Microsoft 365 sign-in is not configured; set M365_TENANT_ID, M365_CLIENT_ID and M365_CLIENT_SECRET");
    }
    let router = app::router_with_web(
        app::AppState::new(Some(pool), auth),
        config.web_dir.as_deref(),
    );
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown())
        .await?;

    let _ = stop.send(true);
    worker.await?;
    Ok(())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
