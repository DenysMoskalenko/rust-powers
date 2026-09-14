# rust-redis

### Triggering

**Should load**

1. "Cache the user lookup in Redis for five minutes and drop the entry when the profile is updated."
2. "This is the redis-py code I am porting: `r.set(key, body, ex=60)` and `r.scan_iter(match='sess:*')`. What is the redis-rs equivalent?"
3. "Two cron replicas both run the nightly reindex. Give me a distributed lock with a TTL so only one of them does."
4. "error[E0034]: multiple applicable items in scope — `conn.get(\"k\")` after importing `AsyncCommands` and `AsyncTypedCommands`."
5. "Rate limit the public API to 100 requests a minute per API key, backed by Redis, as an axum layer."

**Should not load**

1. "Publish an OrderCreated event to the message bus and have two services consume it from a work queue." -> `rust-nats`
2. "The users list endpoint should return a `Page` envelope with limit and offset." -> `axum-service`
3. "`DbErr::RecordNotUpdated` on an insert whose uuid key I generate in Rust." -> `sea-orm-postgres`
4. "Set up `rstest` fixtures and a `test_app` helper that swaps the clock." -> `rust-testing`
5. "Add `cargo-deny` to CI and pin the toolchain file." -> `rust-tooling`

### Eval 1 - a read-through cache on an existing endpoint

**Prompt**: "`GET /users/{id}` hits Postgres on every request. Put Redis in front of it and invalidate when the user is updated."

**Must produce**:
- One `ConnectionManager` built once and held in `AppState` by value, cloned per call; commands issued on the clone; `set_number_of_retries(0)` with explicit connect and response timeouts.
- A key built from a namespaced, versioned pattern such as `app:v1:user:{id}`, and a write that carries a TTL (`set_ex` or `SetExpiry`).
- A read path where a miss and an unreachable Redis both return `None` and fall through to the database, with the error logged and counted rather than returned.
- Invalidation by deleting the key after the write commits.
- `AppError::NotFound(format!("user {id} not found"))` when the loader finds nothing.

**Must not produce**:
- A `Mutex<ConnectionManager>`, an `Arc<ConnectionManager>`, a pool, or a `redis::Client` stored in state.
- A `SET` with no expiry, or a cache error propagated as a 500.
- `#[from] redis::RedisError` on `AppError`, or a new `AppError` variant for Redis.
- The `json` feature enabled to store the struct.
- Writing the updated value into the cache instead of deleting the key.

### Eval 2 - a distributed lock

**Prompt**: "Only one replica should run the nightly reindex. Add a Redis lock with a 10 minute TTL."

**Must produce**:
- `SET key token NX PX ttl` through `SetOptions` with `ExistenceCheck::NX`, and a unique token per holder.
- A Lua compare-and-delete for release, built once and invoked with `invoke_async`, plus the explanation that a plain `DEL` frees a lock taken over after expiry.
- Contention treated as `Ok(None)`, not an error. This is a scheduled job with no request: a failed acquire — held by another replica, or Redis unreachable — skips this run with a log line (info) and a counter, never a crash and never a `?` out of the job; the lease released on both the success and the error path of the body, with the statement that a failed run is retried by the next scheduled run rather than the lease being held. (`required(e, "lock")` to `AppError::Unavailable` (503) is the mapping only when a *request* cannot proceed without the lease — its appearance here is not a fault, its absence is not either.)
- The `enabled` kill switch, if mentioned, described as cache-only: a disabled lease would mean every replica runs the job.
- A sentence that this is a lease, not a mutex, and that a correctness-critical exclusion belongs in a Postgres advisory lock (`sea-orm-postgres`).

**Must not produce**:
- `DEL` as the release, or a `Drop` implementation that claims to release the lease.
- `WATCH`/`MULTI` on the multiplexed connection to make the release atomic.
- A Redlock implementation across several independent servers.

### Eval 3 - Redis-backed rate limiting

**Prompt**: "Limit each API key to 100 requests per minute, as axum middleware, backed by Redis."

**Must produce**:
- `INCR` plus `EXPIRE` in one `redis::pipe().atomic()` against a time-bucketed key, or a sliding-window Lua script, with the trade-off named.
- A `from_fn_with_state` middleware returning `AppError::TooManyRequests { retry_after_secs }` (429 with `Retry-After`), keyed on the verified `AuthUser` subject with the `ConnectInfo` peer address as the fallback — and a note that "per API key" means the authenticated identity, not a raw `x-api-key` header.
- The window bucket computed from the injected `Clock`, not `SystemTime::now()`.
- An explicit fail-open decision when Redis errors, with the failing-closed case named.
- The layer's position in the stack and the `into_make_service_with_connect_info` line in `main.rs` handed to `axum-service`'s middleware reference, not restated.

**Must not produce**:
- A per-process in-memory limiter (`governor`, a `HashMap`, a `Mutex`) presented as equivalent.
- A read-modify-write of the counter across two round trips.
- Keys or caller identifiers written into span fields or metric labels.
- A new middleware ordering invented in the answer, or the limiter applied globally with `Router::layer`.

### Eval 4 - tests for the cache layer

**Prompt**: "Write tests for the cache helper — I want to be sure a miss falls through, the TTL is set, and a WRONGTYPE does not look like an outage."

**Must produce**:
- A real Redis: `TEST_REDIS_URL` when set, otherwise a testcontainers Redis with an explicit `.with_tag("8-alpine")`, named `rust-powers-test-redis` and reused with `ReuseDirective::Always`.
- `testcontainers-modules = { version = "0.15", features = ["postgres", "redis", "nats"] }` left as pinned — no feature list that drops `postgres`.
- Per-test key prefixes for isolation, and a note that a reused named container needs a retry around `start()`.
- An assertion on `err.code() == Some("WRONGTYPE")` distinguished from `is_timeout()` / `is_connection_dropped()`.

**Must not produce**:
- `redis-test`'s `MockRedisConnection` or a hand-written fake connection as the main approach.
- `FLUSHDB` between tests, or a container started per test.
- `tokio::time::pause` used to make a server-side TTL expire.
