//! Process wiring only. Everything testable lives in the library.
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use app::clock::SystemClock;
use app::config::{DB_STATEMENT_TIMEOUT, HTTP_CLIENT_TIMEOUT, SHUTDOWN_TIMEOUT, Settings};
use app::{AppState, build_router};
use migration::{Migrator, MigratorTrait as _};
use sea_orm::{ConnectionTrait as _, TransactionTrait as _};
use secrecy::ExposeSecret as _;

/// Any fixed number. Advisory locks are scoped to the database, so it only has
/// to differ from the keys the service's own jobs take.
const MIGRATION_LOCK_KEY: i64 = 4_815_162_342;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // First line: a missing .env is a no-op, so production reads real env vars.
    dotenvy::dotenv().ok();

    let settings = Settings::load().context("loading settings")?;
    let telemetry = app::telemetry::init(&settings.telemetry).context("init telemetry")?;

    migrate(settings.database.url.expose_secret()).await?;

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
    // falls back to for callers without an API key. Never a bare `app`: axum
    // then deep-copies every route for each connection it accepts.
    let (draining_tx, draining_rx) = tokio::sync::oneshot::channel::<()>();
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown_signal().await;
        draining_tx.send(()).ok();
    });

    // Graceful shutdown waits for every open connection, and a streaming
    // response (SSE, a long download) never closes on its own. The deadline
    // starts at the signal; without it the orchestrator's SIGKILL ends the
    // process before the spans are flushed and the pool is closed.
    tokio::select! {
        result = async { server.await } => result.context("server error")?,
        () = drain_deadline(draining_rx) => {
            tracing::warn!(timeout = ?SHUTDOWN_TIMEOUT, "drain deadline passed with connections still open");
        }
    }

    // Order matters: connections are drained above, then spans are flushed,
    // then the pool is closed. `close` waits for checked-out connections, so it
    // is bounded too.
    telemetry.shutdown();
    if tokio::time::timeout(Duration::from_secs(5), db.close())
        .await
        .is_err()
    {
        tracing::warn!("pool still had connections checked out at exit");
    }
    Ok(())
}

/// Migrations get connections of their own, with no `statement_timeout`: the
/// service's ten seconds would cancel a backfill or an index build half-way.
/// An advisory lock, held by an open transaction until the migrations finish,
/// makes replicas that start together migrate one at a time; the ones that
/// waited find nothing left to apply. With rolling deploys the old code still
/// runs against the new schema, so sequence breaking changes as expand,
/// migrate, contract, or run migrations as a separate job (`make migrate`).
async fn migrate(database_url: &str) -> anyhow::Result<()> {
    let mut options = sea_orm::ConnectOptions::new(database_url);
    options
        // One for the transaction that holds the lock, one for the migrations.
        .max_connections(2)
        .connect_timeout(Duration::from_secs(5))
        .sqlx_logging(false);
    let db = sea_orm::Database::connect(options)
        .await
        .context("connecting to Postgres for migrations")?;

    let lock = db.begin().await.context("opening the migration lock")?;
    // This session idles in its transaction while the migrations run on the
    // other connection. A role or server timeout for that would end it, and
    // the lock with it. `transaction_timeout` exists from Postgres 17: drop
    // that statement on an older server.
    lock.execute_unprepared(
        "SET LOCAL idle_in_transaction_session_timeout = 0; SET LOCAL transaction_timeout = 0",
    )
    .await
    .context("lifting the idle timeouts from the migration lock")?;
    lock.execute_unprepared(&format!(
        "SELECT pg_advisory_xact_lock({MIGRATION_LOCK_KEY})"
    ))
    .await
    .context("waiting for the migration lock")?;

    let migrated = Migrator::up(&db, None).await.context("running migrations");

    // Committing releases the lock; so does a crash, with the session.
    lock.commit()
        .await
        .context("releasing the migration lock")?;
    db.close()
        .await
        .context("closing the migration connections")?;
    migrated
}

/// Resolves `SHUTDOWN_TIMEOUT` after the shutdown signal, and never if the
/// server stops for any other reason first.
async fn drain_deadline(draining: tokio::sync::oneshot::Receiver<()>) {
    if draining.await.is_ok() {
        tokio::time::sleep(SHUTDOWN_TIMEOUT).await;
    } else {
        std::future::pending::<()>().await;
    }
}

async fn shutdown_signal() {
    // A listener that cannot be installed must not look like a signal: the
    // arm stays pending, or the server would shut down as soon as it started.
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "cannot listen for ctrl-c");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        // SIGTERM is what Kubernetes and `docker stop` send; without this arm the
        // process is killed after the grace period instead of draining.
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
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
