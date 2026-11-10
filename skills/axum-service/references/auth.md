# JWT authentication and password hashing

## Contents

- [Keys, claims and the AuthUser extractor](#keys-claims-and-the-authuser-extractor)
- [Guarding a subtree](#guarding-a-subtree)
- [Password hashing with argon2 0.6](#password-hashing-with-argon2-06)

Needs three crates beyond the base set: `axum-extra = { version = "0.12", features = ["typed-header"] }`
(0.10 is a year stale and does not declare axum 0.8), `jsonwebtoken = { version = "11", features =
["aws_lc_rs"] }` and, for passwords, `argon2 = "0.6"`. jsonwebtoken 11 needs exactly one crypto backend:
`aws_lc_rs` is already in the graph through rustls, while `rust_crypto` pulls `rsa`, which cargo-deny
rejects as RUSTSEC-2023-0071. With neither it compiles, then the first `encode` or `decode` panics with `Could
not automatically determine the process-level CryptoProvider`.

## Keys, claims and the AuthUser extractor

```rust,verify
//! `auth.rs` — key material in state, a `FromRequestParts` extractor per route.
use axum::extract::{FromRef, FromRequestParts, Request};
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::get;
use axum::{RequestPartsExt, Router};
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

/// HS256 wants a key at least as long as its 256-bit output (RFC 7518, 3.2).
/// jsonwebtoken signs with any length, so a short secret is refused here.
const MIN_SECRET_BYTES: usize = 32;

impl Keys {
    /// # Errors
    /// The secret is shorter than 32 bytes.
    pub fn new(secret: &SecretString, ttl_secs: u64) -> anyhow::Result<Self> {
        let bytes = secret.expose_secret().as_bytes();
        anyhow::ensure!(
            bytes.len() >= MIN_SECRET_BYTES,
            "the JWT secret must be at least {MIN_SECRET_BYTES} bytes for HS256"
        );
        Ok(Self {
            encoding: EncodingKey::from_secret(bytes),
            decoding: DecodingKey::from_secret(bytes),
            ttl_secs,
        })
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

/// Every route under the layer needs a valid token. The check is the extractor
/// itself, so a subtree is guarded by the same code as a single handler.
pub async fn require_user(_user: AuthUser, request: Request, next: Next) -> Response {
    next.run(request).await
}

pub fn admin_routes<S>(state: S) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    Keys: FromRef<S>,
{
    Router::new()
        .route("/admin/stats", get(|| async { "ok" }))
        // `route_layer`: these routes only, and a path that matches none of
        // them stays a 404 instead of becoming a 401.
        .route_layer(axum::middleware::from_fn_with_state(state, require_user))
}
```

Wire it with a `keys: Keys` field in `AppState` and `impl FromRef<AppState> for Keys`, and
document the route with `security(("bearer" = []))`. Never reveal which half failed: an expired
token, a bad signature and a missing header are all `AppError::Unauthorized`.

`Keys::new` runs once in `main.rs`, as `Keys::new(&settings.auth.jwt_secret, ttl_secs)?`, so a
secret under 32 bytes stops the process at startup instead of signing tokens that can be
brute-forced. The template's placeholder secrets (`.env.example`, `test_app()`) are long enough;
in a project whose placeholders are shorter, lengthen them in the change that adds `Keys` to
`AppState`.

`Validation` also carries `leeway`, `validate_nbf`, `aud`, `iss`, `required_spec_claims` and
`reject_tokens_expiring_in_less_than`. `validate_aud` defaults to `true` with `aud: None`,
so any token carrying an `aud` claim is rejected until you call `set_audience` (or set
`validate_aud = false`). For an external identity provider, fetch the JWKS, build the key with
`DecodingKey::from_jwk`, and cache it — refetching per request adds a network round trip to every
call.

`Claims { sub, exp }` is the minimum for a first-party HS256 token. There is no `iat` for audit,
no `nbf`, and no `jti`, so a token cannot be revoked before it expires; keep the TTL short
(`ttl_secs` is a field added to `AuthSettings`) and
add `jti` plus a denylist only when revocation becomes a requirement.

## Guarding a subtree

`require_user` and `admin_routes`, at the end of the `auth.rs` block above, are the whole pattern:
a `from_fn` middleware whose first argument is `AuthUser`, applied with `route_layer`. The extractor verifies
the signature and the expiry, and its rejection is the same 401 `ErrorBody` a handler would give.
A guard that inspects the header itself, checking that it exists or starts with `Bearer `, accepts
any string a client makes up; never write one. A handler that needs the claims still takes
`AuthUser` as an argument: verifying twice is cheap, and the handler stays correct if the layer
is ever removed.

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
use argon2::password_hash::Error;
use argon2::{Argon2, PasswordHasher, PasswordVerifier};

/// # Errors
/// Fails on an argon2 parameter or internal error.
pub fn hash_password(password: &str) -> Result<String, Error> {
    Ok(Argon2::default()
        .hash_password(password.as_bytes())?
        .to_string())
}

/// # Errors
/// Returns `Error::PasswordInvalid` when the password does not match the hash.
pub fn verify_password(password: &str, phc: &str) -> Result<(), Error> {
    // `PasswordVerifier<str>` compares against the stored PHC string.
    Argon2::default().verify_password(password.as_bytes(), phc)
}

/// argon2 is slow on purpose, and all of that time is CPU on the thread that
/// runs it. In a handler, hash and verify on the blocking pool, or every other
/// request on that worker thread waits.
///
/// # Errors
/// Propagates the hashing error; a panic inside the closure surfaces as a join error.
pub async fn hash_password_off_thread(password: String) -> anyhow::Result<String> {
    let phc = tokio::task::spawn_blocking(move || hash_password(&password)).await??;
    Ok(phc)
}

/// `Ok(false)` is a wrong password, which the login handler answers with
/// `AppError::Unauthorized`.
///
/// # Errors
/// The stored hash is malformed, or the blocking task panicked: both are a 500.
pub async fn verify_password_off_thread(password: String, phc: String) -> anyhow::Result<bool> {
    let outcome =
        tokio::task::spawn_blocking(move || verify_password(&password, &phc)).await?;
    match outcome {
        Ok(()) => Ok(true),
        Err(Error::PasswordInvalid) => Ok(false),
        Err(err) => Err(err.into()),
    }
}
```

Store the PHC string as-is: it carries the algorithm, parameters and salt, so a later parameter
change still verifies old hashes. Never log it, and never put it in a response DTO — that is the
column the "return a DTO, not the entity" rule exists for.
