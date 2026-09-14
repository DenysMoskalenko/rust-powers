//! The whole service lives in the library so that integration tests in `tests/`
//! can build the real router. `main.rs` is process wiring only.
pub mod api;
pub mod clock;
pub mod config;
pub mod entities;
pub mod error;
pub mod extract;
pub mod telemetry;

use std::sync::Arc;

use axum::http::{HeaderName, HeaderValue, Method, StatusCode};
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

use crate::clock::Clock;
use crate::config::{REQUEST_TIMEOUT, ServerSettings, Settings};
use crate::error::{AppError, ErrorBody, catch_panic_layer};

pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// Two megabytes. Anything larger is a file upload and wants its own route.
const BODY_LIMIT_BYTES: usize = 2 * 1024 * 1024;

/// Shared state. Every field is cheap to clone: `DatabaseConnection` is an `Arc`
/// around the pool (never wrap it in another one) and so is the HTTP client.
#[derive(Clone)]
pub struct AppState {
    pub db: sea_orm::DatabaseConnection,
    pub settings: Arc<Settings>,
    pub clock: Arc<dyn Clock>,
    pub http: reqwest_middleware::ClientWithMiddleware,
}

#[derive(OpenApi)]
#[openapi(
    info(title = "app", version = env!("CARGO_PKG_VERSION")),
    tags(
        (name = "health", description = "Liveness, readiness and build info"),
        (name = "users", description = "User management"),
    )
)]
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
                // This is the only HTTP span: a tower-http `TraceLayer` next to it
                // would open a second, unrelated one for every request.
                .layer(OtelInResponseLayer)
                .layer(OtelAxumLayer::default())
                .layer(prometheus_layer)
                .option_layer(cors)
                .layer(CompressionLayer::new())
                .layer(axum::extract::DefaultBodyLimit::max(BODY_LIMIT_BYTES))
                // Innermost. It races the future that produces the response,
                // never the body, so a streaming route (SSE, a download) stays
                // inside this stack and is not cut off. A rate limiter is not a
                // global layer either: it goes on the limited router with
                // `route_layer`.
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
