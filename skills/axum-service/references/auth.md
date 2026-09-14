# JWT authentication and password hashing

## Contents

- [Keys, claims and the AuthUser extractor](#keys-claims-and-the-authuser-extractor)
- [Password hashing with argon2 0.6](#password-hashing-with-argon2-06)

Needs two crates beyond the base set: `axum-extra = { version = "0.12", features = ["typed-header"] }`
(0.10 is a year stale and does not declare axum 0.8), `jsonwebtoken = "11"` and, for passwords,
`argon2 = "0.6"`.

## Keys, claims and the AuthUser extractor

```rust,verify
//! `auth.rs` — key material in state, a `FromRequestParts` extractor per route.
use axum::RequestPartsExt;
use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use axum_extra::TypedHeader;
use axum_extra::headers::{Authorization, authorization::Bearer};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

use crate::error::AppError;

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    /// Numeric UNIX timestamp. A string here fails validation with a confusing message.
    pub exp: u64,
}

/// Derived once at startup and kept in `AppState`.
#[derive(Clone)]
pub struct Keys {
    encoding: EncodingKey,
    decoding: DecodingKey,
    ttl_secs: u64,
}

impl Keys {
    #[must_use]
    pub fn new(secret: &SecretString, ttl_secs: u64) -> Self {
        let bytes = secret.expose_secret().as_bytes();
        Self {
            encoding: EncodingKey::from_secret(bytes),
            decoding: DecodingKey::from_secret(bytes),
            ttl_secs,
        }
    }

    /// # Errors
    /// Fails only if the claims cannot be serialised.
    pub fn issue(&self, subject: &str) -> Result<String, jsonwebtoken::errors::Error> {
        let claims = Claims {
            sub: subject.to_owned(),
            exp: jsonwebtoken::get_current_timestamp() + self.ttl_secs,
        };
        encode(&Header::default(), &claims, &self.encoding)
    }
}

/// `Authorization: Bearer <jwt>` to verified claims. Add it as a handler argument;
/// routes without it stay public.
#[derive(Debug)]
pub struct AuthUser(pub Claims);

impl<S> FromRequestParts<S> for AuthUser
where
    // `FromRef` means this works with any state that can produce `Keys`.
    Keys: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let TypedHeader(Authorization(bearer)) = parts
            .extract::<TypedHeader<Authorization<Bearer>>>()
            .await
            .map_err(|_| AppError::Unauthorized)?;

        let keys = Keys::from_ref(state);
        // `Validation::new(HS256)` sets `validate_exp = true`, `leeway = 60` s and
        // `required_spec_claims = {"exp"}`; `Validation::default()` is the same
        // thing. The trap is `validate_aud`, which is `true` with `aud: None`.
        let validation = Validation::new(Algorithm::HS256);
        let data = decode::<Claims>(bearer.token(), &keys.decoding, &validation)
            .map_err(|_| AppError::Unauthorized)?;

        Ok(Self(data.claims))
    }
}
```

Wire it with `impl FromRef<AppState> for Keys` and document the route with
`security(("bearer" = []))`. Never reveal which half failed: an expired token, a bad signature
and a missing header are all `AppError::Unauthorized`.

`Validation` also carries `leeway`, `validate_nbf`, `aud`, `iss`, `required_spec_claims` and, new
in 11, `reject_tokens_expiring_in_less_than`. `validate_aud` defaults to `true` with `aud: None`,
so any token carrying an `aud` claim is rejected until you call `set_audience` (or set
`validate_aud = false`). For an external identity provider, fetch the JWKS, build the key with
`DecodingKey::from_jwk`, and cache it — refetching per request adds a network round trip to every
call.

`Claims { sub, exp }` is the minimum for a first-party HS256 token. There is no `iat` for audit,
no `nbf`, and no `jti`, so a token cannot be revoked before it expires; keep the TTL short
(`Keys::new(&settings.auth.jwt_secret, ttl_secs)`, with `ttl_secs` added to `AuthSettings`) and
add `jti` plus a denylist only when revocation becomes a requirement.

## Password hashing with argon2 0.6

argon2 0.6 rewrote the API. `SaltString` is gone from `password-hash`, the salt is generated
internally, and verification takes the PHC string directly.

```rust,ignore
// Dead on 0.6: SaltString moved out of password-hash and the signature changed.
let salt = SaltString::generate(&mut OsRng);
let hash = Argon2::default().hash_password(pw.as_bytes(), &salt)?.to_string();
```

```rust,verify
//! argon2 0.6: `hash_password` generates the salt, `verify_password` takes the PHC string.
use argon2::{Argon2, PasswordHasher, PasswordVerifier};

/// # Errors
/// Fails on an argon2 parameter or internal error.
pub fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    Ok(Argon2::default()
        .hash_password(password.as_bytes())?
        .to_string())
}

/// # Errors
/// Returns an error when the password does not match the hash.
pub fn verify_password(password: &str, phc: &str) -> Result<(), argon2::password_hash::Error> {
    // `PasswordVerifier<str>` compares against the stored PHC string.
    Argon2::default().verify_password(password.as_bytes(), phc)
}

/// Both calls burn roughly 100 ms of CPU, which stalls the worker thread they run
/// on. In a handler, move them off the async runtime.
///
/// # Errors
/// Propagates the hashing error; panics inside the closure surface as a join error.
pub async fn hash_password_off_thread(password: String) -> anyhow::Result<String> {
    let phc = tokio::task::spawn_blocking(move || hash_password(&password)).await??;
    Ok(phc)
}
```

Store the PHC string as-is: it carries the algorithm, parameters and salt, so a later parameter
change still verifies old hashes. Never log it, and never put it in a response DTO — that is the
column the "return a DTO, not the entity" rule exists for.
