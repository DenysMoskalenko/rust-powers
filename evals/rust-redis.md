# rust-redis

### Triggering

**Should load**

1. "Cache the user lookup in Redis for five minutes and drop the entry when the profile is updated."
2. "Store a session body with a 60-second TTL, then walk every `sess:*` key. Which redis-rs calls?"
3. "Two cron replicas both run the nightly reindex. Give me a distributed lock with a TTL so only one of them does."
4. "error[E0034]: multiple applicable items in scope — `conn.get(\"k\")` after importing `AsyncCommands` and `AsyncTypedCommands`."
5. "Cap every client of the public API at 100 requests a minute. We run four replicas behind an nginx ingress and already have Redis."
6. "Clients retry `POST /payments` after a timeout and get charged twice. Add `Idempotency-Key` support backed by Redis."
7. "Our service crash-loops on boot whenever Redis is down, even though Redis only holds a cache."

**Should not load**

1. "Publish an OrderCreated event to the message bus and have two services consume it from a work queue." -> `rust-nats`
2. "Use a Postgres advisory lock so only one replica runs the nightly ledger reconciliation inside its transaction." -> `sea-orm-postgres`
3. "Replace our Redis pub/sub notifications with something durable that survives a consumer restart." -> `rust-nats`
4. "Where does the rate-limit layer go relative to auth, timeout and tracing in our middleware stack?" -> `axum-service`
5. "Read feature flags from a NATS KV bucket and react when one changes." -> `rust-nats`
6. "Write the OpenAPI docs for the `Idempotency-Key` header on `POST /payments`: what a replay, a mismatched body and an in-flight retry return." -> `axum-service`
7. "Set `Cache-Control` and an `ETag` on `GET /products/{id}` responses." -> `axum-service`
8. "Add a `redis:8-alpine` service to docker compose and to the CI workflow so the integration tests can reach Redis." -> `rust-tooling`

### Eval 1 - a read-through cache on an existing endpoint

**Prompt**: "`GET /users/{id}` hits Postgres on every request. Put Redis in front of it and invalidate when the user is updated."

**Must produce**:
- One `ConnectionManager` built once and held in `AppState` by value, cloned per call.
- `set_number_of_retries(0)` with explicit connect and response timeouts.
- The manager built with `get_connection_manager_lazy`, or another way that lets the service boot while Redis is down.
- A key built from a namespaced, versioned pattern such as `app:v1:user:{id}`.
- Every cache write carrying a TTL (`set_ex` or `SetExpiry`).
- A read path where a miss and an unreachable Redis both return `None` and fall through to the database.
- The read error logged and counted rather than returned.
- Invalidation by deleting the key after the write commits.
- `AppError::NotFound(format!("user {id} not found"))` when the loader finds nothing.

**Must not produce**:
- A `Mutex<ConnectionManager>`, an `Arc<ConnectionManager>`, a pool, or a `redis::Client` stored in state.
- A `SET` with no expiry.
- A cache error propagated as a 500.
- `#[from] redis::RedisError` on `AppError`.
- A new `AppError` variant for Redis.
- The `json` feature enabled to store the struct.
- Writing the updated value into the cache instead of deleting the key.
- An eager connect at startup whose failure stops the service from booting.

### Eval 2 - a distributed lock

**Prompt**: "Only one replica should run the nightly search reindex, which pushes every product to Elasticsearch. Add a Redis lock with a 10 minute TTL."

**Must produce**:
- `SET key token NX PX ttl` through `SetOptions` with `ExistenceCheck::NX`.
- A unique token per holder.
- A Lua compare-and-delete for release, built once and invoked with `invoke_async`.
- The explanation that a plain `DEL` frees a lock another replica took over after expiry.
- Contention returned as `Ok(None)`, not an error.
- A failed acquire (held elsewhere, or Redis unreachable) skipping this run with an info log and a counter, never a crash or a `?` out of the job.
- The lease released on both the success and the error path of the body.
- A failed run left to the next scheduled run rather than the lease held until its TTL.
- A statement that this is a lease, not a mutex: it only suppresses duplicates, so the reindex must survive running twice.

**Must not produce**:
- `DEL` as the release.
- A `Drop` implementation that claims to release the lease.
- `WATCH`/`MULTI` on the multiplexed connection to make the release atomic.
- A Redlock implementation across several independent servers.
- The cache's `enabled` kill switch applied to the lease.

### Eval 3 - Redis-backed rate limiting

**Prompt**: "Limit each API key to 100 requests per minute, as axum middleware backed by Redis. The service runs behind one nginx ingress, and the public routes need a limit too."

**Must produce**:
- `INCR` plus `EXPIRE` in one `redis::pipe().atomic()` against a time-bucketed key, or a sliding-window Lua script.
- The trade-off between the two named: a fixed window lets up to twice the limit through across a boundary.
- A `from_fn_with_state` middleware returning `AppError::TooManyRequests { retry_after_secs }` (429 with `Retry-After`).
- Keyed callers counted per verified identity (an `AuthUser` subject or a key checked against configuration), never the raw `x-api-key` value.
- Anonymous callers keyed on the `X-Forwarded-For` entry the ingress appended, the last one behind a single proxy.
- The number of trusted proxies taken from configuration, never from the request.
- A fixed-window bucket computed from the injected `Clock` (a sliding script may use Redis `TIME`), never `SystemTime::now()`.
- An explicit fail-open decision when Redis errors.
- The fail-closed case named: a limit that guards something which cannot absorb the traffic.

**Must not produce**:
- The first `X-Forwarded-For` entry, which the client writes, used as the caller's key.
- The `ConnectInfo` peer used as the anonymous caller's key although the service runs behind the ingress.
- A per-process in-memory limiter (`governor`, a `HashMap`, a `Mutex`) presented as equivalent.
- A read-modify-write of the counter across two round trips.
- Keys or caller identifiers written into span fields or metric labels.
- A new middleware ordering invented in the answer.
- The limiter applied globally with `Router::layer`.

### Eval 4 - tests for the cache layer

**Prompt**: "Add a Redis cache-aside helper to this service and write its tests — I want to be sure a miss falls through, the TTL is set, and a WRONGTYPE does not look like an outage."

**Must produce**:
- A real Redis: `TEST_REDIS_URL` when set.
- Otherwise a testcontainers Redis with an explicit `.with_tag("8-alpine")`.
- That container named `rust-powers-test-redis` and reused with `ReuseDirective::Always`.
- Per-test key prefixes for isolation.
- A retry around `start()` for the reused named container.
- An assertion on `err.code() == Some("WRONGTYPE")` distinguished from `is_timeout()` / `is_connection_dropped()`.

**Must not produce**:
- `redis-test`'s `MockRedisConnection` or a hand-written fake connection as the main approach.
- `FLUSHDB` between tests.
- A container started per test.
- `tokio::time::pause` used to make a server-side TTL expire.
- A `testcontainers-modules` feature list that drops `postgres`.

### Eval 5 - idempotency keys per caller

**Prompt**: "Clients retry `POST /payments` after timeouts and we charge them twice. Add `Idempotency-Key` support and keep the replay data in Redis."

**Must produce**:
- The Redis key scoped to the caller, such as `app:v1:idem:{subject}:{Idempotency-Key}`.
- The reason stated: with the raw header as the key, two clients sending the same value get each other's stored response.
- The claim and the check in one command: `SET key marker NX EX ttl`.
- The claim's TTL no shorter than the request timeout.
- The stored entry holding a fingerprint of method, path and body plus the response, never the body itself.
- The stored response written with its own `EX` for the replay window (24 h).
- Same key and same fingerprint replaying the stored status and body.
- Same key with a different fingerprint answered 422.
- Same key while the first attempt is still running answered 409.
- Storage errors mapped through `required(e, ..)` onto `AppError::Unavailable` (503) rather than `?`.
- The payment provider called with the same idempotency key, since Redis alone cannot make the charge atomic with its record.

**Must not produce**:
- The raw `Idempotency-Key` header value used as the whole Redis key.
- A `GET` followed by a `SET` as the claim.
- The request body stored in Redis.
- A new `AppError` variant for idempotency.
- The initial `SET NX` claim given the 24 h replay TTL instead of one near the request timeout.

### Probe 1 - sharing the connection

**Prompt**: "Share one Redis connection across all my axum handlers."

**Wrong answer**: `:\s*(?:std::sync::|tokio::sync::)?(?:Arc|Mutex|RwLock)<\s*(?:(?:std::sync::|tokio::sync::)?Mutex<\s*)?(?:redis::aio::)?(?:Multiplexed\w*|ConnectionManager)`

**Right answer**: `ConnectionManager`

### Probe 2 - KEYS in application code

**Prompt**: "Delete every `sess:*` key from my Rust service."

**Wrong answer**: `\.keys\s*(?:::<[^>]*>)?\([^)]*\)\s*\.await|cmd\(\s*"KEYS"\s*\)`

**Right answer**: `scan_(?:match|options)`

### Probe 3 - releasing a lock with DEL

**Prompt**: "Write the release function for our Redis lock."

**Wrong answer**: `^(?![\s\S]*Script::new)[\s\S]*\.del(?:::<[^>]*>)?\(`

**Right answer**: `Script::new\(`

### Probe 4 - a blanket From for RedisError

**Prompt**: "`?` on a Redis call in my handler fails: the trait `From<RedisError>` is not implemented for `AppError`. Fix it."

**Wrong answer**: `\(\s*#\[from\]\s*(?:redis::)?RedisError\s*\)|impl\s+From<\s*(?:redis::)?RedisError\s*>\s+for\s+AppError\s*\{`

### Probe 5 - an idempotency key shared across callers

**Prompt**: "Add `Idempotency-Key` support to `POST /payments`, storing the replay in Redis."

**Wrong answer**: `format!\(\s*"[^"{]*idem(?:[^"{]|\{(?!caller|subject|sub\b|user|principal|tenant)[^}]*\})*"(?![^)]*\b(?:caller|subject|sub|user|principal|tenant))`

### Probe 6 - run once across replicas on Postgres

**Prompt**: "Only one replica should run the nightly ledger reconciliation, which updates rows in our Postgres. Add a Redis lock."

**Wrong answer**: `^(?![\s\S]*pg_try_advisory_xact_lock)[\s\S]*ExistenceCheck::NX`

**Right answer**: `pg_try_advisory_xact_lock`
