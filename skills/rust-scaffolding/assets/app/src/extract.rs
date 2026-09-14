//! Extractors that reject with [`AppError`]. `Valid<T>` is the `Json<T>`
//! replacement, `ValidQuery<T>` the `Query<T>` one and `Path<T>` the
//! `axum::extract::Path<T>` one, so a failed rule or a malformed URL is an
//! `ErrorBody` with the same shape as every other error.
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

/// `axum::extract::Path<T>` rejects with a plain-text `Invalid URL: ...`, the one
/// 400 in the service that would not be an `ErrorBody`. This wrapper has no
/// validation of its own; it exists only to route that rejection through
/// [`AppError`].
#[derive(Debug, Clone, Copy)]
pub struct Path<T>(pub T);

impl<S, T> FromRequestParts<S> for Path<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let axum::extract::Path(value) = axum::extract::Path::<T>::from_request_parts(parts, state)
            .await
            // A segment of the wrong type is the client's mistake: a 400
            // carrying axum's own text. The wrong arity or an unsupported type
            // is the handler signature's, which axum already calls a 500.
            .map_err(|rejection| {
                let text = rejection.body_text();
                if rejection.status().is_server_error() {
                    AppError::Other(anyhow::anyhow!("{text}"))
                } else {
                    AppError::BadRequest(text)
                }
            })?;
        Ok(Self(value))
    }
}
