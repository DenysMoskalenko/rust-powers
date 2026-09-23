---
name: rust-redis
description: "Use when caching or coordinating through Redis in an axum service with redis-rs — ConnectionManager setup, cache-aside and TTLs, serde values, invalidation, SCAN over KEYS, pipelines, Lua scripts, distributed locks and leases, rate limiting as an axum layer, idempotency keys, testcontainers Redis. Also for Connection refused, NOSCRIPT, WRONGTYPE, the trait bound FromRedisValue is not satisfied, multiple applicable items in scope. Not for pub/sub, queues or events (rust-nats), nor transactional exclusion with advisory locks (sea-orm-postgres)."
metadata:
  version: "0.2.0"
---

# Redis with redis-rs

Assumes Rust 1.98 edition 2024, tokio, axum 0.8, redis 1.7 with the `tokio-comp`,
`tokio-rustls-comp`, `connection-manager` and `script` features, testcontainers-modules 0.15.

Names such as `AppError`, `test_app()`, `Valid<T>` and the Makefile targets come from the rust-scaffolding template. In a project built differently, use its own types, helpers and tooling, map outcomes onto its nearest existing error variant, and say so when none fits instead of adding one. Apply these rules to new code; when editing existing code, keep its public contract and tuned configuration and report differences instead of rewriting, unless asked. If `Cargo.lock` pins another major or minor version than the line above, follow the project and say which rules may not apply.

## Important

- A cache read never becomes a 5xx: reads return `Option<T>`, a miss and an unreachable Redis
  are both `None`. `AppError` gets no `#[from] redis::RedisError` and no new variant; the few
  commands a request cannot survive without go through `required(e, "what")`.
- One `aio::ConnectionManager` in `AppState` by value, cloned per call, with
  `set_number_of_retries(0)` and explicit timeouts. Never `Mutex`, `Arc`, a pool, or a `Client`.
- Every write carries a TTL. Keys and values never reach a span or a log.
- Work whose effect is a write to this
  Postgres database takes an advisory lock inside that transaction (`pg_try_advisory_xact_lock` to
  run once, `pg_advisory_xact_lock` to serialise; `sea-orm-postgres`); anything else — an outbound
  call, a file, work spanning services or outliving one transaction — takes a Redis lease,
  which only suppresses duplicates, so that effect must itself be idempotent.

## References

- `references/caching.md` — read when caching beyond the core pattern: connection setup and settings,
  command traits, the `Json<T>` newtype, keys and TTLs, the full cache-aside module, `SCAN`,
  pipelines, Lua, error classification, the readiness entry, the one case for a pool, the
  cheat sheet.
- `references/locks-and-limits.md` — read when coordinating rather than caching: the lease,
  both rate limiters and the layer function, who gets limited and fail-open, idempotency keys,
  token revocation.
- `references/testing-redis.md` — read when setting up Redis tests: the container and its tag,
  the reuse race, key-prefix isolation, the fixture, eviction policy, what is worth asserting.

## Setup

Redis is a cache and a coordination point, never a source of truth; pub/sub, queues and streams
are `rust-nats`'s.

```toml
redis = { version = "1.7", features = ["tokio-comp", "tokio-rustls-comp", "connection-manager", "script"] }
# deadpool-redis = "0.23"   # only for a module that issues blocking commands
```

The `json` feature is the RedisJSON *server module*, not serde support — caching a struct needs
no feature. `tokio-rustls-comp` is what makes `rediss://` work. Settings are a `RedisSettings`
section read as `APP__REDIS__URL` (timeouts 500 ms / 100 ms, a cache-only `enabled` kill switch:
leases and limiters ignore it).

## The core pattern

Cache-aside over one connection; the reference adds `Json<T>`, invalidation, jitter, counters.

```rust,verify
//! A miss, a bad payload and an unreachable Redis all read as `None`; the
//! loader runs; the write is best effort and always carries a TTL.
use std::{future::Future, time::Duration};

use redis::{AsyncCommands, aio::ConnectionManager};
use serde::{Serialize, de::DeserializeOwned};

use crate::error::AppError;

#[derive(Clone)]
pub struct Cache {
    conn: ConnectionManager,
}

impl Cache {
    /// `skip_all`: the key is user data and must not land in the span.
    #[tracing::instrument(skip_all, fields(db.system = "redis", db.operation = "GET"))]
    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        match self.conn.clone().get::<_, Option<Vec<u8>>>(key).await {
            Ok(Some(bytes)) => serde_json::from_slice(&bytes).ok(),
            Ok(None) => None,
            Err(e) => {
                metrics::counter!("cache_errors_total").increment(1);
                tracing::warn!(error = %e, "cache read failed; degrading to a miss");
                None
            }
        }
    }

    /// Best effort: the caller already holds the value.
    #[tracing::instrument(skip_all, fields(db.system = "redis", db.operation = "SETEX"))]
    pub async fn set<T: Serialize + Sync>(&self, key: &str, value: &T, ttl: Duration) {
        let Ok(bytes) = serde_json::to_vec(value) else { return };
        let ttl = ttl.as_secs().max(1);
        if let Err(e) = self.conn.clone().set_ex::<_, _, ()>(key, bytes, ttl).await {
            tracing::warn!(error = %e, "cache write failed");
        }
    }

    /// `Option<T>` on purpose: `None` round-trips as JSON `null` — negative caching for free.
    pub async fn get_or_set<T, F, Fut>(&self, key: &str, ttl: Duration, load: F) -> Result<Option<T>, AppError>
    where
        T: Serialize + DeserializeOwned + Sync,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Option<T>, AppError>>,
    {
        if let Some(hit) = self.get::<Option<T>>(key).await {
            return Ok(hit);
        }
        let value = load().await?;
        self.set(key, &value, ttl).await;
        Ok(value)
    }
}

/// Only for a command the request cannot survive without: a lease that must be
/// held, an idempotency claim, a revocation check. Canonical variants only.
pub fn required(e: redis::RedisError, dependency: &str) -> AppError {
    if e.is_timeout() || e.is_connection_dropped() || e.is_connection_refusal() || e.is_io_error() {
        AppError::Unavailable(dependency.to_owned()) // 503; the name reaches the log only
    } else {
        AppError::Other(e.into()) // 500: WRONGTYPE, NOSCRIPT, a parse failure — a bug
    }
}
```

In a handler: `cache.get_or_set(&key, ttl, || users::find(&db, id)).await?.ok_or_else(||
AppError::NotFound(format!("user {id} not found")))?`; delete the key after an update commits.

## One connection for the process

`redis::Client` is a parsed URL, not a connection. `aio::ConnectionManager` is: it multiplexes
every caller over one socket and reconnects itself. Commands take `&mut self` and the manager is
`Clone` over an `Arc`, so every call site clones it. A pool of multiplexed connections pools
nothing — except for a module issuing blocking commands (`BLPOP` stalls every other caller),
which alone gets a small `deadpool-redis` pool.

`get_connection_manager_with_config` connects eagerly, so a Redis down at boot fails the boot.
Redis as an optional cache takes `get_connection_manager_lazy(config)`, which dials on first use:
the service boots degraded instead of crash-looping. Set both timeouts explicitly (the defaults
are too slow for a request path) and `set_number_of_retries(0)`: the reconnect budget is paid
*inside* whichever command finds the socket dead, so six retries cost every call seconds during
an outage; zero costs one connect timeout and still reconnects on the next call.

## Commands, values, keys

`AsyncCommands` and `AsyncTypedCommands` define the same method names, so importing both makes
every call `E0034: multiple applicable items in scope`; use the generic `AsyncCommands` wherever
`Option<T>` or a serde value is involved. Discard a reply with
`conn.set_ex::<_, _, ()>(key, value, ttl).await?`; `let _ = conn.set_ex(..)` builds a future and
drops it without sending a command.

Keys are `app:v1:entity:id`, built by one function; bump the version when a cached shape changes
and old entries age out. Every write carries a TTL — `set_ex`, or `SetExpiry` on `set_options` —
jittered by a few percent so a batch does not expire together. Invalidate by deleting the key
after the write commits, never by writing the new value: two writers that both delete cannot
leave a stale entry.

## Errors and readiness

`is_timeout()`, `is_connection_dropped()`, `is_connection_refusal()` and `is_io_error()` mean
the server is not answering: warn, count, degrade to a miss. Anything else is a bug, and
`err.code()` names it:

| Symptom | Meaning | Fix |
|---|---|---|
| `Connection refused` on a `redis://` URL | eager connect at boot, or the wrong host in `APP__REDIS__URL` | fix the URL; `get_connection_manager_lazy` to boot degraded |
| `NOSCRIPT` | a raw `EVALSHA` after a restart | `redis::Script::invoke_async` loads and retries by itself |
| `WRONGTYPE` | two key families share a prefix | a versioned key builder per entity |

Readiness is `axum-service`'s: `/health/ready` returns `{ "status", "checks": { .. } }` and the
status rule. This skill adds one entry, `"cache"`, from one `PING`: `Health::Ok`, else
`Health::Degraded` — an optional dependency, so the probe stays 200. Adding it changes the body
`tests/health.rs` asserts.

## SCAN, pipelines, scripts

`KEYS pattern` blocks the whole server; use `scan_options` with a `MATCH` pattern, drained with
`next_item().await`. A pipeline is one round trip for N commands; `.atomic()` wraps them in
`MULTI`/`EXEC`. Read-modify-write belongs in a Lua script — `MULTI` cannot branch on a value it
read and `WATCH` needs a connection nobody else touches — built once in a `LazyLock`, called
with `invoke_async`, keys through `.key(..)`.

## Leases, limits, idempotency

Coordination state, not cache: a `noeviction` Redis, never `allkeys-lru`.

A lease is `SET key token NX PX ttl` plus a Lua compare-and-delete to release (Redis ≥ 8.4:
`del_ex` with `ValueComparison::ifeq`); a plain `DEL` frees somebody else's lock whenever the
holder stalled past the TTL. A job that cannot acquire — held, or Redis down — skips this run
(info log, counter), never crashes; a failed body releases anyway and the next run retries.

A rate limiter counts per verified caller: the `AuthUser` subject, else the client address — the
`X-Forwarded-For` entry your own ingress appended (counted from the right, one per proxy of yours
that appends to `X-Forwarded-For`), or the `ConnectInfo` peer when nothing proxies. Entries left of
it were written by the client; neither they, an unverified `x-api-key`, nor one shared `anonymous`
bucket is ever the key. It returns `AppError::TooManyRequests { retry_after_secs }` and fails open
on a Redis error, with a counter; it fails closed only where the limit guards what cannot absorb the
traffic. Its slot in the stack and `into_make_service_with_connect_info` are `axum-service`'s.

Idempotency implements `axum-service`'s contract, keyed per caller
(`app:v1:idem:{subject}:{Idempotency-Key}`): with the raw header as the key, two clients sending
the same value get each other's stored response. The claim is `SET key marker NX EX 30`, a TTL
never below the request timeout, or a slow first attempt is re-run while it runs; `store` writes
fingerprint + response (never the body) with its own 24 h `EX`. Redis dedups retries and replays
the answer; it does not make the effect atomic with the record, so the effect must be an outbound
call carrying the same key, never a bare database write. For a create, the unique index is the
idempotency key. A revoked token is one key per `jti`, TTL its lifetime.

## Observability and testing

`#[instrument(skip_all)]` on every cache and lock method: spans carry `db.system = "redis"` and
`db.operation`, never the key or the value; `skip(self)` still records the key. Count
`cache_hits_total`, `cache_misses_total`, `cache_errors_total`, by entity if at all, never by key.

Tests run against a real server: `TEST_REDIS_URL` from compose or CI (`rust-tooling`'s
optional-services block), or a testcontainers Redis pinned with `.with_tag("8-alpine")` (the
module default is `5.0`). Isolate with a per-test key prefix, not `FLUSHDB`.

## redis 0.2x is not 1.x

Most Redis material for Rust predates 1.0; these renames break it:

| 0.2x | 1.x |
|---|---|
| `Client::get_async_connection()` | gone; `get_multiplexed_async_connection()` or a `ConnectionManager` |
| `Commands` / `AsyncCommands` only | plus `TypedCommands` / `AsyncTypedCommands` |
| `from_redis_value(v: &Value) -> RedisResult<Self>` | `from_redis_value(v: Value) -> Result<Self, ParsingError>` |
| `set` / `set_ex` took `ToRedisArgs` | they take `ToSingleRedisArg` |
| `redis::transaction` closure (sync only) | `Pipeline::atomic()` with `query_async` |

## Red Flags — STOP

| About to… | Rule |
|---|---|
| Call `KEYS` in application code | It blocks the whole server; `SCAN` with `MATCH` |
| Release a lease with `DEL` | After a stall the key is the next holder's; Lua compare-and-delete |
| Reach for `WATCH` on a `ConnectionManager` | Multiplexing arms and disarms it unpredictably; use a Lua script |
| Add `#[from] redis::RedisError` or a `Cache` variant to `AppError` | Every `?` becomes a 500; map explicitly with `required` onto `Unavailable`/`Other` |
| Store an idempotency record after a database effect, outside its transaction | The claim expires and the retry repeats the write; write the row in the same transaction (`sea-orm-postgres`) |
| Put leases or idempotency keys on an `allkeys-lru` Redis | They get evicted under pressure; `noeviction` or a separate instance |
| Key a limiter on `x-api-key`, the first `X-Forwarded-For` entry, the `ConnectInfo` peer behind an ingress, or an `anonymous` bucket | Chosen by the client, or everyone shares one bucket; user id, else the address your ingress appended |
| Use the raw `Idempotency-Key` header as the Redis key | Two clients with the same key share a response; prefix it with the caller |
| Call `SystemTime::now()` in the limiter | Time is the injected `Clock`; the window bucket takes `now` from it |
