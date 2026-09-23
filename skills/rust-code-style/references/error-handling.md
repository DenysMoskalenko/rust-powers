# Error Handling

Domain error types and the rules around them. The service-wide `AppError`, its `IntoResponse` impl and the status-code table belong to `axum-service`; this file stops at the point where a domain error is handed over.

## Contents

- [thiserror or anyhow](#thiserror-or-anyhow)
- [One enum per domain module](#one-enum-per-domain-module)
- [Which attribute](#which-attribute)
- [Context with `?`](#context-with-)
- [Never log and return](#never-log-and-return)
- [Panics](#panics)

## thiserror or anyhow

`thiserror` when a caller branches on the error — a domain enum with one variant per failure a caller can do something about. `anyhow` when nothing matches on it and it only ever gets logged: internal plumbing, startup, one-off IO.

The module rule: one `thiserror` enum per domain module when callers match on variants or the module has more than one failure mode (a template that is missing *and* a database that fails); a single-variant wrapper is not worth an enum — propagate that one failure as `AppError` directly.

In production code `Box<dyn Error>` and `Result<T, String>` are banned; a test double may return either. Both throw away the source chain, so the log shows a single line instead of the cause, and neither can be matched on.

## One enum per domain module

A single flat error type with thirty variants is the same dumping ground as a `utils` module. Keep the domain error next to the domain, and let it reach the service error through `#[from]`.

```rust,verify
use chrono::{DateTime, Utc};
use crate::error::AppError;
use uuid::Uuid;

/// Failures callers of the orders module can act on.
#[derive(Debug, thiserror::Error)]
pub enum OrderError {
    #[error("order {0} not found")]
    NotFound(Uuid),
    #[error("insufficient stock for sku {sku}: wanted {wanted}, have {have}")]
    InsufficientStock { sku: String, wanted: u32, have: u32 },
    #[error("order {id} already shipped at {at}")]
    AlreadyShipped { id: Uuid, at: DateTime<Utc> },
    #[error(transparent)]
    Db(#[from] sea_orm::DbErr),
}

impl From<OrderError> for AppError {
    fn from(error: OrderError) -> Self {
        match error {
            OrderError::NotFound(id) => Self::NotFound(format!("order {id}")),
            OrderError::Db(err) => Self::Db(err),
            OrderError::InsufficientStock { .. } | OrderError::AlreadyShipped { .. } => {
                Self::Conflict(error.to_string())
            }
        }
    }
}
```

Every variant either names the concrete thing that failed, with the ids a reader needs to find it, or is `#[error(transparent)]`. `#[error("error")]` and `#[error("operation failed")]` are worse than no error type at all.

```rust,ignore
// Bad: stringly typed, status decided at the call site, cause discarded.
async fn place_order(id: Uuid) -> Result<Json<Order>, (StatusCode, String)> {
    let order = repo.find(id).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(order))
}
```

## Which attribute

| Attribute | Use when |
|---|---|
| `#[from]` | The variant wraps exactly one foreign error and adds nothing. It generates `From`, so `?` just works. One `#[from]` per source type per enum. |
| `#[source]` | The cause belongs in the chain but the variant also carries its own fields, or you write the conversion yourself. |
| `#[error(transparent)]` | The variant *is* the inner error — it delegates both `Display` and `source()`. Use it for the `anyhow` catch-all and pure pass-through wrappers, never with a message of your own. |

A variant that carries context (`InsufficientStock { sku, wanted, have }`) is worth far more at 3am than one that wraps a string. Prefer fields over formatting the context into the message.

## Context with `?`

`context` for a static string, `with_context` for anything that allocates, because the closure only runs on the error path.

```rust,verify
use anyhow::Context as _;
use std::path::Path;

/// Context names what the program was trying to do, not what went wrong —
/// the error already says that.
pub async fn load_seed(path: &Path) -> anyhow::Result<String> {
    tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("reading seed file {}", path.display()))
}
```

Add context once per meaningful operation boundary. Context at every `?` in a call chain produces five-line reports that say the same thing five ways.

## Never log and return

```rust,ignore
// Bad: one failure, two log lines, two severities, two places to change.
let user = repo.find(id).await.map_err(|e| { tracing::error!("db: {e}"); e })?;
```

Log where the response is decided, or where the error is swallowed. Nowhere else. In this stack that means the `IntoResponse` impl logs the chain once for 5xx, and service code just propagates.

Swallowing is a deliberate act and looks like one: `if let Err(error) = cleanup().await { tracing::warn!(?error, "cleanup failed"); }`. A bare `let _ = fallible();` hides a decision nobody made.

## Panics

Outside tests, clippy's `unwrap_used` and `expect_used` fire on every `unwrap()` and `expect()`, whatever the message says, and CI runs clippy with `-D warnings`. Return the error with context instead. Where failure is impossible, the item carries the proof in an `#[expect]`, and its reason reads as one: "the pattern is a compile-time constant", not "failed to compile regex".

```rust,verify
use std::sync::LazyLock;

use reqwest::Url;

#[expect(clippy::expect_used, reason = "a constant URL literal always parses")]
static STATUS_PAGE: LazyLock<Url> =
    LazyLock::new(|| Url::parse("https://status.example.com/").expect("constant URL literal"));
```

A poisoned `Mutex` is not a proof of anything, so `lock().expect("cache poisoned")` earns no such attribute: poisoning only means another thread panicked while holding the guard. `CatchPanicLayer` keeps the process alive after that panic, so an `unwrap()` or `expect()` on the lock makes one panic a permanent 500 for every later request. Recover it — `lock().unwrap_or_else(std::sync::PoisonError::into_inner)` — where the critical section is short enough that it cannot have left the data half-updated (a single assignment, a counter, a whole value replaced). Where it could, recovery restores the guard, not the invariant: rebuild the value or treat it as fatal.

`todo!` and `unimplemented!` are for a work-in-progress buffer, never a commit. `unreachable!` is acceptable only with a comment proving it, and a type change is usually the better fix.
