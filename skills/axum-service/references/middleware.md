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
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use axum_prometheus::PrometheusMetricLayer;
use axum_tracing_opentelemetry::middleware::{OtelAxumLayer, OtelInResponseLayer};
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{Any, CorsLayer};
use tower_http::timeout::TimeoutLayer;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_swagger_ui::SwaggerUi;

use crate::AppState;
use crate::api;
use crate::config::{REQUEST_TIMEOUT, ServerSettings};
use crate::error::{AppError, ErrorBody, catch_panic_layer};

/// Two megabytes. Anything larger is a file upload and wants its own route.
const BODY_LIMIT_BYTES: usize = 2 * 1024 * 1024;

#[derive(OpenApi)]
#[openapi(info(title = "app", version = env!("CARGO_PKG_VERSION")))]
pub struct ApiDoc;

/// The real router, used by `main` and by every integration test.
///
/// Layers inside a `ServiceBuilder` run OUTERMOST FIRST, the reverse of chained
/// `.layer()` calls. The request id comes first so everything below it, spans and
/// error bodies included, can name the request; the timeout is innermost so the
/// budget measures the handler rather than the middleware above it.
pub fn build_router(state: AppState) -> Router {
    let (prometheus_layer, metric_handle) = PrometheusMetricLayer::pair();
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
                .layer(axum::middleware::from_fn(api::request_id))
                // Directly inside the request id, so a panic anywhere below
                // still answers with the id in the body and the header.
                .layer(catch_panic_layer())
                // Writes `traceparent` back, then reads it off the request and
                // opens the server span with `http.route` from the matched path.
                // This is the only HTTP span: a second HTTP tracing layer next to
                // it would open a second, unrelated one for every request.
                .layer(OtelInResponseLayer)
                .layer(OtelAxumLayer::default())
                .layer(prometheus_layer)
                .option_layer(cors)
                .layer(CompressionLayer::new())
                .layer(axum::extract::DefaultBodyLimit::max(BODY_LIMIT_BYTES))
                // Innermost. A streaming route (SSE, a download) must be mounted
                // OUTSIDE this layer, or the timeout cuts the stream mid-flight.
                // This service has none. A rate limiter is not a global layer
                // either: it goes on the limited router with `route_layer`.
                .layer(TimeoutLayer::with_status_code(
                    StatusCode::REQUEST_TIMEOUT,
                    REQUEST_TIMEOUT,
                )),
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

Outermost to innermost: `api::request_id` (trusts or mints `x-request-id`, opens the `request`
span, scopes the `REQUEST_ID` task-local, echoes the header), `error::catch_panic_layer()`
(tower-http's `CatchPanicLayer` with the scaffold's handler), `OtelInResponseLayer` +
`OtelAxumLayer` (the one HTTP span, `traceparent` in and out), the Prometheus layer, CORS from
`server.cors_origins`, compression, the body limit, the timeout. Everything, `/metrics`, the probes and both fallbacks included, sits inside the
stack, which is why a 404 carries a request id. The request id is a `from_fn` rather than
tower-http's `SetRequestIdLayer` + `PropagateRequestIdLayer`: those two do the header half and put
the value in neither a span nor reach of the error body.

The `ServiceBuilder` has to be inlined. Factoring it into a helper that returns
`impl Layer<Route>` erases the `Service: Clone + Send + Sync` bounds `Router::layer` requires, and
the call site fails with four unsatisfied-trait-bound errors.

`TimeoutLayer::new` is deprecated since tower-http 0.6.7; under `-D warnings` it fails the build.
Use `with_status_code`. `REQUEST_TIMEOUT` lives in `config.rs` next to the other budget constants.
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
`next.run(req).await`. The scaffold's own example is `api::request_id`; this one guards a subtree.

```rust,verify
//! Guard a subtree without touching the handlers in it.
use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;

use crate::AppState;
use crate::error::AppError;

pub async fn require_bearer(
    State(_state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let ok = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("Bearer "));

    if !ok {
        return Err(AppError::Unauthorized);
    }
    Ok(next.run(req).await)
}

pub fn admin_routes(state: AppState) -> axum::Router {
    axum::Router::new()
        .route("/admin/stats", axum::routing::get(|| async { "ok" }))
        // `route_layer`: applies to these routes only, and skips 404s.
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_bearer,
        ))
        .with_state(state)
}
```

Prefer an extractor over a middleware when the handler needs the value: `AuthUser` gives the
handler typed claims, while a middleware can only stash them in an `Extension`.

## The rate-limit slot

The limiter itself — what it counts, the Redis script, the `Limiter` type — is `rust-redis`'s.
Where it sits is fixed here, so the two skills agree:

- It is a `from_fn_with_state` middleware, applied per router with `Router::route_layer` on the
  subtree it protects (`/login`, a paid endpoint), never with `.layer()` on the whole stack: a
  probe, a scrape or a 404 must not spend a token.
- In request order it runs **inside** the whole global stack — after `api::request_id`, the two
  Otel layers and the `TimeoutLayer` — and immediately **before** the handler, extracting the
  caller's identity itself (`extract_parts_with_state::<AuthUser, _>`). That is what
  `route_layer` on a merged sub-router gives you, and it is the right place: a 429 carries a
  request id and lands in the trace, and the request timeout also bounds the limiter's Redis
  round trip.
- It answers `AppError::TooManyRequests { retry_after_secs }`; `error.rs` already renders the 429
  and the `Retry-After` header. No new variant.
- Its fallback identity for an unauthenticated caller is the peer address, which only exists when
  `main.rs` serves with `app.into_make_service_with_connect_info::<SocketAddr>()` — the scaffold
  does. Plain `axum::serve(listener, app)` has no `ConnectInfo`, and the limiter fails every
  anonymous request with a 500 rather than quietly sharing one bucket.

```rust
// main.rs
axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
    .with_graceful_shutdown(shutdown_signal())
    .await?;

// where the limited routes are built
Router::new()
    .route("/login", post(login))
    .route_layer(axum::middleware::from_fn_with_state(state.clone(), rate_limit::<AppState, AuthUser>))
```

## Layers that are easy to misuse

- **`Extension<T>` versus `State`.** `State` is checked at compile time; a missing `Extension` is
  a 500 at request time. Use `Extension` only for per-request values a middleware inserts.
- **`HandleErrorLayer`** is for tower services whose error is not `Infallible` (`tower::timeout`,
  `tower::limit`). The tower-http equivalents used above are already infallible; wrapping them in
  it is a common over-application.
- **The panic catcher is a net, not a path.** `error::catch_panic_layer()` (tower-http feature
  `catch-panic`) sits directly inside `api::request_id`, so a panic anywhere below it is answered
  with the constant 500 `ErrorBody`, request id included, logged as `handler panicked` with the
  message, and the connection stays open. Without it the connection task dies with the panic: no
  status, no body, no log line. The layers inside it are unwound, so that one request reaches the
  log and the client but not the span status or the request counter. Return `AppError`; do not
  `panic!` to signal failure.
- **`CorsLayer::permissive()`** answers every preflight with a wildcard. The stack builds the layer
  from `server.cors_origins` (`APP__SERVER__CORS_ORIGINS=https://a,https://b`) and adds none when
  the list is empty; for a browser client on another origin in local development, set the variable
  rather than swapping the layer.
- **A second HTTP tracing layer** (tower-http's `trace` feature) next to `OtelAxumLayer` opens a
  second, unrelated server span per request. The two Otel layers are the only HTTP span source.
