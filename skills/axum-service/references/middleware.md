# The middleware stack

- [The stack, in order](#the-stack-in-order)
- [The two ordering rules](#the-two-ordering-rules)
- [Writing a middleware](#writing-a-middleware)
- [The rate-limit slot](#the-rate-limit-slot)
- [Layers that are easy to misuse](#layers-that-are-easy-to-misuse)

## The stack, in order

`build_router` is the one place the stack is assembled, and integration tests call it, so they
run through the real layers. This is the scaffold's `lib.rs` minus `AppState`.

```rust,verify
//! `lib.rs` — one inlined `ServiceBuilder`, outermost layer first.
use std::sync::OnceLock;

use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use axum_prometheus::metrics_exporter_prometheus::PrometheusHandle;
use axum_prometheus::{EndpointLabel, PrometheusMetricLayerBuilder};
use axum_tracing_opentelemetry::middleware::{OtelAxumLayer, OtelInResponseLayer};
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{Any, CorsLayer};
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_swagger_ui::SwaggerUi;

use crate::AppState;
use crate::api;
use crate::config::ServerSettings;
use crate::error::{AppError, ErrorBody, catch_panic_layer};

/// Two megabytes. Anything larger is a file upload and wants its own route.
const BODY_LIMIT_BYTES: usize = 2 * 1024 * 1024;

#[derive(OpenApi)]
#[openapi(
    info(title = "app", version = env!("CARGO_PKG_VERSION")),
    tags(
        (name = "health", description = "Liveness, readiness and build info"),
        (name = "users", description = "User management"),
    )
)]
pub struct ApiDoc;

/// The `metrics` recorder is one per process by design. It is installed by the
/// first router and shared by every later one (a second listener, the next test
/// in the same process), which would otherwise panic installing it again.
static METRICS: OnceLock<PrometheusHandle> = OnceLock::new();

fn metrics_handle() -> PrometheusHandle {
    METRICS
        .get_or_init(|| axum_prometheus::Handle::default().0)
        .clone()
}

/// A request no route matched has no `MatchedPath`. Labelling it with the raw
/// path, the default, mints one series per URL a scanner tries.
fn unmatched(_path: &str) -> String {
    "unmatched".to_owned()
}

/// The real router, used by `main` and by every integration test.
///
/// Layers inside a `ServiceBuilder` run OUTERMOST FIRST, the reverse of chained
/// `.layer()` calls. The server span is outermost, so it times and records
/// everything below it, a panic and a timeout included; the request id sits
/// inside it, so every log line and error body can name the request; the timeout
/// is innermost, so the budget measures the handler rather than the middleware.
pub fn build_router(state: AppState) -> Router {
    let (prometheus_layer, metric_handle) = PrometheusMetricLayerBuilder::new()
        .with_endpoint_label_type(EndpointLabel::MatchedPathWithFallbackFn(unmatched))
        .with_metrics_from_fn(metrics_handle)
        .build_pair();
    let cors = cors_layer(&state.settings.server);

    let (router, api) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(api::router())
        .split_for_parts();

    router
        .merge(SwaggerUi::new("/docs").url("/openapi.json", api))
        .route("/metrics", get(move || async move { metric_handle.render() }))
        // Both fallbacks answer in `ErrorBody`, so a client never has to parse a
        // different shape, or an empty body, for some failures.
        .fallback(|| async { AppError::NotFound("route not found".to_owned()) })
        .method_not_allowed_fallback(|| async {
            (
                StatusCode::METHOD_NOT_ALLOWED,
                Json(ErrorBody::new("method not allowed")),
            )
                .into_response()
        })
        .layer(
            ServiceBuilder::new()
                // Reads `traceparent` off the request and opens the server span
                // with `http.route` from the matched path. This is the only HTTP
                // span: a tower-http `TraceLayer` next to it would open a second,
                // unrelated one for every request.
                .layer(OtelAxumLayer::default())
                // Writes the server span's `traceparent` on the response, so it
                // has to run inside that span. The order is the reverse of the
                // crate's README, which chains `.layer()` on a `Router`.
                .layer(OtelInResponseLayer)
                // Inside the server span: the `request` span it opens is then a
                // child in the same trace, not a second root.
                .layer(axum::middleware::from_fn(api::request_id))
                // Directly inside the request id, so a panic anywhere below
                // still answers with the id in the body and the header.
                .layer(catch_panic_layer())
                .layer(prometheus_layer)
                .option_layer(cors)
                .layer(CompressionLayer::new())
                .layer(axum::extract::DefaultBodyLimit::max(BODY_LIMIT_BYTES))
                // Innermost. A streaming route (SSE, a download) stays inside
                // this stack and is not cut off. A rate limiter is not a global
                // layer either: it goes on the limited router with `route_layer`.
                .layer(axum::middleware::from_fn(api::request_timeout)),
        )
        .with_state(state)
}

/// No configured origins means no CORS layer at all, which is same-origin only.
/// `CorsLayer::permissive()` is never the answer: it answers every preflight with
/// a wildcard, including from a page the user did not expect to be calling you.
fn cors_layer(settings: &ServerSettings) -> Option<CorsLayer> {
    let origins: Vec<HeaderValue> = settings
        .cors_origins
        .iter()
        .filter_map(|origin| origin.parse().ok())
        .collect();

    (!origins.is_empty()).then(|| {
        CorsLayer::new()
            .allow_origin(origins)
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
            ])
            .allow_headers(Any)
    })
}
```

Outermost to innermost: `OtelAxumLayer` (reads `traceparent`, opens the one HTTP server span),
`OtelInResponseLayer` (writes that span's `traceparent` on the response), `api::request_id` (keeps
an inbound `x-request-id` of 1–64 `[A-Za-z0-9._-]` characters or mints a uuid v7, opens the
`request` span, scopes the `REQUEST_ID` task-local, echoes the header),
`error::catch_panic_layer()` (tower-http's `CatchPanicLayer` with the scaffold's handler), the
Prometheus layer, CORS from `server.cors_origins`, compression, the body limit,
`api::request_timeout`. Everything, `/metrics`, the probes and both fallbacks included, sits inside the
stack, which is why a 404 carries a request id. The request id is a `from_fn` rather than
tower-http's `SetRequestIdLayer` + `PropagateRequestIdLayer`: those two do the header half and put
the value in neither a span nor reach of the error body.

The first three positions are load-bearing, and a test in the scaffold's `tests/api/stack.rs`
holds them:

- `OtelAxumLayer` is outermost, so the server span times and records everything below it, the
  constant 500 of a panic and the 503 of a timeout included.
- `OtelInResponseLayer` comes second. It writes the context of the span that is current when the
  response passes through it, so it has to run inside the server span. The crate's README lists the
  two in the other order because it chains `.layer()` on a `Router`, where the last call is
  outermost; copied into a `ServiceBuilder` as written, that order puts `OtelInResponseLayer`
  outside the server span, and the response carries the context of whatever span surrounds it.
- `api::request_id` comes third, inside the server span, so the `request` span it opens is a child
  in the caller's trace. Placed outside, the `request` span is the root of a trace of its own.

With the request id outermost and the two Otel layers in README order, a request that arrives
with a `traceparent` gets one back with a different trace id. An existing service with that order
has this defect; report it instead of reordering unasked.

The `ServiceBuilder` has to be inlined. Factoring it into a helper that returns
`impl Layer<Route>` erases the `Service: Clone + Send + Sync` bounds `Router::layer` requires, and
the call site fails with four unsatisfied-trait-bound errors.

The request timeout is the scaffold's own `api::request_timeout`, shown under
[Writing a middleware](#writing-a-middleware), and `REQUEST_TIMEOUT` lives in `config.rs` next to
the other budget constants. It answers through `AppError::Unavailable`: a 503 in the `ErrorBody`
shape with the request id, logged once, so a timeout looks like every other failure. tower-http's
`TimeoutLayer` is not in the stack, and its `timeout` feature is not enabled: it answers with a
bare status and an empty body, the one response that would break the shape, and the status it is
usually given, 408, says the client was slow to send the request, which is not what happened. In
a project that does use it, `TimeoutLayer::new` is deprecated since tower-http 0.6.7 and fails the
build under `-D warnings`; use `with_status_code`.
`DefaultBodyLimit` is axum's own limit layer, so the tower-http `limit` feature is not needed.

## The two ordering rules

They point in opposite directions, which is the single most common mistake in this file:

| Construction | Outermost is |
|---|---|
| `ServiceBuilder::new().layer(a).layer(b)` | `a` — the first call |
| `router.layer(a).layer(b)` | `b` — the last call |

Requests travel outermost to innermost, responses come back the other way. Two more rules:
`.layer()` wraps only the routes registered **before** it, and `.route_layer()` wraps only
already-registered routes and does **not** run when the request 404s — which is what an auth
layer wants, so a probe for an unknown path does not get a 401.

## Writing a middleware

`axum::middleware::from_fn` and `from_fn_with_state` wrap an `async fn` that ends with
`next.run(req).await`. The scaffold has two, `api::request_id` and `api::request_timeout`. This is
the second, and the same idea applied to one subtree.

```rust,verify
//! `api/mod.rs` — the request budget, and a tighter one for a single subtree.
use std::time::Duration;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::get;

use crate::AppState;
use crate::config::REQUEST_TIMEOUT;
use crate::error::AppError;

/// The request budget. It races the future that produces the response, never
/// the body, so a streaming route (SSE, a download) is not cut off. The timeout
/// answers through `AppError`: a 503 in the `ErrorBody` shape with the request
/// id, logged once. tower-http's `TimeoutLayer` answers with an empty body.
///
/// # Errors
/// `AppError::Unavailable` when no response was produced within `REQUEST_TIMEOUT`.
pub async fn request_timeout(request: Request, next: Next) -> Result<Response, AppError> {
    tokio::time::timeout(REQUEST_TIMEOUT, next.run(request))
        .await
        .map_err(|_| AppError::Unavailable(format!("no response within {REQUEST_TIMEOUT:?}")))
}

/// The same budget as a parameter. `from_fn_with_state` hands the function
/// whatever value it is given; it does not have to be `AppState`.
///
/// # Errors
/// `AppError::Unavailable` when no response was produced within `limit`.
pub async fn budget(
    State(limit): State<Duration>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    tokio::time::timeout(limit, next.run(request))
        .await
        .map_err(|_| AppError::Unavailable(format!("no response within {limit:?}")))
}

pub fn search_routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/search", get(|| async { "[]" }))
        // `route_layer`: these routes only, and not when the path matched none.
        .route_layer(axum::middleware::from_fn_with_state(
            Duration::from_secs(2),
            budget,
        ))
}
```

A middleware that rejects returns `Result<Response, AppError>`, so its refusal is rendered and
logged by `error.rs` like a handler's. Prefer an extractor over a middleware when the handler
needs the value: `AuthUser` gives the handler typed claims, while a middleware can only stash them
in an `Extension`. Authentication for a whole subtree is a `from_fn` that takes `AuthUser` as an
argument, so the token is verified by the same code as everywhere else; a middleware that only
checks that an `Authorization` header is present or well-formed lets any string through.

## The rate-limit slot

The limiter itself — what it counts, the Redis script, the `Limiter` type — is `rust-redis`'s.
Where it sits is fixed here, so the two skills agree:

- It is a `from_fn_with_state` middleware, applied per router with `Router::route_layer` on the
  subtree it protects (`/login`, a paid endpoint), never with `.layer()` on the whole stack: a
  probe, a scrape or a 404 must not spend a token.
- In request order it runs **inside** the whole global stack — after the two Otel layers,
  `api::request_id` and `api::request_timeout` — and immediately **before** the handler, extracting the
  caller's identity itself (`extract_parts_with_state::<AuthUser, _>`). That is what
  `route_layer` on a merged sub-router gives you, and it is the right place: a 429 carries a
  request id and lands in the trace, and the request timeout also bounds the limiter's Redis
  round trip.
- It answers `AppError::TooManyRequests { retry_after_secs }`; `error.rs` already renders the 429
  and the `Retry-After` header. No new variant.
- Which address identifies an unauthenticated caller is `rust-redis`'s trusted-proxy rule; its
  last resort, the `ConnectInfo` peer, only exists when `main.rs` serves with
  `app.into_make_service_with_connect_info::<SocketAddr>()` — the scaffold does. A bare
  `Router` handed to `axum::serve` has no `ConnectInfo`, so the limiter fails every anonymous
  request with a 500 rather than quietly sharing one bucket. It is wrong for a second reason
  that has nothing to do with the limiter: axum 0.8.9 deep-copies a bare router for every
  connection, so `into_make_service()` is the minimum even where no peer address is needed.

```rust
// main.rs
axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())

// where the limited routes are built
Router::new()
    .route("/login", post(login))
    .route_layer(axum::middleware::from_fn_with_state(state.clone(), rate_limit::<AppState, AuthUser>))
```

## Layers that are easy to misuse

- **`Extension<T>` versus `State`.** `State` is checked at compile time; a missing `Extension` is
  a 500 at request time. Use `Extension` only for per-request values a middleware inserts.
- **`HandleErrorLayer`** is for tower services whose error is not `Infallible` (`tower::timeout`,
  `tower::limit`). The tower-http layers and the `from_fn` middleware used above are already
  infallible; wrapping them in it is a common over-application.
- **The panic catcher is a net, not a path.** `error::catch_panic_layer()` (tower-http feature
  `catch-panic`) sits directly inside `api::request_id`, so a panic anywhere below it is answered
  with the constant 500 `ErrorBody`, request id included, logged as `handler panicked` with the
  message, and the connection stays open. Without it the connection task dies with the panic: no
  status, no body, no log line. The layers inside it are unwound, so that one request reaches the
  log, the client and the server span, which sits outside and records the 500, but not the request
  counter. Return `AppError`; do not `panic!` to signal failure.
- **`CorsLayer::permissive()`** answers every preflight with a wildcard. The stack builds the layer
  from `server.cors_origins` (`APP__SERVER__CORS_ORIGINS=https://a,https://b`) and adds none when
  the list is empty; for a browser client on another origin in local development, set the variable
  rather than swapping the layer. An entry must be `scheme://host[:port]` with nothing after it,
  which is exactly what a browser sends in `Origin`: `Settings` refuses anything else at startup,
  because `https://app.example.com/` would otherwise be configured and never match.
- **A second HTTP tracing layer** (tower-http's `trace` feature) next to `OtelAxumLayer` opens a
  second, unrelated server span per request. The two Otel layers are the only HTTP span source.
