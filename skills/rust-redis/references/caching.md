# Caching and commands with redis-rs

- [Connect once, clone per call](#connect-once-clone-per-call)
- [Which command trait](#which-command-trait)
- [Values: the Json newtype](#values-the-json-newtype)
- [Keys and TTLs](#keys-and-ttls)
- [Cache-aside](#cache-aside)
- [Negative caching and stampedes](#negative-caching-and-stampedes)
- [Invalidation](#invalidation)
- [SCAN, never KEYS](#scan-never-keys)
- [Pipelines](#pipelines)
- [Lua scripts](#lua-scripts)
- [Degrading, classifying, counting](#degrading-classifying-counting)
- [Blocking commands and the one case for a pool](#blocking-commands-and-the-one-case-for-a-pool)
- [Command cheat sheet](#command-cheat-sheet)

## Connect once, clone per call

`redis::Client` is a parsed URL, not a connection. The connection is an
`aio::ConnectionManager`: it multiplexes every caller's commands over one socket and reconnects
itself with exponential backoff, so a process wants exactly one, held in application state by
value. It is `Clone` (an `Arc` and an `ArcSwap` inside), and commands take `&mut self`, so each
call site clones it. A `Mutex` around it would serialise the whole service for no gain.

```rust,verify
use std::time::Duration;

use redis::{
    IntoConnectionInfo,
    aio::{ConnectionManager, ConnectionManagerConfig},
};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

use crate::api::health::Health;

/// The settings section to add: `APP__REDIS__URL`, `APP__REDIS__RESPONSE_TIMEOUT_MS`, ...
#[derive(Debug, Clone, Deserialize)]
pub struct RedisSettings {
    /// `redis://[:password@]host:port[/db]`, or `rediss://...` for TLS.
    pub url: SecretString,
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
    /// Deliberately short: a slow cache must not turn into a slow API.
    #[serde(default = "default_response_timeout_ms")]
    pub response_timeout_ms: u64,
    /// Kill switch for the cache only. When false every read misses and every
    /// write is skipped, which is how a cache incident gets resolved without a
    /// deploy. Leases, limiters and idempotency keys ignore it: "disabled" for
    /// a lease would mean every replica runs the job.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

const fn default_connect_timeout_ms() -> u64 {
    500
}
const fn default_response_timeout_ms() -> u64 {
    100
}
const fn default_true() -> bool {
    true
}

/// Lazy: nothing is dialled until the first command, so a Redis that is down at
/// boot leaves the service up with `"cache": "degraded"` instead of crash-looping
/// the pod, and the first command after Redis returns connects. It spawns a task,
/// so call it inside the runtime. A service that cannot run without Redis uses
/// `get_connection_manager_with_config(config).await` and fails the boot instead.
pub fn connect(settings: &RedisSettings) -> Result<ConnectionManager, redis::RedisError> {
    let info = settings.url.expose_secret().into_connection_info()?;
    // What `CLIENT LIST` shows as `lib-name`; the default is `redis-rs`.
    let handshake = info
        .redis_settings()
        .clone()
        .set_lib_name(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    let client = redis::Client::open(info.set_redis_settings(handshake))?;
    let config = ConnectionManagerConfig::new()
        .set_connection_timeout(Some(Duration::from_millis(settings.connect_timeout_ms)))
        .set_response_timeout(Some(Duration::from_millis(settings.response_timeout_ms)))
        // Zero, because the reconnect budget is paid inside the command that
        // finds the socket dead - a request-path caller cannot afford it. The
        // manager still re-arms a reconnect for the next command.
        .set_number_of_retries(0);
    client.get_connection_manager_lazy(config)
}

/// The `"cache"` entry for `axum-service`'s readiness `checks` map: one
/// `PING`, no keys touched, bounded by the response timeout. Redis is optional,
/// so a failure is `Degraded` (the probe stays 200), never `Unavailable`.
pub async fn readiness(conn: &ConnectionManager) -> Health {
    let mut conn = conn.clone();
    match redis::cmd("PING").exec_async(&mut conn).await {
        Ok(()) => Health::Ok,
        Err(error) => {
            tracing::warn!(%error, "readiness: redis ping failed");
            Health::Degraded
        }
    }
}
```

Since 1.0 the defaults are a 1 s connection timeout, a 500 ms response timeout and six reconnect
attempts with exponential backoff. The timeouts bound a *slow* server: a paused Redis turns a `GET`
into a miss after the response timeout (measured 101 ms with the settings above). The retries
bound nothing useful in a request path, because the manager runs the whole retry loop *inside*
whichever command finds the socket dead, and every later command joins the loop still running.
Measured with six retries against a refused port: 4.7 s per `Cache::get`, every call, for the
length of the outage; 8.3 s when the host is unreachable. With zero retries the same calls take
0.6 ms and one connect timeout, and the manager still reconnects on the next command after the
server returns (measured after a `docker restart`: first command fails, second succeeds). So the
worst case a cache call can add to a request is `connect_timeout + response_timeout`, and that is
the number to keep inside the request budget.

## Which command trait

Two traits carry the commands and they share every method name:

- `AsyncCommands` is generic in the return type. Every call needs an annotation or a turbofish,
  because `RV: FromRedisValue` is otherwise unconstrained. It is the one that accepts
  `Option<T>`, `HashMap<K, V>` or a custom `FromRedisValue` type.
- `AsyncTypedCommands` fixes the return types (`get` is `Option<String>`, `set_ex` is `()`), so
  the annotations disappear and unusual shapes are out of reach.

Importing both into one module makes every call ambiguous:

```rust,ignore
use redis::{AsyncCommands, AsyncTypedCommands};   // E0034
let v: Option<String> = conn.get("k").await?;     // multiple applicable items in scope
```

One trait per module. Use `AsyncCommands` where serde values and `Option` shapes are wanted, and
`AsyncTypedCommands` in a module that only moves strings and counters:

```rust,verify
use redis::{AsyncTypedCommands, aio::ConnectionManager};

pub async fn counters(conn: &ConnectionManager) -> Result<isize, redis::RedisError> {
    let mut conn = conn.clone();
    conn.set_ex("app:v1:greeting", "hello", 60).await?;
    let greeting = conn.get("app:v1:greeting").await?;
    debug_assert_eq!(greeting.as_deref(), Some("hello"));
    let hits = conn.incr("app:v1:counter:page", 1).await?;
    conn.del("app:v1:greeting").await?;
    Ok(hits)
}
```

Discarding a reply is `conn.set_ex::<_, _, ()>(key, value, ttl).await?`, not
`let _ = conn.set_ex(..)`: the latter builds a future and drops it without sending anything. The
stack's lint table catches that one (`let_underscore_future`), but only that one.

## Values: the Json newtype

`redis`'s `json` feature is the RedisJSON *server module* (`JSON.SET`), not serde support, and
enabling it to store structs earns `ERR unknown command 'JSON.SET'` against a server without the
module. Caching a struct needs no feature at all, just a newtype that implements the three
conversion traits. It composes where a `serde_json::from_slice` at the call site does not: as an
`hset` value, inside `pipe()`, and as the element type of `Vec<Json<T>>` from an `mget`.
A value that no longer deserializes (`ParsingError`) comes out of the cache read as `None`, logged
as a bug: bump the key version instead of living with it.

## Keys and TTLs

One builder, one place to bump the version, and never a write without an expiry. Staging and
production sharing one managed instance need either an environment segment in the namespace or a
database index in the URL (`redis://host:6379/3`); keys in one index are invisible from another.

```rust,verify
//! `app:v1:user:01890a...` - namespace, schema version, entity, id.

/// Bump when the serialized shape of anything cached changes: old keys then age
/// out on their own TTL instead of deserializing into the new struct and failing.
pub const VERSION: &str = "v1";
const NAMESPACE: &str = "app";

#[must_use]
pub fn key(entity: &str, id: &str) -> String {
    format!("{NAMESPACE}:{VERSION}:{entity}:{id}")
}

#[must_use]
pub fn scoped(entity: &str, scope: &str, id: &str) -> String {
    format!("{NAMESPACE}:{VERSION}:{entity}:{scope}:{id}")
}

/// A prefix to `SCAN` for. Never pass this to `KEYS`.
#[must_use]
pub fn entity_pattern(entity: &str) -> String {
    format!("{NAMESPACE}:{VERSION}:{entity}:*")
}
```

TTL policy: pick the shortest value that still moves the hit rate. Read-through entity caches
live minutes, not hours; a rendered page or an expensive aggregate can live longer; anything
holding authorization state lives seconds. The TTL is the only cleanup mechanism Redis has, so
the cache's memory ceiling is the sum of what fits inside it.

## Cache-aside

The module below is the whole pattern: `Json<T>`, a read that degrades, a best-effort write, and
`get_or_set`.

```rust,verify
//! src/cache.rs - cache-aside over one `ConnectionManager`.
//!
//! - a miss is `None`, never an error;
//! - a Redis failure on the read path degrades to a miss and is logged once;
//! - a failure on the write path is logged and dropped, because the caller
//!   already holds the value;
//! - every write carries a TTL. `SET` without an expiry is how a cache becomes
//!   a memory leak.

use std::{
    future::Future,
    hash::{DefaultHasher, Hash, Hasher},
    time::Duration,
};

use redis::{
    AsyncCommands, FromRedisValue, ParsingError, RedisWrite, ToRedisArgs, ToSingleRedisArg, Value,
    aio::ConnectionManager,
};
use serde::{Serialize, de::DeserializeOwned};

use crate::error::AppError;

/// The serde bridge. `redis`'s `json` feature is a different thing entirely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Json<T>(pub T);

impl<T: Serialize> ToRedisArgs for Json<T> {
    fn write_redis_args<W>(&self, out: &mut W)
    where
        W: ?Sized + RedisWrite,
    {
        // `ToRedisArgs` cannot fail. Serializing a plain struct cannot either;
        // a map with non-string keys can. Write a JSON null and let the read
        // side surface it rather than panic inside a trait that forbids it.
        // (Not `to_writer(out.writer_for_next_arg(), ..)` with a fallback: a
        // partial argument is committed on drop, and the null becomes a second
        // argument - `SET key { null` is a syntax error on the server.)
        let bytes = serde_json::to_vec(&self.0).unwrap_or_else(|_| b"null".to_vec());
        out.write_arg(&bytes);
    }
}

/// `set`, `set_ex`, `set_options` and `hset` take `ToSingleRedisArg`, a marker
/// saying "this is exactly one argument". A value type that implements only
/// `ToRedisArgs` compiles against `rpush` and `sadd` and fails against these.
impl<T: Serialize> ToSingleRedisArg for Json<T> {}

impl<T: DeserializeOwned> FromRedisValue for Json<T> {
    fn from_redis_value(v: Value) -> Result<Self, ParsingError> {
        let bytes = Vec::<u8>::from_redis_value(v)?;
        serde_json::from_slice(&bytes)
            .map(Self)
            .map_err(|e| ParsingError::from(format!("invalid cached JSON: {e}")))
    }
}

#[derive(Clone)]
pub struct Cache {
    conn: ConnectionManager,
    enabled: bool,
}

impl std::fmt::Debug for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cache")
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

impl Cache {
    #[must_use]
    pub const fn new(conn: ConnectionManager, enabled: bool) -> Self {
        Self { conn, enabled }
    }

    /// Cloning the manager is how a caller gets the `&mut` the commands want.
    #[must_use]
    pub fn conn(&self) -> ConnectionManager {
        self.conn.clone()
    }

    /// `None` for both "not cached" and "cache unreachable". `skip_all`: the
    /// key is user data and must not land in the span.
    #[tracing::instrument(skip_all, fields(db.system = "redis", db.operation = "GET"))]
    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        if !self.enabled {
            return None;
        }
        match self.conn().get::<_, Option<Json<T>>>(key).await {
            Ok(Some(Json(value))) => {
                metrics::counter!("cache_hits_total").increment(1);
                Some(value)
            }
            Ok(None) => {
                metrics::counter!("cache_misses_total").increment(1);
                None
            }
            Err(e) => {
                degraded(&e, "GET");
                None
            }
        }
    }

    /// Best effort: the caller has the value already, so a failed write is not
    /// its problem.
    #[tracing::instrument(skip_all, fields(db.system = "redis", db.operation = "SETEX"))]
    pub async fn set<T: Serialize + Sync>(&self, key: &str, value: &T, ttl: Duration) {
        if !self.enabled {
            return;
        }
        let ttl = jittered(key, ttl);
        let write = self
            .conn()
            .set_ex::<_, _, ()>(key, Json(value), ttl.as_secs().max(1))
            .await;
        if let Err(e) = write {
            degraded(&e, "SETEX");
        }
    }

    /// Invalidate by deleting, never by writing the new value: two concurrent
    /// writers cannot leave a stale entry behind if both delete.
    #[tracing::instrument(skip_all, fields(db.system = "redis", db.operation = "DEL"))]
    pub async fn invalidate(&self, keys: &[String]) {
        if !self.enabled || keys.is_empty() {
            return;
        }
        if let Err(e) = self.conn().del::<_, ()>(keys).await {
            degraded(&e, "DEL");
        }
    }

    /// `load` runs on a miss, on a deserialization failure, and when Redis is
    /// down. The value type is `Option<T>` on purpose: `None` round-trips as a
    /// JSON `null`, which is negative caching for free.
    pub async fn get_or_set<T, E, F, Fut>(
        &self,
        key: &str,
        ttl: Duration,
        load: F,
    ) -> Result<Option<T>, E>
    where
        T: Serialize + DeserializeOwned + Sync,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Option<T>, E>>,
    {
        if let Some(hit) = self.get::<Option<T>>(key).await {
            return Ok(hit);
        }
        let value = load().await?;
        self.set(key, &value, ttl).await;
        Ok(value)
    }
}

/// Spread expiries so a batch written in the same second does not expire in the
/// same second. Derived from the key rather than a RNG, so it is reproducible
/// and needs no extra dependency.
fn jittered(key: &str, ttl: Duration) -> Duration {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    let spread = hasher.finish() % 21; // 0..=20 %
    let base = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX);
    Duration::from_millis(base.saturating_add(base / 100 * spread))
}

/// "Redis is not answering", as opposed to "Redis answered, and said no".
#[must_use]
pub fn is_unavailable(e: &redis::RedisError) -> bool {
    e.is_timeout() || e.is_connection_dropped() || e.is_connection_refusal() || e.is_io_error()
}

/// For the few commands whose failure a request cannot survive - a lease that
/// must be held, an idempotency claim, a token-revocation check. Explicit, never
/// `#[from]`: a blanket `From<RedisError>` would make every `?` on a cache call
/// a 500, which is the failure mode this module exists to prevent.
#[must_use]
pub fn required(e: redis::RedisError, dependency: &str) -> AppError {
    if is_unavailable(&e) {
        AppError::Unavailable(dependency.to_owned())
    } else {
        AppError::Other(e.into())
    }
}

fn degraded(e: &redis::RedisError, op: &'static str) {
    metrics::counter!("cache_errors_total").increment(1);
    if is_unavailable(e) {
        unavailable(e, op);
    } else {
        broken(e, op);
    }
}

/// Expected during a failover, and not an error for this request.
fn unavailable(e: &redis::RedisError, op: &'static str) {
    tracing::warn!(db.system = "redis", db.operation = op, error = %e, "cache unavailable, degrading");
}

/// WRONGTYPE, NOSCRIPT, a bad `FromRedisValue`: a bug, not an outage.
fn broken(e: &redis::RedisError, op: &'static str) {
    tracing::error!(db.system = "redis", db.operation = op, error = %e, "cache command failed");
}
```

Wire it up by adding the manager to application state and building the `Cache` beside it. The
handler then reads through the cache and invalidates after the write commits:

```rust
let user = cache
    .get_or_set(&keys::key("user", &id), Duration::from_secs(300), || async {
        users::find(&db, id).await
    })
    .await?
    .ok_or_else(|| AppError::NotFound(format!("user {id} not found")))?;
```

## Negative caching and stampedes

`Option<T>` as the cached type means a missing row is stored as `null` and a 404 stops re-querying
the database on every retry. `get_or_set` gives both outcomes the same TTL; when the row is likely
to appear soon, invalidate its key from the code that creates it (below) rather than shortening
the TTL - that is the same delete-on-write the update path already does.

A stampede is what happens when a popular key expires and every in-flight request loads it at
once. Three mitigations, in increasing cost:

1. **Jitter** (above) stops a batch written together from expiring together. It is free and it is
   usually enough.
2. **A lease**: take a short lock on the key before loading, and let the losers serve a stale
   value or wait. One writer refills, the rest do not touch the database.
3. **Early recomputation**: store the value with its own expiry timestamp inside and refresh it
   probabilistically before the TTL. Only worth the complexity for a handful of very hot keys.

Do not reach for a `Mutex` or a `tokio::sync::Semaphore` here: it coordinates one process, and the
stampede comes from all of them.

## Invalidation

Delete on write, after the transaction commits. Deleting before it commits opens a window where a
concurrent read repopulates the cache with the old row, and writing the new value instead of
deleting lets two concurrent writers interleave into a stale entry.

When one write invalidates a family of keys, either keep a set of the members, or give the family
its own version segment and bump it. Scanning for a prefix on every write is the slow option.

## SCAN, never KEYS

`KEYS pattern` walks the entire keyspace in one blocking pass and stalls every other client on
that server for the duration. `SCAN` is a cursor, and the iterator borrows the connection for its
lifetime, so collect the keys before issuing other commands on the same clone.

```rust,verify
use redis::{AsyncCommands, ScanOptions, aio::ConnectionManager};

pub async fn scan_prefix(
    conn: &ConnectionManager,
    pattern: &str,
) -> Result<Vec<String>, redis::RedisError> {
    let mut conn = conn.clone();
    let options = ScanOptions::default().with_pattern(pattern).with_count(100);
    let mut iter = conn.scan_options::<String>(options).await?;
    let mut keys = Vec::new();
    // `AsyncIter` implements `Stream`, so `futures::TryStreamExt::try_collect`
    // works too; `next_item` yields `Option<RedisResult<T>>` without the import.
    while let Some(key) = iter.next_item().await {
        keys.push(key?);
    }
    Ok(keys)
}

/// Deleting a whole prefix: scan, then delete in one batch. `COUNT` is a hint,
/// so a scan can return the same key twice - `DEL` does not care.
pub async fn delete_prefix(
    conn: &ConnectionManager,
    pattern: &str,
) -> Result<usize, redis::RedisError> {
    let keys = scan_prefix(conn, pattern).await?;
    if keys.is_empty() {
        return Ok(0);
    }
    conn.clone().del(&keys).await
}
```

## Pipelines

A pipeline is one round trip for N commands; `.atomic()` wraps them in `MULTI`/`EXEC`. The result
tuple needs exactly one slot per command that is not `.ignore()`, and a mismatch is a runtime
parse failure rather than a compile error.

```rust,verify
use redis::aio::ConnectionManager;

pub async fn bump_and_check(
    conn: &ConnectionManager,
    counter: &str,
    other: &str,
) -> Result<(i64, bool), redis::RedisError> {
    let mut conn = conn.clone();
    let (count, existed): (i64, bool) = redis::pipe()
        .atomic()
        .incr(counter, 1)
        .expire(counter, 3600)
        .ignore()
        .exists(other)
        .query_async(&mut conn)
        .await?;
    Ok((count, existed))
}
```

`MULTI`/`EXEC` is not a transaction that can branch: there is no reading a value and deciding
what to write. That needs `WATCH`, and `WATCH` needs a connection nobody else touches - which a
multiplexed connection is not, since other callers' commands interleave and arm and disarm the
watch unpredictably. Read-modify-write belongs in a Lua script instead.

## Lua scripts

Everything between the first and the last `redis.call` in a script runs without interleaving,
which is what makes a read-modify-write safe. `Script::invoke_async` sends `EVALSHA` first and,
on `NOSCRIPT`, runs `SCRIPT LOAD` and retries the `EVALSHA` (never `EVAL` - relevant to an ACL
that denies `SCRIPT`), so a restarted Redis with an empty script cache needs no handling. Build the `Script` once in a `LazyLock`; rebuilding it per call
re-hashes the body every time.

```rust,verify
use std::sync::LazyLock;

use redis::aio::ConnectionManager;

static COMPARE_AND_SET: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"if redis.call('GET', KEYS[1]) == ARGV[1] then
              redis.call('SET', KEYS[1], ARGV[2])
              return 1
          end
          return 0",
    )
});

pub async fn compare_and_set(
    conn: &ConnectionManager,
    key: &str,
    expected: &str,
    next: &str,
) -> Result<bool, redis::RedisError> {
    let mut conn = conn.clone();
    let changed: i64 = COMPARE_AND_SET
        .key(key)
        .arg(expected)
        .arg(next)
        .invoke_async(&mut conn)
        .await?;
    Ok(changed == 1)
}
```

Keys the script touches go through `.key(..)`, never `.arg(..)`: only the declared `KEYS` are
visible to a clustered server's slot routing, and a script that reaches a key it did not declare
fails there while passing locally.

Redis 8.4 added the compare natively: `SetOptions::default().value_comparison(ValueComparison::ifeq(expected))`
is `SET key v IFEQ expected`, and `conn.del_ex(key, ValueComparison::ifeq(token))` is
`DELEX key IFEQ token`. Both fail with an unknown-command or syntax error on 7.x, Valkey and most
managed offerings, so the script stays the default and the native form is an optimisation for a
fleet known to be on 8.4 or later.

## Degrading, classifying, counting

`RedisError` mixes two unrelated failures. `is_timeout()`, `is_connection_dropped()`,
`is_connection_refusal()` and `is_io_error()` mean the server is not answering: degrade to a miss,
count it, and carry on. Anything else - `WRONGTYPE`, `NOSCRIPT` surfacing through a raw `EVALSHA`,
a `ParsingError` from a bad `FromRedisValue` - is a bug in the code, and `err.code()` names it.

A cache error reaching the client as a 500 is the failure mode this section exists to prevent, so
`AppError` gets no `#[from] redis::RedisError`: with one, every `?` on a Redis call in a handler is
a 500. Cache reads return `Option` and never an error. The few commands a request cannot survive
without - a lease that must be held, an idempotency claim, a revocation check - go through
`required(e, "what")` above: `AppError::Unavailable` (503, constant body, the name in the log only)
when Redis is unreachable, `AppError::Other` (500) for a bug such as `WRONGTYPE`.

Metrics worth having: `cache_hits_total`, `cache_misses_total` and `cache_errors_total`. Label them
by entity if at all (`counter!("cache_hits_total", "entity" => "user")`), never by key, which is
unbounded cardinality. Spans carry `db.system = "redis"` and `db.operation` under
`#[instrument(skip_all)]`; `skip(self)` still records every other argument, so the key ends up in
the span as `key="app:v1:user:..."`. Neither the key nor the value belongs there: both are user
data and the span is emitted at info level.

## Blocking commands and the one case for a pool

A multiplexed connection interleaves every caller's commands on one socket, so a blocking command
holds that socket for its entire server-side timeout. Measured against `redis:8-alpine`: a
`BLPOP key 2` on an empty list, and a `GET` issued 100 ms later on a clone of the same
`ConnectionManager`, returned after **1.94 s**. With a production-sized 100 ms response timeout
the `GET` would have failed instead.

So the rule is one `ConnectionManager` for everything, and a small pool only for the module that
issues `BLPOP`, `BRPOP`, `BLMOVE`, `BZPOPMIN`, `XREAD BLOCK` or `WAIT`. In this stack a queue is a
NATS subject, so that module usually does not exist.

```rust,verify
/// Only for a module that blocks. `deadpool-redis = "0.23"` is an extra
/// dependency; adding it for anything else pools connections that already
/// multiplex, which buys nothing.
pub fn blocking_pool(url: &str) -> Result<deadpool_redis::Pool, deadpool_redis::CreatePoolError> {
    deadpool_redis::Config::from_url(url).create_pool(Some(deadpool_redis::Runtime::Tokio1))
}
```

## Command cheat sheet

| Need | redis-rs |
|---|---|
| connect | `Client::open(url)?` then `get_connection_manager_lazy(cfg)` for an optional cache, `get_connection_manager_with_config(cfg).await` where Redis is required |
| share the connection | one `aio::ConnectionManager`, cloned per call |
| set with a TTL | `conn.set_ex::<_, _, ()>(k, v, 60)` |
| set only if absent, with a TTL | `set_options` with `ExistenceCheck::NX` and `SetExpiry::PX` |
| read a key that may be missing | `conn.get::<_, Option<String>>(k)` |
| walk keys by pattern | `conn.scan_options::<String>(..)` then `next_item().await` |
| many commands, one round trip | `redis::pipe().atomic()...query_async(&mut conn)` |
| run a Lua script | `redis::Script::new(src)` then `.key(..).arg(..).invoke_async(..)` |
| take a lock | no built-in; `SET NX PX` plus a Lua release (`del_ex` IFEQ on ≥ 8.4) |
| tell a dead server from a bad command | `e.is_connection_refusal()` or `e.is_connection_dropped()` |
| catch a wrong-type key | `e.code() == Some("WRONGTYPE")` |
