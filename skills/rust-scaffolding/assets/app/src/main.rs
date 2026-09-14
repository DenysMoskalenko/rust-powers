//! Process wiring only. Everything testable lives in the library.
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context as _;
use app::clock::SystemClock;
use app::config::{DB_STATEMENT_TIMEOUT, HTTP_CLIENT_TIMEOUT, Settings};
use app::{AppState, build_router};
use migration::{Migrator, MigratorTrait as _};
use secrecy::ExposeSecret as _;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // First line: a missing .env is a no-op, so production reads real env vars.
    dotenvy::dotenv().ok();

    let settings = Settings::load().context("loading settings")?;
    let telemetry = app::telemetry::init(&settings.telemetry).context("init telemetry")?;

    let mut options = sea_orm::ConnectOptions::new(settings.database.url.expose_secret());
    options
        .max_connections(settings.database.max_connections)
        .connect_timeout(Duration::from_secs(5))
        .acquire_timeout(Duration::from_secs(5))
        // Server-side guard: Postgres cancels a runaway query even if the client
        // has stopped waiting. Shorter than the request timeout on purpose.
        .statement_timeout(DB_STATEMENT_TIMEOUT)
        // Off: every statement at INFO is seven lines per request. Flip it on
        // locally when you need to see the SQL.
        .sqlx_logging(false);
    let db = sea_orm::Database::connect(options)
        .await
        .context("connecting to Postgres")?;

    // Fine for a single-replica service. With rolling deploys, run migrations as
    // a separate job so two replicas cannot migrate at once.
    Migrator::up(&db, None)
        .await
        .context("running migrations")?;

    let http = reqwest_middleware::ClientBuilder::new(
        reqwest::Client::builder()
            .timeout(HTTP_CLIENT_TIMEOUT)
            .connect_timeout(Duration::from_secs(3))
            .build()
            .context("building the HTTP client")?,
    )
    // Injects `traceparent` into every outbound request.
    .with(reqwest_tracing::TracingMiddleware::default())
    .build();

    let address = (settings.server.host.clone(), settings.server.port);
    let state = AppState {
        db: db.clone(),
        settings: Arc::new(settings),
        clock: Arc::new(SystemClock),
        http,
    };
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(address)
        .await
        .context("binding the listener")?;
    tracing::info!(addr = ?listener.local_addr()?, "listening");

    // `ConnectInfo` gives every request the peer address, which a rate limiter
    // falls back to for callers without an API key. Plain `app` has no such thing.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("server error")?;

    // Order matters: connections are drained by `axum::serve` above, then spans
    // are flushed, then the pool is closed.
    telemetry.shutdown();
    db.close().await.context("closing the pool")?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let terminate = async {
        // SIGTERM is what Kubernetes and `docker stop` send; without this arm the
        // process is killed after the grace period instead of draining.
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutting down");
}
