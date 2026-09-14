# Locks, rate limits and idempotency

- [A lease is not a mutex](#a-lease-is-not-a-mutex)
- [The lease](#the-lease)
- [required without a cache module](#required-without-a-cache-module)
- [Rate limiting](#rate-limiting)
- [Who gets limited, and what happens when Redis is down](#who-gets-limited-and-what-happens-when-redis-is-down)
- [Idempotency keys](#idempotency-keys)
- [Token blacklist](#token-blacklist)

Everything in this file is coordination state, not cache: a lease key that gets evicted means
two holders, an evicted idempotency marker means a duplicate payment, an evicted limiter bucket
means a reset limit. It lives on a Redis with `maxmemory-policy noeviction` (the server default),
or on its own instance; `allkeys-lru` is for a Redis that only caches. When a write is refused
with `OOM`, that is an outage to surface, not a miss to swallow.

## A lease is not a mutex

`SET key token NX PX ttl` gives at most one holder *probably*, never at most one holder
*certainly*: a garbage-collection pause or a network stall longer than the TTL means two holders
and no way for either to notice. That is fine for suppressing duplicate work - one cron runner,
one cache refill, one webhook consumer - and not fine for anything whose correctness depends on
exclusion. When money or a uniqueness invariant is at stake, take `pg_advisory_xact_lock` inside
the transaction that does the write, since it dies with the session and has no lease to expire;
for that see `sea-orm-postgres`.

Releasing with a plain `DEL` is the classic bug: if the holder stalled past the TTL, the key now
belongs to somebody else and `DEL` frees *their* lock. Compare the token and delete in one Lua
script, where the server's single-threaded execution guarantees nothing slips in between. (Redis
8.4 has this natively as `del_ex(key, ValueComparison::ifeq(token))` - `DELEX key IFEQ token`;
the script is what runs on 7.x, Valkey and managed offerings.)

## The lease

```rust,verify
//! src/lock.rs - a best-effort mutual-exclusion lease.

use std::{sync::LazyLock, time::Duration};

use redis::{AsyncCommands, ExistenceCheck, SetExpiry, SetOptions, aio::ConnectionManager};
use uuid::Uuid;

/// `GET` then `DEL` in one round trip: nothing can run between the compare and
/// the delete, so a lease that expired and was retaken is never freed by its
/// previous holder.
static RELEASE: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"if redis.call('GET', KEYS[1]) == ARGV[1] then
              return redis.call('DEL', KEYS[1])
          else
              return 0
          end",
    )
});

/// Extend only if still ours, for the same reason.
static RENEW: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"if redis.call('GET', KEYS[1]) == ARGV[1] then
              return redis.call('PEXPIRE', KEYS[1], ARGV[2])
          else
              return 0
          end",
    )
});

#[derive(Debug)]
pub struct Lease {
    key: String,
    token: String,
    conn: ConnectionManager,
}

/// `Ok(None)` means somebody else holds it. Contention is an outcome, not an error.
#[tracing::instrument(skip_all, fields(db.system = "redis", db.operation = "SET"))]
pub async fn acquire(
    conn: &ConnectionManager,
    key: impl Into<String>,
    ttl: Duration,
) -> Result<Option<Lease>, redis::RedisError> {
    let key = key.into();
    let token = Uuid::now_v7().to_string();
    let options = SetOptions::default()
        .conditional_set(ExistenceCheck::NX)
        .with_expiration(SetExpiry::PX(
            u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX),
        ));
    // Redis replies +OK or Null, so `Option<String>` is the honest return type.
    let set: Option<String> = conn.clone().set_options(&key, &token, options).await?;
    Ok(set.map(|_| Lease {
        key,
        token,
        conn: conn.clone(),
    }))
}

impl Lease {
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// `true` if this call released it, `false` if the lease had already expired
    /// and been taken by somebody else - which means the work ran twice and is
    /// worth a log line.
    ///
    /// Releasing is explicit because `Drop` cannot await: there is no RAII
    /// version, so release on both the success and the error path.
    #[tracing::instrument(skip_all, fields(db.system = "redis", db.operation = "EVALSHA"))]
    pub async fn release(mut self) -> Result<bool, redis::RedisError> {
        let deleted: i64 = RELEASE
            .key(&self.key)
            .arg(&self.token)
            .invoke_async(&mut self.conn)
            .await?;
        Ok(deleted == 1)
    }

    #[tracing::instrument(skip_all, fields(db.system = "redis", db.operation = "EVALSHA"))]
    pub async fn renew(&mut self, ttl: Duration) -> Result<bool, redis::RedisError> {
        let ttl_ms = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX);
        let ok: i64 = RENEW
            .key(&self.key)
            .arg(&self.token)
            .arg(ttl_ms)
            .invoke_async(&mut self.conn)
            .await?;
        Ok(ok == 1)
    }

    /// For the end of a `with_lease` body: the work is done and nothing about
    /// the release can undo it, so a failed release is a warning, never an
    /// error - an error would make the caller retry the body, the exact
    /// duplicate the lease exists to prevent. The key expires on its own.
    pub async fn release_or_warn(self) {
        let outcome = self.release().await;
        if !matches!(outcome, Ok(true)) {
            // `Ok(false)`: expired and retaken, the work may have run twice.
            // `Err`: Redis unreachable after the work; the key expires anyway.
            tracing::warn!(?outcome, "lease not released cleanly");
        }
    }
}

/// Runs `body` only if the lease is free, and releases it afterwards whatever
/// `body` returned: a failed run gives the lease up so the next scheduled run,
/// on any replica, retries — holding it until the TTL would only delay that.
/// `Ok(None)` means somebody else is already doing the work.
pub async fn with_lease<T, F, Fut>(
    conn: &ConnectionManager,
    key: impl Into<String>,
    ttl: Duration,
    body: F,
) -> Result<Option<T>, redis::RedisError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    let Some(lease) = acquire(conn, key, ttl).await? else {
        return Ok(None);
    };
    let out = body().await;
    lease.release_or_warn().await;
    Ok(Some(out))
}

/// A scheduled job: every way of not getting the lease means "not this run",
/// logged and counted, never a crash and never a `?` out of the job. The
/// `enabled` kill switch in `RedisSettings` is the cache's; a lease ignores it,
/// because "disabled" would mean every replica runs the job.
pub async fn nightly_reindex(conn: &ConnectionManager) {
    let lease = with_lease(conn, "app:v1:lock:reindex", Duration::from_mins(10), reindex).await;
    let reason = match lease {
        Ok(Some(result)) => return report(result),
        Ok(None) => "another replica holds the lease".to_owned(),
        Err(err) => {
            metrics::counter!("lock_errors_total").increment(1);
            format!("lock unreachable: {err}")
        }
    };
    // Info, never an error: contention is the normal case with several replicas.
    tracing::info!(%reason, "reindex skipped");
}

/// The body ran and the lease is already released, so a failure is the next
/// scheduled run's to retry.
fn report(result: anyhow::Result<()>) {
    if let Err(err) = result {
        tracing::error!(%err, "reindex failed; the next run retries");
    }
}

/// The work itself; a stand-in here.
async fn reindex() -> anyhow::Result<()> {
    Ok(())
}
```

Size the TTL above the worst-case runtime of the body, not the average, and call `renew` from a
long job rather than taking a TTL of an hour: a lease held by a crashed process is unavailable
until it expires.

Who called decides what an acquire failure means. A job skips the run — contention is the normal
case with several replicas, and an unreachable Redis is one missed run, not an outage — and the
scheduler's next tick retries. A body that failed is released, not held: the lease suppresses
*concurrent* duplicates, and the retry the next tick gives is the recovery. Only a *request* that
genuinely cannot proceed without the lease maps its acquire error with `required(e, "lock")` —
a 503 when Redis is unreachable — never with `?`.

## required without a cache module

`required` and `is_unavailable` are generic Redis error classification, not cache logic; they sit
in `cache.rs` because that is where most services first need them. A lock-only or limiter-only
service has no `cache.rs`, so give them their own file and let every Redis module import from it
(`src/redis_errors.rs`, not `src/redis/errors.rs`: a module named `redis` makes every
`use redis::..` in `lib.rs` ambiguous, E0659):

```rust,verify
//! `src/redis_errors.rs` - shared by `cache.rs`, `lock.rs`, `ratelimit.rs`, `idempotency.rs`.
use crate::error::AppError;

#[must_use]
pub fn is_unavailable(e: &redis::RedisError) -> bool {
    e.is_timeout() || e.is_connection_dropped() || e.is_connection_refusal() || e.is_io_error()
}

/// For the few commands whose failure a request cannot survive - a lease that
/// must be held, an idempotency claim, a token-revocation check. Explicit, never
/// `#[from]`: a blanket `From<RedisError>` would make every `?` on a Redis call
/// a 500.
#[must_use]
pub fn required(e: redis::RedisError, dependency: &str) -> AppError {
    if is_unavailable(&e) {
        AppError::Unavailable(dependency.to_owned()) // 503; the name reaches the log only
    } else {
        AppError::Other(e.into()) // 500: WRONGTYPE, NOSCRIPT, a parse failure - a bug
    }
}
```

## Rate limiting

Two algorithms, and the choice is about state, not accuracy:

| | Fixed window | Sliding window |
|---|---|---|
| State per caller | one integer | one sorted-set member per request in the window |
| Round trips | one pipeline | one script |
| Worst case | 2x the limit across a boundary (measured: 10 of "5 per second" in 100 ms) | exact |
| Use when | the limit is a fairness guard | the limit is a contract with a paying customer |

```rust,verify
//! src/ratelimit.rs - both limiters plus the axum layer that applies one.

use std::{
    net::SocketAddr,
    sync::{Arc, LazyLock},
    time::Duration,
};

use axum::{
    RequestExt,
    extract::{ConnectInfo, FromRef, FromRequestParts, Request, State},
    middleware::Next,
    response::Response,
};
use redis::aio::ConnectionManager;

use crate::clock::Clock;
use crate::error::AppError;

#[derive(Debug, Clone, Copy)]
pub struct Decision {
    pub allowed: bool,
    pub remaining: i64,
    pub retry_after: Duration,
}

/// `INCR` then `EXPIRE`, pipelined and atomic. The `EXPIRE` runs on every call
/// because that is cheaper than a round trip to learn whether the key is new,
/// and it cannot extend the window: the bucket is part of the key. `now_secs`
/// comes from the injected `Clock`, so a test can sit on a window boundary.
#[tracing::instrument(skip_all, fields(db.system = "redis", db.operation = "INCR"))]
pub async fn fixed_window(
    conn: &ConnectionManager,
    key: &str,
    limit: i64,
    window: Duration,
    now_secs: u64,
) -> Result<Decision, redis::RedisError> {
    let secs = window.as_secs().max(1);
    let bucket = format!("{key}:{}", now_secs / secs);
    let mut conn = conn.clone();
    let (count,): (i64,) = redis::pipe()
        .atomic()
        .incr(&bucket, 1)
        .expire(&bucket, i64::try_from(secs).unwrap_or(i64::MAX))
        .ignore()
        .query_async(&mut conn)
        .await?;
    Ok(Decision {
        allowed: count <= limit,
        remaining: (limit - count).max(0),
        retry_after: Duration::from_secs(secs - (now_secs % secs)),
    })
}

/// Trim, count, maybe add - all inside one script, which is what makes this
/// read-modify-write safe without `WATCH`. The clock is the server's (`TIME`),
/// so replicas with skewed clocks share one window; the refusal reports when the
/// oldest member leaves the window rather than a whole window.
static SLIDING: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"local t      = redis.call('TIME')
          local now    = t[1] * 1000 + math.floor(t[2] / 1000)
          local window = tonumber(ARGV[1])
          local limit  = tonumber(ARGV[2])
          redis.call('ZREMRANGEBYSCORE', KEYS[1], 0, now - window)
          local used = redis.call('ZCARD', KEYS[1])
          if used < limit then
              redis.call('ZADD', KEYS[1], now, ARGV[3])
              redis.call('PEXPIRE', KEYS[1], window)
              return {limit - used - 1, 0}
          end
          local oldest = redis.call('ZRANGE', KEYS[1], 0, 0, 'WITHSCORES')
          return {-1, oldest[2] + window - now}",
    )
});

#[tracing::instrument(skip_all, fields(db.system = "redis", db.operation = "EVALSHA"))]
pub async fn sliding_window(
    conn: &ConnectionManager,
    key: &str,
    limit: i64,
    window: Duration,
) -> Result<Decision, redis::RedisError> {
    let mut conn = conn.clone();
    let (remaining, retry_ms): (i64, i64) = SLIDING
        .key(key)
        .arg(i64::try_from(window.as_millis()).unwrap_or(i64::MAX))
        .arg(limit)
        .arg(uuid::Uuid::now_v7().to_string())
        .invoke_async(&mut conn)
        .await?;
    Ok(Decision {
        allowed: remaining >= 0,
        remaining: remaining.max(0),
        retry_after: Duration::from_millis(u64::try_from(retry_ms).unwrap_or(0)),
    })
}

/// The layer's own state; `FromRef<AppState>` is the two-line impl that lets
/// the layer pull it out of the router state.
#[derive(Clone)]
pub struct Limiter {
    pub conn: ConnectionManager,
    pub limit: i64,
    pub window: Duration,
    /// The same `Arc<dyn Clock>` as `AppState.clock`; never `SystemTime::now()`.
    pub clock: Arc<dyn Clock>,
}

/// Applied with `from_fn_with_state(state.clone(), rate_limit::<AppState, AuthUser>)`
/// in the rate-limit slot that `axum-service`'s middleware reference defines
/// (it also has `main.rs` serve with `into_make_service_with_connect_info`, which
/// is what makes `ConnectInfo` exist here). `U` is the authenticated identity -
/// `axum-service`'s `AuthUser`, with
/// `impl AsRef<str> for AuthUser { fn as_ref(&self) -> &str { &self.0.sub } }`.
/// A request that does not carry a valid token is limited by peer address
/// instead, the one unauthenticated identity the client cannot choose; the
/// handler's own `AuthUser` argument still answers 401. What it counts belongs
/// here; where it sits does not.
pub async fn rate_limit<S, U>(
    State(state): State<S>,
    mut request: Request,
    next: Next,
) -> Result<Response, AppError>
where
    S: Clone + Send + Sync + 'static,
    Limiter: FromRef<S>,
    U: FromRequestParts<S> + AsRef<str> + 'static,
{
    let limiter = Limiter::from_ref(&state);
    let caller = match request.extract_parts_with_state::<U, S>(&state).await {
        Ok(user) => format!("user:{}", user.as_ref()),
        // Without `into_make_service_with_connect_info` in `main.rs` every
        // unauthenticated request fails loudly here rather than sharing one bucket.
        Err(_) => match request.extensions().get::<ConnectInfo<SocketAddr>>() {
            Some(ConnectInfo(peer)) => format!("ip:{}", peer.ip()),
            None => {
                return Err(AppError::Other(anyhow::anyhow!(
                    "rate_limit: no ConnectInfo; serve with into_make_service_with_connect_info"
                )));
            }
        },
    };
    let key = format!("app:v1:ratelimit:{caller}");
    let now_secs = u64::try_from(limiter.clock.now().timestamp()).unwrap_or(0);
    match fixed_window(&limiter.conn, &key, limiter.limit, limiter.window, now_secs).await {
        Ok(decision) if decision.allowed => Ok(next.run(request).await),
        Ok(decision) => Err(AppError::TooManyRequests {
            retry_after_secs: Some(
                u64::try_from(decision.retry_after.as_millis().div_ceil(1_000)).unwrap_or(u64::MAX),
            ),
        }),
        Err(e) => {
            // Fail open, and count it: Redis being down must not take the API
            // down. A limiter that guards something which cannot absorb the
            // traffic (a paid upstream, a login form) fails closed instead:
            // `Err(required(e, "rate limiter"))`, a 503 with a constant body.
            metrics::counter!("ratelimit_errors_total").increment(1);
            tracing::warn!(error = %e, "rate limiter unavailable, failing open");
            Ok(next.run(request).await)
        }
    }
}
```

## Who gets limited, and what happens when Redis is down

The key names the caller, and the caller has to be something the client cannot pick. An
authenticated subject (`AuthUser` above) is the right unit; the peer address is the fallback for
public routes, and behind a load balancer that means the address the proxy reports - the
*last* `X-Forwarded-For` entry, the one the trusted proxy appended, never the first, which the
client wrote. Two things never become the key: an unverified header such as `x-api-key`, which an
attacker rotates to get a fresh bucket and which then sits in plain text in every `SCAN`,
`MONITOR` and `SLOWLOG` output; and a shared `anonymous` bucket, where one client exhausts the
limit for every other unauthenticated caller. A per-route limit is the same key with the route
appended.

The layer above fails open on a Redis error, counted in `ratelimit_errors_total` so the outage is
visible even though no request sees it. That is right for a fairness limiter. Fail closed - a 503
through `required` - only where the limit protects something that genuinely cannot absorb the
traffic: an outbound paid API, a login endpoint. Decide per limiter and write the decision in a
comment, because the code looks the same either way.

## Idempotency keys

For a create, the idempotency key is the unique index: the duplicate insert fails with 23505 and
`AppError` already maps that to 409 - see `axum-service`. What follows is for a side effect with
no natural key (a payment, an outbound call), and it implements `axum-service`'s idempotency
contract exactly: `Idempotency-Key` header; fingerprint = hash of method,
path and body; same key and fingerprint replays the stored status and body; same key, different
fingerprint is 422; same key while the first attempt runs is 409.

The claim and the check have to be one command, or two concurrent retries both believe they are
first. `SET key marker NX EX ttl` is that command, with a *short* TTL: a claim never followed by a
`store` (the pod restarted mid-request) would otherwise block the key for the whole replay window.
That short TTL is a trade, not a free win - the ceiling below. The result then gets its own 24 h
`EX`. Store the fingerprint and the response, never the body: it is user data and can be large.

What this buys and what it does not. Redis dedups concurrent retries and replays the stored
response; it does not make the effect atomic with the record of it. A crash - or a handler slower
than the claim's TTL - between the effect and `store` lets the claim expire, and the next retry
gets `Claim::Fresh` and runs the effect a second time. So the effect itself has to tolerate that:
an outbound call that forwards the same key and is deduplicated by the provider, or an operation
that is safe to repeat. When the effect is a database write, do not use this at all - write the
idempotency row in the same transaction as the write, which is `sea-orm-postgres`'s. And `store`
is an unconditional `SETEX`: a stalled first attempt can overwrite a later attempt's entry,
harmless while both carry the same provider result, and the reason `IN_FLIGHT_TTL` is never
shorter than the request timeout.

```toml
sha2 = "0.11"   # add when needed; already in the lock file through sqlx-postgres, so nothing new compiles
```

```rust,verify
//! src/idempotency.rs - storage for the `Idempotency-Key` contract.

use std::{fmt::Write as _, time::Duration};

use redis::{AsyncCommands, ExistenceCheck, SetExpiry, SetOptions, aio::ConnectionManager};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// How long a claim without a `store` blocks retries; never below the request
/// timeout, or a slow first attempt is re-run while it is still running. Both
/// ends cost something: the 24 h result TTL would make one crash mid-request
/// block the key for a day, and this short TTL instead lets the retry after
/// that crash re-run the effect - which is why the effect must survive that.
pub const IN_FLIGHT_TTL: Duration = Duration::from_secs(30);
/// The replay window. 24 hours is the usual contract.
pub const RESULT_TTL: Duration = Duration::from_hours(24);

/// Enough to replay the request's answer, status and body alike.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredResponse {
    pub status: u16,
    pub body: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    fingerprint: String,
    /// `None` while the first attempt is still running.
    response: Option<StoredResponse>,
}

#[derive(Debug)]
pub enum Claim {
    /// This request owns the key: do the work, then call `store`.
    Fresh,
    /// Same key, same fingerprint, finished: replay it.
    Replay(StoredResponse),
    /// Same key, first attempt still running: 409, the client retries later.
    InFlight,
    /// Same key, a different request: 422.
    Mismatch,
}

/// What makes "the same request" checkable without storing the body. SHA-256
/// rather than `DefaultHasher`, which is not stable across toolchains, and a
/// replay may arrive after a deploy.
#[must_use]
pub fn fingerprint(method: &str, path: &str, body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    for part in [method.as_bytes(), b"\n", path.as_bytes(), b"\n", body] {
        hasher.update(part);
    }
    let mut hex = String::with_capacity(64);
    for byte in hasher.finalize() {
        let _ = write!(hex, "{byte:02x}"); // writing to a String cannot fail
    }
    hex
}

/// Claim and check in one command; two concurrent first attempts cannot both win.
#[tracing::instrument(skip_all, fields(db.system = "redis", db.operation = "SET"))]
pub async fn claim(
    conn: &ConnectionManager,
    key: &str,
    fingerprint: &str,
) -> Result<Claim, redis::RedisError> {
    let mut conn = conn.clone();
    let marker = encode(&Entry {
        fingerprint: fingerprint.to_owned(),
        response: None,
    })?;
    let options = SetOptions::default()
        .conditional_set(ExistenceCheck::NX)
        .with_expiration(SetExpiry::EX(IN_FLIGHT_TTL.as_secs()));
    if conn
        .set_options::<_, _, Option<String>>(key, marker, options)
        .await?
        .is_some()
    {
        return Ok(Claim::Fresh);
    }
    let Some(raw) = conn.get::<_, Option<String>>(key).await? else {
        // Expired between the two commands; the client's retry will claim it.
        return Ok(Claim::InFlight);
    };
    let entry: Entry = serde_json::from_str(&raw).map_err(|e| parse_error(&e))?;
    Ok(if entry.fingerprint != fingerprint {
        Claim::Mismatch
    } else if let Some(response) = entry.response {
        Claim::Replay(response)
    } else {
        Claim::InFlight
    })
}

/// Replace the marker with the response and start the replay window. An
/// explicit `EX`, not `KEEPTTL`: the marker's TTL was the short in-flight one.
#[tracing::instrument(skip_all, fields(db.system = "redis", db.operation = "SETEX"))]
pub async fn store(
    conn: &ConnectionManager,
    key: &str,
    fingerprint: &str,
    response: StoredResponse,
) -> Result<(), redis::RedisError> {
    let entry = encode(&Entry {
        fingerprint: fingerprint.to_owned(),
        response: Some(response),
    })?;
    conn.clone()
        .set_ex::<_, _, ()>(key, entry, RESULT_TTL.as_secs())
        .await
}

fn encode(entry: &Entry) -> Result<String, redis::RedisError> {
    serde_json::to_string(entry).map_err(|e| parse_error(&e))
}

fn parse_error(e: &serde_json::Error) -> redis::RedisError {
    redis::RedisError::from((redis::ErrorKind::Parse, "idempotency entry", e.to_string()))
}
```

The handler side, with the key scoped to the caller so one client's `Idempotency-Key: 1` cannot
collide with another's, and every storage error mapped through `required` (`cache::required`, or
`redis_errors::required` in a service without a cache module) because a request that promised
idempotency cannot proceed without it:

```rust
let header = headers
    .get("idempotency-key")
    .and_then(|v| v.to_str().ok())
    .ok_or_else(|| AppError::BadRequest("Idempotency-Key header is required".to_owned()))?;
let key = format!("app:v1:idem:{}:{header}", user.0.sub);
let fingerprint = idempotency::fingerprint(method.as_str(), uri.path(), &body);
match idempotency::claim(&state.redis, &key, &fingerprint)
    .await
    .map_err(|e| cache::required(e, "idempotency store"))?
{
    Claim::Fresh => {}
    Claim::Replay(stored) => {
        let status = StatusCode::from_u16(stored.status).unwrap_or(StatusCode::OK);
        return Ok((status, Json(stored.body)).into_response());
    }
    Claim::InFlight => return Err(AppError::Conflict("request still in flight".to_owned())),
    Claim::Mismatch => {
        let mut errors = ValidationErrors::new();
        errors.add("idempotency-key", ValidationError::new("reused for a different request"));
        return Err(errors.into()); // 422
    }
}
// The effect forwards the same `header` to the provider as *its* idempotency
// key, so the provider collapses a re-run after a lost claim into one charge.
// Redis only keeps concurrent retries out and replays the answer.
let created = state.payments.charge(&input, header).await?;
let body = serde_json::to_value(&created).map_err(anyhow::Error::from)?;
let stored = StoredResponse { status: 201, body };
idempotency::store(&state.redis, &key, &fingerprint, stored.clone())
    .await
    .map_err(|e| cache::required(e, "idempotency store"))?;
Ok((StatusCode::CREATED, Json(stored.body)).into_response())
```

## Token blacklist

Revoking a JWT before its expiry is the same shape, one key per `jti`, and the TTL is what keeps
the set from growing forever: store it for exactly the token's remaining lifetime.

```rust,verify
use std::time::Duration;

use redis::{AsyncCommands, aio::ConnectionManager};

#[tracing::instrument(skip_all, fields(db.system = "redis", db.operation = "SETEX"))]
pub async fn revoke(
    conn: &ConnectionManager,
    jti: &str,
    remaining_lifetime: Duration,
) -> Result<(), redis::RedisError> {
    conn.clone()
        .set_ex(
            format!("app:v1:revoked:{jti}"),
            1,
            remaining_lifetime.as_secs().max(1),
        )
        .await
}

/// Checked on every authenticated request, so it is on the hot path: keep the
/// response timeout short. This one fails closed - the whole point is to stop a
/// stolen token - so the caller maps the error with `required(e, "revocation
/// list")` and answers 503, never `?`.
#[tracing::instrument(skip_all, fields(db.system = "redis", db.operation = "EXISTS"))]
pub async fn is_revoked(conn: &ConnectionManager, jti: &str) -> Result<bool, redis::RedisError> {
    conn.clone().exists(format!("app:v1:revoked:{jti}")).await
}
```
