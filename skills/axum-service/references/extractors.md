# Errors, extractors and rejections

- [AppError and ErrorBody](#apperror-and-errorbody)
- [Valid and ValidQuery](#valid-and-validquery)
- [Rejection types and status codes](#rejection-types-and-status-codes)
- [Handler argument order](#handler-argument-order)
- [Other ways to customise a rejection](#other-ways-to-customise-a-rejection)

## AppError and ErrorBody

`error.rs` is the only file in the service that chooses a status code. Services return domain
errors or `anyhow::Error`; `#[from]` lifts them, and `?` in a handler does the rest. The
`REQUEST_ID` task-local is declared here and nowhere else; the request-id middleware scopes it,
and `ErrorBody::new` reads it.

```rust,verify
//! One error type for the whole service. Handlers return
//! `Result<T, AppError>`; `IntoResponse` is the only place a status code is chosen.
use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
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
```

The message affixes belong to the caller: `NotFound(format!("user {id} not found"))`, not
`NotFound("user")`, because the variants render `{0}` as-is.

`ValidationErrors` serialises as a map of field to an array of `{ code, message, params }`, and a
nested struct as an object instead of an array, so clients branch on array-versus-object to walk
nested errors. `code` is the stable machine key; `message` is `null` unless the rule sets one.

The generic `other => ErrorBody::new(other.to_string())` arm echoes axum's own rejection text for a
4xx (`Failed to deserialize the JSON body into the target type: age: invalid type: string "old",
expected u8 at line 1 column 12`). That publishes Rust field names and types, which is acceptable
for a client mistake and never happens for a 5xx or a 409, whose bodies are constants.

Give `ErrorBody` to utoipa on every fallible route: `(status = 422, body = ErrorBody)`. The 404
and 405 fallbacks in `build_router` answer in the same shape, so there is one wire shape for every
failure, including a wrong path.

### Extending the type

Add-on skills (a cache, a broker, an LLM provider) never add a variant. They map their own error
onto what is here with one function: an unreachable or failed backend is `Unavailable(String)`
(503, the string logged), a provider or client rate limit is `TooManyRequests { .. }` (429), a
wiring bug is `Other(anyhow::Error)` (500), bad input is `BadRequest(String)` (400). 502 is only
ever `Http`, this service's own outbound call.

The one arm the scaffold does not ship, because its schema has no foreign key yet: SQLSTATE 23503,
`SqlErr::ForeignKeyConstraintViolation`, is a client error like 23505 — a `POST /orders` naming a
`user_id` that does not exist. Without the arm it is a 500. A pre-flight `SELECT` is not the fix:
it loses every race the constraint wins. Add the arm on both matches the day the first foreign key
lands:

```rust
// in `status()`
Self::Db(err) => match err.sql_err() {
    Some(sea_orm::SqlErr::UniqueConstraintViolation(_)) => StatusCode::CONFLICT,
    Some(sea_orm::SqlErr::ForeignKeyConstraintViolation(_)) => StatusCode::UNPROCESSABLE_ENTITY,
    _ => StatusCode::INTERNAL_SERVER_ERROR,
},

// in `into_response()`, before the `Self::Db(_) | Self::Other(_)` arm
Self::Db(err) if matches!(err.sql_err(), Some(sea_orm::SqlErr::ForeignKeyConstraintViolation(_))) => {
    ErrorBody::new("referenced row does not exist")
}
```

The body is a constant: the constraint name is a schema detail, and a 422 is not logged (4xx are the
client's mistake), so nothing leaks. Document the route with `(status = 422, body = ErrorBody)`, the
same entry a validation failure uses.

## Valid and ValidQuery

```rust,verify
//! Extractors that validate. `Valid<T>` is the `Json<T>` replacement and
//! `ValidQuery<T>` the `Query<T>` one; both reject with [`AppError`], so a
//! failed rule is a 422 with the same body shape as every other error.
use axum::extract::{FromRequest, FromRequestParts, Query, Request};
use axum::http::request::Parts;
use serde::de::DeserializeOwned;
use validator::Validate;

use crate::error::AppError;

#[derive(Debug, Clone, Copy)]
pub struct Valid<T>(pub T);

// Extractors are plain `async fn` in a trait; `#[async_trait]` is a 0.6-era pattern.
impl<S, T> FromRequest<S> for Valid<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Validate,
{
    type Rejection = AppError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let axum::Json(value) = axum::Json::<T>::from_request(req, state).await?;
        value.validate()?;
        Ok(Self(value))
    }
}

/// A separate type, not a second impl on `Valid<T>`: one type cannot implement
/// both `FromRequest` and `FromRequestParts`, the blanket impls overlap.
#[derive(Debug, Clone, Copy)]
pub struct ValidQuery<T>(pub T);

impl<S, T> FromRequestParts<S> for ValidQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Validate,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Query(value) = Query::<T>::from_request_parts(parts, state)
            .await
            // A query string that does not even deserialise is a 400; one that
            // deserialises and then fails a rule is the 422 below.
            .map_err(|rejection| AppError::BadRequest(rejection.body_text()))?;
        value.validate()?;
        Ok(Self(value))
    }
}
```

Both take `type Rejection = AppError`, which is the cheapest custom rejection there is: the
extractor fails, `AppError::into_response` renders the same body as every other failure.

Validator rules worth knowing: `#[validate(nested)]` recurses into a field that is itself
`Validate` (the bare `#[validate]` spelling is gone); `custom(function = f)` takes a quoted string
or a bare path and the function is `fn(&T) -> Result<(), ValidationError>`; `Option<T>` fields are
skipped when `None`. For a value that is constructed once and then valid everywhere, use a
`nutype` newtype instead — but never both on one field, because nutype's `Deserialize` rejects
before `validator` runs and the response loses its field map.

## Rejection types and status codes

| Rejection | Cause | Status here | `details` |
|---|---|---|---|
| `ValidationErrors` | parsed, failed a rule | 422 | field map |
| `JsonRejection::JsonDataError` | valid JSON, wrong shape or missing field | 422 | absent |
| `JsonRejection::JsonSyntaxError` | malformed JSON | 400 | absent |
| `JsonRejection::MissingJsonContentType` | no `application/json` | 400 | absent |
| `JsonRejection::BytesRejection` | body over `DefaultBodyLimit` | 400 | absent |
| `QueryRejection`, `PathRejection` | wrong type or arity in the URL | 400 via `BadRequest` | absent |
| `TypedHeaderRejection` | missing or malformed `Authorization` | 401 via `Unauthorized` | absent |

Two shapes share the 422 status: a `Validation` failure carries the field map, a
`JsonDataError` carries only the prose `error`, so a client treats `details` as optional. An
oversized body is a 400 like every other unparseable body; a client that must branch on "send
less" gets it from an extra arm, `Self::JsonRejection(JsonRejection::BytesRejection(_)) =>
StatusCode::PAYLOAD_TOO_LARGE`, ahead of the catch-all.

Since 0.8 `Query` and `Form` report the failing field through `serde_path_to_error`, `Path` tuples
check arity exactly, `Json` rejects trailing characters after the document, and
`Option<Query<T>>` no longer swallows every error — opt into "absent is fine" with
`OptionalFromRequestParts`.

## Handler argument order

`FromRequestParts` extractors read headers and the URI only and may appear any number of times.
The one `FromRequest` extractor consumes the body and must be the last argument.

```rust,ignore
// Wrong: body extractor before a parts extractor. The compiler says only
// "the trait bound `Handler<_, _>` is not satisfied", pointing at the route.
pub async fn update_user(
    Valid(body): Valid<UpdateUser>,
    Path(id): Path<Uuid>,
) -> Result<Json<UserResponse>, AppError> { todo!() }
```

Add `#[axum::debug_handler]` (the `macros` feature) above the handler and the error turns into
"consider moving this argument last". Reach for it first whenever a route stops compiling.

## Other ways to customise a rejection

Three levels, cheapest first:

1. Implement `From<SomeRejection>` on `AppError` and match the variant — what `AppError` does.
2. `axum_extra::extract::WithRejection<Json<T>, MyError>` to reuse a stock extractor with a
   different rejection at one call site.
3. A newtype extractor with `type Rejection = AppError` — what `Valid` and `ValidQuery` are.
