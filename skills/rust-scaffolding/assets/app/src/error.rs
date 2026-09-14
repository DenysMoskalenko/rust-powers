//! One error type for the whole service. Handlers return
//! `Result<T, AppError>`; `IntoResponse` is the only place a status code is chosen.
use std::any::Any;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use tower_http::catch_panic::CatchPanicLayer;
use utoipa::ToSchema;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Body parsed but failed a `validator` rule.
    #[error("validation failed")]
    Validation(#[from] validator::ValidationErrors),
    /// Body was malformed, the wrong content type, or too large.
    #[error(transparent)]
    JsonRejection(#[from] JsonRejection),
    /// Anything else the client got wrong: an unparseable query string, a path
    /// segment that is not a uuid, a combination of parameters that cannot hold.
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("unauthorized")]
    Unauthorized,
    /// The caller is over a rate limit; `Some` becomes a `Retry-After` header.
    #[error("too many requests")]
    TooManyRequests { retry_after_secs: Option<u64> },
    /// A dependency this request cannot do without (a cache, a lock, a queue) is
    /// down. The string names it in the log; the client sees a constant.
    #[error("{0}")]
    Unavailable(String),
    #[error(transparent)]
    Db(#[from] sea_orm::DbErr),
    #[error(transparent)]
    Http(#[from] reqwest_middleware::Error),
    /// Anything unexpected. Never rendered to the client.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// The wire shape of every error response, fallbacks included.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorBody {
    #[schema(example = "validation failed")]
    pub error: String,
    /// Echoes `x-request-id` so a user-reported failure can be found in the logs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Field to messages for 422, absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl ErrorBody {
    /// Picks up the request id of the request being served.
    pub fn new(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            request_id: current_request_id(),
            details: None,
        }
    }
}

tokio::task_local! {
    /// Set once per request by the request-id middleware, so `IntoResponse` can
    /// reach the id without every handler threading it down.
    pub static REQUEST_ID: String;
}

fn current_request_id() -> Option<String> {
    REQUEST_ID.try_with(Clone::clone).ok()
}

/// Turns a panic anywhere below it into the constant 500 that `AppError::Other`
/// renders, logged with the panic message. Without it the connection task dies
/// with the panic: the client sees a dropped connection, no status, no body, no
/// log line. A panic is still a bug; handlers return `AppError`.
pub fn catch_panic_layer() -> CatchPanicLayer<fn(Box<dyn Any + Send + 'static>) -> Response> {
    CatchPanicLayer::custom(handle_panic)
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "the signature is tower-http's ResponseForPanic"
)]
fn handle_panic(err: Box<dyn Any + Send + 'static>) -> Response {
    // `panic!("literal")` carries a `&str`, `panic!("{x}")` a `String`.
    let message = err
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| err.downcast_ref::<&str>().copied())
        .unwrap_or("unknown panic");
    tracing::error!(panic = %message, "handler panicked");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorBody::new("internal server error")),
    )
        .into_response()
}

impl AppError {
    fn status(&self) -> StatusCode {
        match self {
            // A body that parsed but broke the rules is 422, like FastAPI; a body
            // that did not parse at all is 400.
            Self::Validation(_) | Self::JsonRejection(JsonRejection::JsonDataError(_)) => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            Self::JsonRejection(_) | Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::TooManyRequests { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            // 23505 is a duplicate key: a client problem, not a server one.
            Self::Db(err) => match err.sql_err() {
                Some(sea_orm::SqlErr::UniqueConstraintViolation(_)) => StatusCode::CONFLICT,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            },
            // This service is fine; something it depends on is not.
            Self::Http(_) => StatusCode::BAD_GATEWAY,
            Self::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();

        // Logged once, here, where every failure passes through. 5xx carries the
        // whole cause chain; the 409 line is what tells you which constraint fired.
        if status.is_server_error() || status == StatusCode::CONFLICT {
            tracing::error!(error = ?self, %status, "request failed");
        }

        let body = match &self {
            // A `DbErr` or a reqwest error renders its whole cause chain,
            // constraint names and connection strings included, so neither is ever
            // stringified into a response. Nor is a 409, whose only honest
            // constant is the status itself.
            _ if status == StatusCode::CONFLICT => ErrorBody::new("conflict"),
            Self::Db(_) | Self::Other(_) => ErrorBody::new("internal server error"),
            Self::Http(_) => ErrorBody::new("upstream request failed"),
            Self::Unavailable(_) => ErrorBody::new("service unavailable"),
            Self::Validation(errors) => ErrorBody {
                details: serde_json::to_value(errors).ok(),
                ..ErrorBody::new("validation failed")
            },
            other => ErrorBody::new(other.to_string()),
        };

        let mut response = (status, Json(body)).into_response();
        if let Self::TooManyRequests {
            retry_after_secs: Some(secs),
        } = self
        {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(secs));
        }
        response
    }
}
