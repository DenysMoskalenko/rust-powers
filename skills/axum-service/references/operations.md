# Operational endpoints and lifecycle

- [Health, readiness and version](#health-readiness-and-version)
- [Request id](#request-id)
- [Startup and shutdown order](#startup-and-shutdown-order)
- [Timeout budget](#timeout-budget)
- [Pagination contract](#pagination-contract)
- [Work that outlives the response](#work-that-outlives-the-response)
- [Idempotent writes](#idempotent-writes)
- [Outbound HTTP](#outbound-http)
- [Memory after load](#memory-after-load)

## Health, readiness and version

Liveness answers "is the process alive" and must not touch a dependency — a database blip should
not make the orchestrator kill a healthy process. Readiness answers "can this instance serve
traffic" and checks each dependency under a short timeout. Both are unauthenticated, both live
outside any auth layer, and neither is versioned.

### The readiness contract

`GET /health/ready` answers with one body shape, whatever the service depends on:

```json
{ "status": "degraded", "checks": { "database": "ok", "cache": "degraded" } }
```

- Every value is `"ok"`, `"degraded"` or `"unavailable"`; `status` is the worst value in `checks`.
- A **required** dependency — the database — that fails or times out is `"unavailable"`, and the
  probe answers **503**: the load balancer stops routing here.
- An **optional** dependency — a cache, a message broker — that fails is `"degraded"`, and the
  probe stays **200**: requests still succeed without it, so the instance keeps its traffic.
- An add-on skill adds exactly one entry to `checks`, named after the dependency (`"cache"`,
  `"messaging"`), computed with the same `timeout(READINESS_TIMEOUT, ..)` shape as `database`
  below. The body, the status rule and the enum are owned here and never redefined elsewhere.

```rust,verify
//! `api/health.rs` — the three endpoints an orchestrator asks for. Keep them free
//! of business logic: a readiness probe that runs a real query will flap.
use std::collections::BTreeMap;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::AppState;
use crate::config::READINESS_TIMEOUT;

#[derive(Debug, Serialize, ToSchema)]
pub struct Version {
    pub name: String,
    pub version: String,
}

/// One dependency's answer. Ordered worst-last so the overall status is `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Health {
    Ok,
    /// An optional dependency (a cache, a broker) is down; requests still work.
    Degraded,
    /// A required dependency (the database) is down; take this instance out.
    Unavailable,
}

/// The readiness body: the worst check decides `status`, and `Unavailable` is
/// the only value that turns the probe into a 503.
#[derive(Debug, Serialize, ToSchema)]
pub struct Readiness {
    pub status: Health,
    /// One entry per dependency, named after the thing checked.
    pub checks: BTreeMap<String, Health>,
}

/// Liveness: the process is running. Never touches a dependency, or Kubernetes
/// restarts the pod every time the database hiccups.
#[utoipa::path(
    get, path = "/health/live", tag = "health",
    responses((status = 200, description = "The process is running"))
)]
pub async fn live() -> StatusCode {
    StatusCode::OK
}

/// Readiness: this instance can serve traffic. A failure takes it out of the load
/// balancer without killing it.
#[utoipa::path(
    get, path = "/health/ready", tag = "health",
    responses(
        (status = 200, body = Readiness, description = "Ready; an optional dependency may be degraded"),
        (status = 503, body = Readiness, description = "A required dependency is unreachable"),
    )
)]
// House style: `skip_all` plus the fields worth having. `skip(state)` would still
// try to record every other argument, which is how a password reaches a log.
#[tracing::instrument(skip_all)]
pub async fn ready(State(state): State<AppState>) -> (StatusCode, Json<Readiness>) {
    // One line per dependency. A required one answers `Unavailable` when it
    // fails; an optional one (cache, messaging) answers `Degraded` instead and
    // leaves the probe at 200.
    let checks = BTreeMap::from([("database".to_owned(), database(&state.db).await)]);

    let status = checks.values().copied().max().unwrap_or(Health::Ok);
    let code = if status == Health::Unavailable {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    (code, Json(Readiness { status, checks }))
}

async fn database(db: &sea_orm::DatabaseConnection) -> Health {
    // A ping that hangs is a failed probe, not a probe that never answers: without
    // the timeout the orchestrator's own deadline decides, and the pod looks dead.
    let reason = match tokio::time::timeout(READINESS_TIMEOUT, db.ping()).await {
        Ok(Ok(())) => return Health::Ok,
        Ok(Err(error)) => format!("ping failed: {error}"),
        Err(_) => format!("ping timed out after {READINESS_TIMEOUT:?}"),
    };
    tracing::warn!(%reason, "readiness: database unavailable");
    Health::Unavailable
}

/// Build info, so a running pod can be matched to a release.
#[utoipa::path(
    get, path = "/version", tag = "health",
    responses((status = 200, body = Version, description = "Crate name and version"))
)]
pub async fn version() -> Json<Version> {
    Json(Version {
        name: env!("CARGO_PKG_NAME").to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    })
}

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(live))
        .routes(routes!(ready))
        .routes(routes!(version))
}
```

To report the commit as well, add `commit: option_env!("GIT_SHA").unwrap_or("unknown")` and pass
`GIT_SHA` as a build argument in the Dockerfile. Readiness should fail during shutdown too, so the
load balancer stops sending traffic before the drain begins; a shared `AtomicBool` flipped by the
shutdown signal is enough.

## Request id

One middleware owns the id. It keeps an inbound `x-request-id` only when it is 1–64 characters of
`[A-Za-z0-9._-]` and mints a uuid v7 otherwise: the header is client input that lands on every log
line, in every error body and in a response header, so anything that is not id-shaped is replaced
rather than trusted. It opens a `request` span carrying the id (so every log line inside the
request has it), scopes the `REQUEST_ID` task-local declared in `error.rs` (so `ErrorBody::new`
can copy it into the body), and echoes the header on the response. In `build_router` it sits
directly inside the two Otel layers and outside everything else, so the 404 fallback gets an id
as well, and so does the constant 500 that `error::catch_panic_layer()`, directly inside it,
renders for a handler panic (logged; the connection is not dropped).

```rust,verify
//! `api/mod.rs` — one value in three places: the span, the error body, the response header.
use axum::extract::Request;
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument as _;
use uuid::Uuid;

use crate::REQUEST_ID_HEADER;
use crate::error::REQUEST_ID;

/// One middleware owns the request id: it keeps an inbound `x-request-id` that
/// looks like an id, mints a uuid v7 otherwise, puts the id on every log line
/// inside the request through the span, hands it to `AppError` through a task
/// local, and echoes it back on the response. Splitting this across tower-http's
/// set and propagate layers costs two layers and still leaves the span and the
/// error body without the id.
pub async fn request_id(request: Request, next: Next) -> Response {
    let id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| is_request_id(value))
        .map_or_else(|| Uuid::now_v7().to_string(), ToOwned::to_owned);

    let span = tracing::info_span!("request", request_id = %id);
    let mut response = REQUEST_ID
        .scope(id.clone(), next.run(request).instrument(span))
        .await;

    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    response
}

/// The inbound id is client input that lands on every log line, in every error
/// body and in a response header, so anything that is not id-shaped is replaced.
fn is_request_id(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}
```

`REQUEST_ID_HEADER` is `HeaderName::from_static("x-request-id")` in `lib.rs`. The span is opened
with the id as a field and `.instrument(span)` wraps the rest of the request; recording onto
`Span::current()` would not work, because the current span at that point is the Otel server span,
which declares no `request_id` field.

## Startup and shutdown order

```rust,verify
//! `main.rs` — process wiring only. Everything testable lives in the library.
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
```

Order matters in both directions: telemetry comes first so everything below is logged and traced,
migrations run before the listener binds so a failed migration never serves a request, and the
exporter flushes before the pool closes so the spans that explain the shutdown are not lost. The
exporter shutdown blocks — call it after `serve` returns, never from inside an async task on a
current-thread runtime. `ConnectOptions` beyond those shown (pool sizing per replica, idle and
lifetime limits) belong to `sea-orm-postgres`; `sqlx_logging(false)` is there because every
statement at INFO is seven log lines per request.

**Migrations at startup.** `migrate()` runs before the service pool exists, and three things in
it are deliberate:

- **Its own connections, with no `statement_timeout`.** The service pool carries
  `DB_STATEMENT_TIMEOUT` as a connection option, so a migration run on it is cancelled after ten
  seconds: a backfill stops half-way, and a cancelled `CREATE INDEX CONCURRENTLY` leaves an
  invalid index behind. Never hand the service pool to `Migrator::up`. A timeout set on the
  role (`ALTER ROLE .. SET statement_timeout`) still reaches these connections, which log in
  as that role: there, migrate from a job under a role without it.
- **`pg_advisory_xact_lock(MIGRATION_LOCK_KEY)` held by an open transaction.** Replicas that
  start together migrate one at a time, and the ones that waited find nothing left to apply. The
  pool has two connections for that reason: one holds the lock, the other runs the migrations.
  Committing releases the lock, and so does a crash, with the session. The key is any fixed number
  that differs from the keys the service's own jobs take. The lock session idles while the
  migrations run, so it first lifts `idle_in_transaction_session_timeout` and
  `transaction_timeout` for itself. The second exists from Postgres 17; on an older server it is
  an unrecognized parameter and startup fails, so drop that statement there.
- **The result is returned only after the lock is released and the connections are closed.** A
  failed migration still stops the process; it does not leave the lock for the session teardown
  to release.

The lock only orders the migrations. During a rolling deploy the old code keeps running against
the new schema, so a breaking change is sequenced as expand, migrate, contract, or the migrations
run as a separate job (`make migrate`) before the rollout.

**Shutdown has a deadline.** Graceful shutdown waits for every open connection, and a streaming
response (SSE, a long download) never closes on its own. `drain_deadline` starts `SHUTDOWN_TIMEOUT`
at the signal and the `select!` stops waiting when it passes; without it the orchestrator's
SIGKILL ends the process before the spans are flushed and the pool is closed. Two details keep
this honest: a signal listener that cannot be installed stays pending instead of resolving, or the
server would shut down the moment it started, and `db.close()` is bounded at five seconds because
it waits for checked-out connections.

## Timeout budget

The budget lives in `config.rs` as five constants, in one place so the numbers can be compared.
Each is shorter than the layer above it: a handler that has given up waiting on Postgres still has
time to render an error before the request timeout fires.

```text
READINESS_TIMEOUT       2 s   the probe's ping
DB_STATEMENT_TIMEOUT   10 s   Postgres cancels the statement server-side
HTTP_CLIENT_TIMEOUT    10 s   one outbound call, connect included
REQUEST_TIMEOUT        30 s   the whole inbound request (api::request_timeout, innermost)
                         <   the caller's client timeout
SHUTDOWN_TIMEOUT       35 s   after SIGTERM, how long in-flight requests get
                         <   the orchestrator's grace period
```

Inverted, the client gives up while the query keeps running and the connection stays busy. Keep
the strict order when changing a number: `statement_timeout` is the `ConnectOptions` value in
`main.rs` above and in `sea-orm-postgres`'s baseline connect, and both read 10 s against the
30 s request budget.

A handler past `REQUEST_TIMEOUT` is answered by `api::request_timeout` with
`AppError::Unavailable`: 503, `"service unavailable"` in the `ErrorBody` shape with the request
id, logged once. The middleware races the future that produces the response, never the body, so a
streaming route is not cut off.

`SHUTDOWN_TIMEOUT` is longer than `REQUEST_TIMEOUT` so a request that arrived just before the
signal can finish, and the orchestrator's grace period has to be longer still, or the process is
killed mid-drain. Kubernetes' default `terminationGracePeriodSeconds` is 30, below the 35 s here:
raise it well above `SHUTDOWN_TIMEOUT` (the span flush and the pool close, up to five seconds,
run after the drain), or lower `SHUTDOWN_TIMEOUT` and `REQUEST_TIMEOUT` together.

## Pagination contract

One contract for the whole service: `?limit=&offset=`, default limit 20, maximum 100 and an
`offset` of at most `MAX_OFFSET`, both enforced by validation (a larger value is a 422, not a
silent clamp), and an envelope with the total so a client can render a page count.

```rust,verify
//! The query parameters every list endpoint takes, and the `Page` envelope.
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use validator::Validate;

/// `OFFSET` reads and discards every skipped row, so it is capped: past this a
/// client narrows the filter, or the endpoint moves to keyset pagination. The
/// cap is also what keeps a `u64` from the query string inside the `i64` that
/// Postgres takes; the driver panics on a larger value.
const MAX_OFFSET: u64 = 10_000;

#[derive(Debug, Deserialize, Validate, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListUsers {
    // The serde default makes the parameter optional on the wire; the `param`
    // attributes repeat the rule for the document, which never reads `validate`.
    #[validate(range(min = 1, max = 100))]
    #[serde(default = "default_limit")]
    #[param(default = 20, minimum = 1, maximum = 100)]
    pub limit: u64,
    #[validate(range(max = MAX_OFFSET))]
    #[serde(default)]
    #[param(default = 0, maximum = 10000)]
    pub offset: u64,
}

fn default_limit() -> u64 {
    20
}

/// Never return a bare array: adding `total` later is a breaking change.
/// utoipa 6 handles the generic itself: the document names it `Page_UserResponse`.
#[derive(Debug, Serialize, ToSchema)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub limit: u64,
    pub offset: u64,
}
```

The scaffold's `list_users` fills it with a `count` and the page fetch, inline: the two belong to
one page and count as the one query a handler runs. The query needs a second ordering key
(`created_at`, then `id`) or OFFSET over a non-unique sort is not stable.

`offset` is capped at `MAX_OFFSET`, 10 000, for two reasons. Postgres reads and discards every
skipped row, so a deep offset is a slow query anyone can ask for. And the value is a `u64` from
the query string that the driver binds as an `i64`: past `i64::MAX` the conversion panics, which
turns `?offset=9223372036854775808` into a 500. Every `u64` that reaches `.offset()` or `.limit()`
needs a validated upper bound for that reason. A client that needs to go deeper narrows the
filter, or the endpoint switches to a cursor (`?after=<id>&limit=`); the envelope then carries
`next_cursor` instead of `total`.

## Work that outlives the response

axum has no built-in after-response hook. Spawn the work, clone what it needs out of the request,
and log the error inside the task — a dropped `JoinHandle` discards the result silently.

```rust,verify
//! Fire-and-forget after the response, with the failure visible.
use axum::extract::State;
use axum::http::StatusCode;
use tracing::Instrument as _;

use crate::AppState;
use crate::error::AppError;

pub async fn resend_receipt(State(state): State<AppState>) -> Result<StatusCode, AppError> {
    state.db.ping().await?; // the work the client is waiting for happens first

    // The task is `'static`, so it cannot borrow the handler's state: move the
    // handle in (it is an `Arc`) or clone the few values the task needs.
    let http = state.http;

    tokio::spawn(
        async move {
            if let Err(err) = http.post("https://mail.example.com/send").send().await {
                tracing::error!(error = %err, "receipt delivery failed");
            }
        }
        // Without this the task starts a new trace and the work looks orphaned.
        .instrument(tracing::info_span!("resend_receipt")),
    );

    Ok(StatusCode::ACCEPTED)
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new().route("/receipts/resend", axum::routing::post(resend_receipt))
}
```

The task dies with the process, so shutdown drops whatever is still queued. Anything that must
not be lost belongs in a table the service polls, not in `tokio::spawn`.

## Idempotent writes

A client that retries a POST after a timeout must not create a second row. For a create, prefer a
natural unique key (an email, an external reference) and let the unique index reject the
duplicate — that is the 409 `AppError` already produces from SQLSTATE 23505, and the uniqueness
lives in the database, so two concurrent retries cannot both win.

For a side effect with no natural key (a payment, an outbound call) the HTTP contract is the
`Idempotency-Key` header. Fingerprint = hash of method + path with its query string + body. Same key, same fingerprint:
replay the stored response, status and body alike. Same key, different fingerprint: 422. Same key
while the first attempt is still in flight: 409, and the client retries later. A key belongs to
the caller that sent it: the record is stored and looked up under the caller's verified identity
(the token subject, a verified API key) together with the header value, never under the header
alone, or two clients that send the same value get each other's stored response. Storage — Redis,
a short in-flight TTL, a 24 h result TTL — is `rust-redis`'s; this file owns only the contract.

That storage dedups retries and replays the answer; it does not make the effect atomic with the
record of it, so the contract fits an effect that survives being repeated — an outbound call
forwarding the same key, deduplicated by the provider. An effect that is a database write writes
its idempotency row in the same transaction as the write instead (`sea-orm-postgres`), and needs
none of this.

## Outbound HTTP

One `ClientWithMiddleware` lives in `AppState` and is cloned per call — it is an `Arc` around the
connection pool, so building one per request re-does TLS setup and leaks connections. Set a total
timeout (`HTTP_CLIENT_TIMEOUT`, inside the request budget) and a connect timeout; there is no
default total timeout.

Retries are first-party in reqwest 0.13, and a policy does nothing until it has a classifier:
`reqwest::retry::for_host(..)` alone classifies every attempt as final, so it retries nothing. A
custom policy also replaces the client's default one, for every host, so the default retry of
protocol NACKs (an HTTP/2 `REFUSED_STREAM`, a graceful `GOAWAY`) is gone as well. `classify_fn`
decides both: the classifier below retries any transport error for an idempotent method, which
covers those NACKs, and retries nothing else.

```rust,verify,test
//! Retries for one upstream, and the proof that they happen.
use std::time::Duration;

use axum::http::{Method, StatusCode};
use httpmock::Method::{GET, POST};
use httpmock::MockServer;

use crate::config::HTTP_CLIENT_TIMEOUT;

/// The `reqwest::Client` that `main.rs` wraps in `reqwest_middleware::ClientBuilder`.
fn upstream_client(host: String) -> reqwest::Result<reqwest::Client> {
    let upstream = host.clone();
    let retries = reqwest::retry::for_host(host)
        // On top of the first attempt.
        .max_retries_per_request(2)
        .classify_fn(move |attempt| {
            // The scope stops a second retry to another host, not the first.
            let in_scope = attempt.uri().host() == Some(upstream.as_str());
            // Only what the upstream can safely see twice.
            let idempotent = matches!(
                *attempt.method(),
                Method::GET | Method::HEAD | Method::PUT | Method::DELETE
            );
            // No response at all, or a status that says "not now".
            let transient = attempt.error().is_some()
                || matches!(
                    attempt.status(),
                    Some(
                        StatusCode::BAD_GATEWAY
                            | StatusCode::SERVICE_UNAVAILABLE
                            | StatusCode::GATEWAY_TIMEOUT
                    )
                );
            if in_scope && idempotent && transient {
                attempt.retryable()
            } else {
                attempt.success()
            }
        });

    reqwest::Client::builder()
        .timeout(HTTP_CLIENT_TIMEOUT)
        .connect_timeout(Duration::from_secs(3))
        .retry(retries)
        .build()
}

#[tokio::test]
async fn a_get_is_retried_on_503_and_a_post_is_not() -> anyhow::Result<()> {
    let upstream = MockServer::start_async().await;
    let rates = upstream
        .mock_async(|when, then| {
            when.method(GET).path("/rates");
            then.status(503);
        })
        .await;
    let charges = upstream
        .mock_async(|when, then| {
            when.method(POST).path("/charges");
            then.status(503);
        })
        .await;
    let client = upstream_client(upstream.host())?;

    let response = client.get(upstream.url("/rates")).send().await?;
    client.post(upstream.url("/charges")).send().await?;

    // The last attempt's answer comes back as it is; a retry policy never turns it into an error.
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(rates.calls_async().await, 3, "the first attempt and two retries");
    assert_eq!(charges.calls_async().await, 1, "a POST is never sent twice");
    Ok(())
}
```

- **Retry idempotent requests only.** GET, HEAD, PUT and DELETE are safe to repeat; a POST is not,
  unless the upstream honours an idempotency key, and then the classifier may admit that route.
- **Compare the host inside the classifier.** The classifier is asked about every request the
  client sends, and in reqwest 0.13.5 the `for_host` scope only stops a second retry: without the
  `in_scope` line, a request to any other host is retried once.
- **Retries have a budget and no backoff.** Every policy carries a budget of 20% extra load
  (`max_extra_load` changes it), so a failing upstream is not hammered, but a retry is sent at
  once: nothing waits between attempts. For a `Retry-After` or a backoff, retry in the calling
  code instead.
- **The client's `timeout` covers all attempts together**, so retries never stretch a call past
  `HTTP_CLIENT_TIMEOUT`.
- **The retries happen inside `reqwest::Client`**, below `reqwest-tracing`, so a retried call is
  still one client span. Do not add `reqwest-retry`; `reqwest-middleware` is in the stack for
  `traceparent` injection, not for retries.

## Memory after load

Checked against axum 0.8.9, governor 0.10.4, moka 0.12.16 and tikv-jemallocator 0.7.0. Resident
memory that rises under load and never comes back is live heap until a profile says otherwise.
Look in this order.

1. **The serve call.** Given a bare `Router`, `axum::serve` calls `self.clone().with_state(())`
   for every accepted connection: a copy of every route and everything its handler captured,
   held until the connection closes. `into_make_service()` and
   `into_make_service_with_connect_info()` build the service once and hand out an `Arc` clone.
   Behind a gateway that keeps connections open, the bare form's memory follows the peak
   connection count and stays there.
2. **What handlers own.** A route clones its handler for every request, so a closure that owns
   a `Vec<u8>` or `String` copies it each time. Serialize a large static body once into `Bytes`,
   or keep it in `State` behind an `Arc`. `SwaggerUi::url(path, api)` already shares its
   document through an `Arc` (utoipa-swagger-ui 10).
3. **Per-key maps.** A governor keyed limiter never drops a key by itself: call
   `retain_recent()`, then `shrink_to_fit()`. moka evicts only inside reads and writes, so an
   idle cache keeps its entries: set `time_to_idle` or `time_to_live` and call
   `run_pending_tasks().await`. Run both from one `tokio::time::interval` task that stops on
   the shutdown signal, after `serve` returns and before telemetry is flushed.
4. **The allocator, last.** Swapping it does not free live heap. Profile first: build with
   tikv-jemallocator's `profiling` feature, run with
   `_RJEM_MALLOC_CONF=prof:true,prof_final:true` (`prof:true` alone writes nothing) and read
   the dump with `jeprof`. Keep the system allocator unless a profile justifies another.

Autoscale on CPU. Memory does not follow load: the allocator keeps freed pages, and caches and
pools stay full, so a memory-based autoscaler sits at its maximum after the first peak.
