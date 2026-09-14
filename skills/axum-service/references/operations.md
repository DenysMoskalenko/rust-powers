# Operational endpoints and lifecycle

- [Health, readiness and version](#health-readiness-and-version)
- [Request id](#request-id)
- [Startup and shutdown order](#startup-and-shutdown-order)
- [Timeout budget](#timeout-budget)
- [Pagination contract](#pagination-contract)
- [Work that outlives the response](#work-that-outlives-the-response)
- [Idempotent writes](#idempotent-writes)
- [Outbound HTTP](#outbound-http)

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
#[utoipa::path(get, path = "/health/live", tag = "health", responses((status = 200)))]
pub async fn live() -> StatusCode {
    StatusCode::OK
}

/// Readiness: this instance can serve traffic. A failure takes it out of the load
/// balancer without killing it.
#[utoipa::path(
    get, path = "/health/ready", tag = "health",
    responses(
        (status = 200, body = Readiness),
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

/// Build info, so a running pod can be matched to a commit.
#[utoipa::path(get, path = "/version", tag = "health", responses((status = 200, body = Version)))]
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

One middleware owns the id. It trusts an inbound `x-request-id`, mints a uuid v7 otherwise, opens
a `request` span carrying it (so every log line inside the request has it), scopes the
`REQUEST_ID` task-local declared in `error.rs` (so `ErrorBody::new` can copy it into the body),
and echoes the header on the response. It is the outermost layer in `build_router`, so the 404
fallback gets an id as well, and so does the constant 500 that `error::catch_panic_layer()`,
directly inside it, renders for a handler panic (logged; the connection is not dropped).

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

/// One middleware owns the request id: it trusts an inbound `x-request-id`, mints
/// a uuid v7 otherwise, puts the id on every log line inside the request through
/// the span, hands it to `AppError` through a task local, and echoes it back on
/// the response. Splitting this across tower-http's set and propagate layers costs
/// two layers and still leaves the span and the error body without the id.
pub async fn request_id(request: Request, next: Next) -> Response {
    let id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
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
```

Order matters in both directions: telemetry comes first so everything below is logged and traced,
migrations run before the listener binds so a failed migration never serves a request, and the
exporter flushes before the pool closes so the spans that explain the shutdown are not lost. The
exporter shutdown blocks — call it after `serve` returns, never from inside an async task on a
current-thread runtime. `ConnectOptions` beyond those shown (pool sizing per replica, idle and
lifetime limits) belong to `sea-orm-postgres`; `sqlx_logging(false)` is there because every
statement at INFO is seven log lines per request.

## Timeout budget

The budget lives in `config.rs` as four constants, in one place so the numbers can be compared.
Each is shorter than the layer above it: a handler that has given up waiting on Postgres still has
time to render an error before the request timeout fires.

```text
READINESS_TIMEOUT       2 s   the probe's ping
DB_STATEMENT_TIMEOUT   10 s   Postgres cancels the statement server-side
HTTP_CLIENT_TIMEOUT    10 s   one outbound call, connect included
REQUEST_TIMEOUT        30 s   the whole inbound request (TimeoutLayer, innermost)
                         <   the caller's client timeout
```

Inverted, the client gives up while the query keeps running and the connection stays busy. Keep
the strict order when changing a number: `statement_timeout` is the `ConnectOptions` value in
`main.rs` above and in `sea-orm-postgres`'s baseline connect, and both read 10 s against the
30 s request budget.

## Pagination contract

One contract for the whole service: `?limit=&offset=`, default limit 20, maximum 100 enforced by
validation (a larger value is a 422, not a silent clamp), and an envelope with the total so a
client can render a page count.

```rust,verify
//! The query parameters every list endpoint takes, and the `Page` envelope.
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use validator::Validate;

#[derive(Debug, Deserialize, Validate, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListUsers {
    // The serde default makes the parameter optional on the wire; the `param`
    // attributes repeat the rule for the document, which never reads `validate`.
    #[validate(range(min = 1, max = 100))]
    #[serde(default = "default_limit")]
    #[param(default = 20, minimum = 1, maximum = 100)]
    pub limit: u64,
    #[serde(default)]
    #[param(default = 0)]
    pub offset: u64,
}

fn default_limit() -> u64 {
    20
}

/// Never return a bare array: adding `total` later is a breaking change.
/// utoipa 5 handles the generic itself: the document names it `Page_UserResponse`.
#[derive(Debug, Serialize, ToSchema)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub limit: u64,
    pub offset: u64,
}
```

The scaffold's `list_users` fills it. `offset` is deliberately unbounded: a
deep offset is slow rather than wrong, and the query needs a second ordering key (`created_at`,
then `id`) or OFFSET over a non-unique sort is not stable. Switch to a cursor
(`?after=<id>&limit=`) only when offsets get deep enough to hurt; the envelope then carries
`next_cursor` instead of `total`.

## Work that outlives the response

There is no `BackgroundTasks` equivalent. Spawn the work, clone what it needs out of the request,
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
`Idempotency-Key` header. Fingerprint = hash of method + path + body. Same key, same fingerprint:
replay the stored response, status and body alike. Same key, different fingerprint: 422. Same key
while the first attempt is still in flight: 409, and the client retries later. Storage — Redis,
a short in-flight TTL, a 24 h result TTL — is `rust-redis`'s; this file owns only the contract.

## Outbound HTTP

One `ClientWithMiddleware` lives in `AppState` and is cloned per call — it is an `Arc` around the
connection pool, so building one per request re-does TLS setup and leaks connections. Set a total
timeout (`HTTP_CLIENT_TIMEOUT`, inside the request budget) and a connect timeout; there is no
default total timeout. Retries are first-party in reqwest 0.13:

```rust
reqwest::Client::builder()
    .timeout(HTTP_CLIENT_TIMEOUT)
    .connect_timeout(Duration::from_secs(3))
    .retry(reqwest::retry::for_host("api.example.com").max_retries_per_request(3))
    .build()?
```

Retry idempotent requests only: GET, PUT and DELETE are safe, a POST is not unless the upstream
honours an idempotency key. `max_extra_load` keeps a token budget so a failing upstream cannot be
hammered. Do not add `reqwest-retry`; `reqwest-middleware` is in the stack for `traceparent`
injection, not for retries.
