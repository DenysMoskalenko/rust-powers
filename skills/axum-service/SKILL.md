---
name: axum-service
description: "Use when building or modifying an axum service — routes, extractors, validated request bodies, response DTOs, AppError and status mapping, utoipa OpenAPI, settings and secrets, middleware order, health and readiness, pagination, SSE, background tasks, graceful shutdown, JWT auth, tracing, OTLP export, Prometheus metrics. Also for Handler is not satisfied, a router panic about path segments, or spans never reaching the collector. Not for queries (sea-orm-postgres), flaky tests (rust-testing), or Cargo.toml, CI and lint config (rust-tooling)."
metadata:
  version: "0.1.0"
---

# axum service patterns

Assumes Rust 1.98 edition 2024, axum 0.8, tower-http 0.7, utoipa 5, validator 0.21, config 0.15, secrecy 0.10, reqwest 0.13, tracing 0.1 with opentelemetry 0.32, axum-prometheus 0.10, jsonwebtoken 11, argon2 0.6.

## Important

- Path captures are `/{id}` and `/{*rest}`; the 0.7 spelling `/:id` compiles and panics at router build.
- The one body extractor (`Json`, `Valid`) is the last handler argument; otherwise the only error is `Handler` is not satisfied.
- Handlers return `Result<_, AppError>`; `error.rs` is the only file that chooses a status code, and a 5xx body is a constant. Never `panic!` to signal failure.
- A response is its own DTO with `From<Model>`; `Json(model)` ships every column, `password_hash` included. Not `#[serde(skip_serializing)]` on the entity: codegen overwrites it.
- Every validator rule a client needs is repeated as `#[schema(...)]` or `#[param(...)]`; utoipa never reads `validate`.

## Architecture

A handler extracts, runs at most one ORM statement directly, and maps the result to a response DTO. Logic that spans more than one statement, or that a second caller needs, moves to a `services/` function. No repository layer — sea-orm is already the abstraction over SQL; a trait on top buys one implementation and a mock that proves nothing.

```text
src/
  main.rs      wiring only: dotenv, Settings::load, telemetry, pool, AppState, serve, shutdown
  lib.rs       AppState, ApiDoc, build_router(state) -> Router: the stack and both fallbacks
  config.rs    Settings sub-structs, the four timeout constants
  error.rs     AppError + ErrorBody + IntoResponse; REQUEST_ID task-local
  extract.rs   Valid, ValidQuery
  telemetry.rs init(&TelemetrySettings) -> TelemetryGuard
  auth.rs      Keys, Claims, AuthUser, hashing (with the first protected route)
  api/         mod.rs (router(), request_id), health.rs, users.rs (DTOs, handlers, router())
  services/    multi-statement or shared logic; the scaffold has none yet
```

`build_router(state)` is public so integration tests hit the real stack.

## State

`AppState` is cheap to clone — every field is a handle. Never `Arc<AppState>`: it forces `State<Arc<AppState>>` everywhere and gives up `FromRef`.

```rust
#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub settings: Arc<Settings>,
    pub clock: Arc<dyn Clock>,
    pub http: ClientWithMiddleware,
}
```

`State` for anything known at startup (`FromRef` hands an extractor one field); `Extension` only for per-request values a middleware injects — a missing one is a 500.

## Routes and handlers

Argument order is load-bearing: `FromRequestParts` extractors (`State`, `Path`, `Query`, `AuthUser`) may repeat; the one body extractor comes last. When a route stops compiling, `#[axum::debug_handler]` names the culprit.

```rust
#[tracing::instrument(skip_all)]
pub async fn create_user(
    State(state): State<AppState>,
    Valid(body): Valid<CreateUser>,          // body extractor last
) -> Result<(StatusCode, Json<UserResponse>), AppError> {
    let created = user::ActiveModel {
        id: Set(Uuid::now_v7()),
        email: Set(body.email),
        name: Set(body.name),
        created_at: Set(state.clock.now().into()),
    }
    .insert(&state.db)                       // one statement: it stays in the handler
    .await?;
    Ok((StatusCode::CREATED, Json(created.into())))
}
```

## Errors

One enum, one `IntoResponse`, one wire shape. `Router::fallback` answers 404 with `AppError::NotFound`, `method_not_allowed_fallback` 405 with an `ErrorBody`: a wrong path has the shape of every other failure, never an empty body.

| Variant | Status | Body `error` |
|---|---|---|
| `Validation(ValidationErrors)` | 422 | `"validation failed"`, `details` = field map |
| `JsonRejection(JsonDataError)` | 422 | well-formed JSON, wrong shape |
| `JsonRejection(_)` | 400 | bad syntax, wrong content-type, too large |
| `BadRequest(String)` | 400 | the message |
| `NotFound(String)` | 404 | the message |
| `Conflict(String)`, `Db(DbErr)` with SQLSTATE 23505 | 409 | constant `"conflict"`; the constraint name is logged |
| `Unauthorized` | 401 | constant |
| `TooManyRequests { retry_after_secs }` | 429 | constant; `Some` adds a `Retry-After` header |
| `Unavailable(String)` | 503 | constant `"service unavailable"`; the string is logged |
| `Db(DbErr)` otherwise, `Other(anyhow::Error)` | 500 | constant `"internal server error"` |
| `Http(reqwest_middleware::Error)` | 502 | constant `"upstream request failed"` |

`ErrorBody` is `{ "error", "request_id", "details" }` (the last two absent when `None`); `ErrorBody::new(msg)` fills `request_id` from the task-local. Every 5xx and 409 renders a constant and logs the chain once, in `into_response`.

Add-on skills never add a variant: backend down → `Unavailable`, rate limit → `TooManyRequests`, wiring bug → `Other`, bad input → `BadRequest`. The first foreign key adds one arm beside 23505: `SqlErr::ForeignKeyConstraintViolation` (23503) is a 422 with the constant `"referenced row does not exist"`; a pre-flight `SELECT` loses the race the constraint wins.

## Request DTOs and validation

`Valid<T>` is `Json<T>` plus `T::validate()`; `ValidQuery<T>` is the query-string sibling: unparseable query string → `BadRequest` (400), failed rule → 422.

```rust
#[derive(Debug, Deserialize, Validate, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateUser {
    #[validate(email)]
    #[schema(format = Email, example = "ada@example.com")]
    pub email: String,
    #[validate(length(min = 1, max = 100))]
    #[schema(min_length = 1, max_length = 100, example = "Ada Lovelace")]
    pub name: String,
}
```

House style: snake_case JSON (serde's default), `deny_unknown_fields` on every request-body DTO so a typo'd field is a 422 instead of silence, `ToSchema` on every DTO. Response DTOs are separate types with an explicit `From<Model>`; whatever the DTO declares is what ships.

## OpenAPI, settings, middleware

OpenAPI: `OpenApiRouter::with_openapi(ApiDoc::openapi())`, `.routes(routes!(..))` per path, `.merge(api::router())`, `split_for_parts()`, `SwaggerUi::new("/docs").url("/openapi.json", api)`. Nesting and the bearer scheme: `references/openapi.md`.

Settings come from the environment only (`APP__SERVER__PORT`, `__` between every level); `dotenvy::dotenv().ok()` is the first statement of `main`; secrets are `SecretString`; tests use `Settings::from_map` because `std::env::set_var` is `unsafe` in edition 2024. Details: `references/settings.md`.

Layer order has two opposite rules: inside one `ServiceBuilder` the **first** `.layer()` is outermost; chaining `.layer()` on a `Router` the **last** call is. The stack, outermost first: `api::request_id`, `error::catch_panic_layer()`, `OtelInResponseLayer`, `OtelAxumLayer`, Prometheus, CORS from `server.cors_origins` (empty means no layer; never `permissive()`), compression, `DefaultBodyLimit`, `TimeoutLayer` innermost. Everything — `/metrics`, probes, fallbacks — sits inside it. A rate limiter (`rust-redis`) is a `from_fn_with_state` on the limited subtree via `route_layer` — inside the global stack, right before the handler; `main.rs` serves with `into_make_service_with_connect_info::<SocketAddr>()` so its IP fallback has a `ConnectInfo`.

## Telemetry

`#[tracing::instrument(skip_all, fields(user_id = %id))]` on the function that does the work: `skip_all` then opt fields back in, because with `skip(state)` an argument added later silently becomes a field. `err` only on service functions, never on a handler returning `AppError`: `into_response` already logs each 5xx once, so `err` double-logs and records a 404 as an error. `%` is `Display`, `?` is `Debug`. `SecretString` redacts `Debug` only; never `%` one, and never log a whole body.

One middleware, `api::request_id`, trusts or mints `x-request-id`, opens a `request` span carrying it, scopes the `REQUEST_ID` task-local so `ErrorBody::new` copies it into every error body, and echoes the header.

`OtelAxumLayer` + `OtelInResponseLayer` are the only HTTP span source: inbound `traceparent` continued, server span named by `http.route`, context written back. `telemetry::init` always installs the tracer provider and `TraceContextPropagator`; `APP__TELEMETRY__OTLP_ENDPOINT` only decides whether an exporter is attached — unset means no exporter, and the SDK's own `OTEL_EXPORTER_OTLP_ENDPOINT` is never consulted.

Metrics are `metrics::counter!` and `histogram!` against the recorder `build_router` installs once with `PrometheusMetricLayer::pair()` (a second call panics) and serves on `/metrics`. Labels are constants: a user id or raw path multiplies the series until the scrape falls over.

## Operations

Liveness `/health/live` is 200 with no dependencies. Readiness `/health/ready` returns `{ "status", "checks": { "<name>": ... } }` where every value is `"ok"`, `"degraded"` or `"unavailable"` and `status` is the worst check: a required dependency (the database, pinged under `READINESS_TIMEOUT`) failing is `"unavailable"` and 503; an optional one (cache, messaging) failing is `"degraded"` and still 200. Add-on skills add one entry to `checks`, nothing else. `/version` returns the crate name and `CARGO_PKG_VERSION`. All three unauthenticated.

Timeout budget, four constants in `config.rs`, shortest first: `READINESS_TIMEOUT` 2 s, `DB_STATEMENT_TIMEOUT` 10 s (`ConnectOptions`), `HTTP_CLIENT_TIMEOUT` 10 s, `REQUEST_TIMEOUT` 30 s (`TimeoutLayer`), then the caller's timeout. Inverted, the client gives up while the query still runs.

Shutdown order: drain in-flight (`with_graceful_shutdown`), flush the exporter (`guard.shutdown()`), close the pool.

Pagination is `?limit=&offset=`, default 20, maximum 100 via `#[validate(range(min = 1, max = 100))]`, in a `Page<T> { items, total, limit, offset }` envelope, never a bare array.

Work that outlives the response is `tokio::spawn` with the error logged inside the task; a dropped `JoinHandle` discards it.

A handler panic is answered with the constant 500 body and logged (`error::catch_panic_layer`, directly inside the request id); the connection survives. Still a bug: return `AppError`.

## Common issues

| Symptom | Open |
|---|---|
| Spans never reach the collector | `references/telemetry.md`, Debugging: endpoint unset, `RUST_LOG` too quiet, no `guard.shutdown()`, provider built outside the runtime, port 4318 instead of 4317 |
| Trace ids differ across services | same section: missing `set_text_map_propagator`, a bare `reqwest::Client`, a `tokio::spawn` without `.instrument` |

## axum 0.7 to 0.8 corrections

| 0.7 | 0.8 |
|---|---|
| `/:id`, `/*rest` | `/{id}`, `/{*rest}` — the old form panics at router build |
| `#[async_trait]` on `FromRequest` / `FromRequestParts` | plain `async fn` in trait; the attribute is now a compile error |
| `Option<Query<T>>` swallowed every rejection | it rejects; opt in with `OptionalFromRequestParts` |
| handlers required `Send` | handlers and services also require `Sync` |
| a 405 was an empty response you could not customise | `Router::method_not_allowed_fallback` |
| `Serve::tcp_nodelay` | `serve::ListenerExt` |
| `Query` / `Form` reported "failed to deserialize" | `serde_path_to_error` names the field |

## Red Flags — STOP

| About to… | Rule to apply |
|---|---|
| Write `/users/:id` | Routes — 0.8 captures are `/{id}`; `:id` panics at router build |
| Return `Json(model)` from a handler | Request DTOs — a response DTO, or every column ships |
| Put a second statement, an if-ladder or reused logic in a handler | Architecture — one statement in the handler; more is a `services/` function |
| Add a repository trait over sea-orm | Architecture — the ORM is the abstraction |
| Expect `#[validate(range(max = 100))]` in the schema | Request DTOs — repeat it as `#[schema(maximum = 100)]` |
| Put an `anyhow` chain or a constraint name in a body | Errors — 5xx and 409 render a constant and log the chain |
| Add an `AppError` variant for a cache, broker or provider error | Errors — map onto `Unavailable`, `TooManyRequests`, `Other` |
| Add a second HTTP-span layer next to `OtelAxumLayer`, or `CorsLayer::permissive()` | Middleware — one HTTP span; origins come from settings |
| Export OTLP without `set_text_map_propagator` | Telemetry — the default propagator is a silent no-op |
| Put an id or a raw path in a metric label | Telemetry — labels are constants; ids go in span fields |

Do not flag: `Arc<Settings>` in state, `.clone()` on `DatabaseConnection` or `Client` (handles), or a handler taking `State` by value.

## References

- `references/extractors.md` — `AppError`, `ErrorBody`, `Valid` / `ValidQuery`, rejection table, argument order, the 23503 arm. Before writing the error type or an extractor.
- `references/openapi.md` — utoipa 5 wiring, `routes!`, nesting, bearer scheme, what the document omits. When documenting routes.
- `references/settings.md` — nested env keys, lists, secrecy, `.env` precedence, `from_map`. When adding a configuration value.
- `references/auth.md` — `Keys`, `Claims`, `AuthUser`, argon2 0.6 hashing. When adding authentication.
- `references/middleware.md` — `build_router`: the stack in order, fallbacks, CORS, `route_layer`, `from_fn`, the rate-limit slot. When adding or reordering a layer.
- `references/operations.md` — health and the readiness body, request id, `main.rs`, timeout budget, pagination, background work, idempotency, outbound HTTP. When wiring `main.rs` or a production concern.
- `references/streaming.md` — SSE and streaming bodies. When a response is unbounded or long-lived.
- `references/telemetry.md` — subscriber, `EnvFilter`, OTLP export, propagation, shutdown flush, missing spans. When wiring or fixing observability.
- `references/metrics.md` — `/metrics`, custom instruments, cardinality. When adding a metric.

Queries, migrations, entities: `sea-orm-postgres`. Tests: `rust-testing`. LLM endpoints: `building-rig-agents`. Rate limiting and idempotency storage: `rust-redis`.
