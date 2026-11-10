pub mod health;
pub mod users;

use axum::extract::Request;
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument as _;
use utoipa_axum::router::OpenApiRouter;
use uuid::Uuid;

use crate::AppState;
use crate::REQUEST_ID_HEADER;
use crate::config::REQUEST_TIMEOUT;
use crate::error::{AppError, REQUEST_ID};

/// Every route in the service. Merged rather than nested: each module declares
/// its own absolute paths, so the `OpenAPI` document and the router cannot drift.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .merge(health::router())
        .merge(users::router())
}

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
