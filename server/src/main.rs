use std::net::SocketAddr;

use std::sync::Arc;

use sourcer_server::{
    ai, app, candidates::RankHandler, config::Config, db, mail::Mail, recruitly::Recruitly,
    searching::PullHandler, sending::Sender, sources::pdl::PdlClient, worker::Worker,
};
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

    // People Data Labs, shared by the search screen and the background pulls.
    let pdl = Arc::new(PdlClient::new(config.pdl_api_key.clone()));
    if !pdl.configured() {
        tracing::warn!("Searching is off; set PDL_API_KEY to turn it on");
    }

    // Claude, shared by brief drafting and the background ranker.
    let ai = Arc::new(
        ai::Claude::new(
            config.anthropic_api_key.clone(),
            config.anthropic_model.clone(),
        )
        .with_draft_model(config.anthropic_draft_model.clone()),
    );
    if !ai.configured() {
        tracing::warn!("Brief drafting and ranking are off; set ANTHROPIC_API_KEY to turn them on");
    }

    // Recruitly: roles from its jobs, the check at shortlist, and handover.
    let recruitly = Arc::new(Recruitly::new(
        config.recruitly_api_key.clone(),
        config.recruitly_daily_cap,
    ));
    if !recruitly.configured() {
        tracing::warn!("The Recruitly link is off; set RECRUITLY_API_KEY to turn it on");
    }

    // Background jobs. Handlers are registered as features land.
    let (stop, stopped) = watch::channel(false);
    let worker = Worker::new(pool.clone())
        .register(Arc::new(PullHandler {
            pool: pool.clone(),
            source: pdl.clone(),
        }))
        .register(Arc::new(RankHandler {
            pool: pool.clone(),
            ai: ai.clone(),
        }));
    let worker = tokio::spawn(worker.run(stopped.clone()));

    // Each person's Outlook: sends approved emails and watches for replies.
    let mail = Arc::new(Mail::new(config.mail()?));
    let sender = tokio::spawn(
        Sender {
            pool: pool.clone(),
            mail: mail.clone(),
        }
        .run(stopped),
    );

    let addr: SocketAddr = config.bind_addr.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "sourcer server listening");

    let auth = config.auth();
    if auth.is_none() {
        tracing::warn!("Microsoft 365 sign-in is not configured; set M365_TENANT_ID, M365_CLIENT_ID and M365_CLIENT_SECRET");
    }
    let mut state = app::AppState::new(Some(pool), auth);
    state.pdl = pdl;
    state.ai = ai;
    state.recruitly = recruitly;
    state.mail = mail;
    let router = app::router_with_web(state, config.web_dir.as_deref());
    // Connection info lets the sign-in limit count attempts per address.
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown())
    .await?;

    let _ = stop.send(true);
    worker.await?;
    sender.await?;
    Ok(())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
