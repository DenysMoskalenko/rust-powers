---
name: axum-service
description: "Use when changing an axum service — routes, extractors, response DTOs, AppError statuses, utoipa OpenAPI, settings, secrets, middleware order, health checks, pagination, SSE, background tasks, shutdown, JWT auth, tracing, JSON logs, OTLP export, traceparent propagation, Prometheus metrics. Also for Handler is not satisfied, Path segments must not start with a colon, no method named tracer, spans never reaching the collector. Not for queries (sea-orm-postgres), a new service (rust-scaffolding), flaky tests (rust-testing), Cargo.toml or CI (rust-tooling)."
metadata:
  version: "0.5.0"
---

# axum service patterns

Assumes Rust 1.99 edition 2024, axum 0.8, tower-http 0.7, utoipa 6, validator 0.21, config 0.15, secrecy 0.10, reqwest 0.13, tracing 0.1 with opentelemetry 0.32, axum-prometheus 0.10, jsonwebtoken 11, argon2 0.6.

Names such as `AppError`, `test_app()`, `Valid<T>` and the Makefile targets come from the rust-scaffolding template. In a project built differently, use its own types, helpers and tooling, map outcomes onto its nearest existing error variant, and say so when none fits instead of adding one. Apply these rules to new code; when editing existing code, keep its public contract and tuned configuration and report differences instead of rewriting, unless asked. If `Cargo.lock` pins another major or minor version than the line above, follow the project and say which rules may not apply.

## Important

- Handlers return `Result<_, AppError>`; `error.rs` is the only file that chooses an error status, and a 5xx body is a constant. Never `panic!` to signal failure.
- Every validator rule a client needs is repeated as `#[schema(...)]` or `#[param(...)]`; utoipa never reads `validate`.
- `#[tracing::instrument(skip_all, fields(..))]` on the function that does the work; `err` only where an error never reaches `into_response`, which already logs each 5xx once.
- Serve `app.into_make_service_with_connect_info::<SocketAddr>()` or `into_make_service()`, never a bare `Router`: axum 0.8.9 deep-copies it, handler captures included, per open connection.

## References

- `references/extractors.md` — `AppError`, `ErrorBody`, `Valid` / `ValidQuery` / `Path`, rejection table, argument order, the 23503 arm. Before writing the error type or an extractor.
- `references/openapi.md` — utoipa 6 wiring, `routes!`, nesting, bearer scheme, a `description` on every response, OpenAPI 3.2 for SSE, what the document omits. When documenting routes.
- `references/settings.md` — nested env keys, lists, secrecy, `.env` precedence, `from_map`. When adding a configuration value.
- `references/auth.md` — `Keys`, `Claims`, `AuthUser`, guarding a subtree, jsonwebtoken's crypto feature, argon2 0.6 hashing. When adding authentication.
- `references/middleware.md` — `build_router`: the stack in order, the panic catcher, fallbacks, the 503 request timeout, CORS, `route_layer`, `from_fn`, the rate-limit slot. When adding or reordering a layer.
- `references/operations.md` — health and the readiness body, request id, `main.rs`, timeout budget, pagination, background work, idempotency, outbound HTTP, memory after load. When wiring `main.rs` or a production concern.
- `references/streaming.md` — SSE and streaming bodies. When a response is unbounded or long-lived.
- `references/telemetry.md` — subscriber, `EnvFilter`, OTLP export, propagation, shutdown flush, missing spans. When wiring or fixing observability.
- `references/metrics.md` — `/metrics`, custom instruments, cardinality. When adding a metric.

Queries, migrations, entities: `sea-orm-postgres`. Tests: `rust-testing`. LLM endpoints: `building-rig-agents`. Rate limiting and idempotency storage: `rust-redis`.

## Architecture

A handler extracts, runs at most one query directly, and maps the result to a response DTO; a page's `count` and fetch are one query. Anything more, or logic a second caller needs, moves to a `services/` function. No repository layer — sea-orm is already the abstraction over SQL; a trait on top buys one implementation and a mock that proves nothing.

```text
src/
  main.rs      wiring only: dotenv, Settings::load, telemetry, migrate, pool, AppState, serve, shutdown
  lib.rs       AppState, ApiDoc, build_router(state) -> Router: the stack and both fallbacks
  config.rs    Settings sub-structs, the five timeout constants
  error.rs     AppError + ErrorBody + IntoResponse; REQUEST_ID task-local
  extract.rs   Valid, ValidQuery, Path
  telemetry.rs init(&TelemetrySettings) -> TelemetryGuard
  auth.rs      Keys, Claims, AuthUser, hashing (with the first protected route)
  api/         mod.rs (router(), request_id, request_timeout), health.rs, users.rs (DTOs, handlers, router())
  services/    multi-query or shared logic; the scaffold has none yet
```

## State

`AppState` is cheap to clone — every field is a handle. While that holds, never `Arc<AppState>`: it forces `State<Arc<AppState>>` everywhere and gives up `FromRef`.

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

`FromRequestParts` extractors (`State`, `Path`, `Query`, `AuthUser`) may repeat before the body extractor. When a route stops compiling, `#[axum::debug_handler]` names the culprit.

```rust
#[tracing::instrument(skip_all)]
pub async fn create_user(
    State(state): State<AppState>,
    Valid(body): Valid<CreateUser>,          // body extractor last
) -> Result<(StatusCode, Json<UserResponse>), AppError> {
    let created = user::ActiveModel {
        id: NotSet,
        email: Set(body.email),
        name: Set(body.name),
        created_at: Set(state.clock.now().into()),
    }
    .insert(&state.db)                       // one query: it stays in the handler
    .await?;
    Ok((StatusCode::CREATED, Json(created.into())))
}
```

## Errors

`Router::fallback` answers 404 with `AppError::NotFound`, `method_not_allowed_fallback` 405 with an `ErrorBody`, so a wrong path has the shape of every other failure.

| Variant | Status | Body `error` |
|---|---|---|
| `Validation(ValidationErrors)` | 422 | `"validation failed"`, `details` = field map |
| `JsonRejection(JsonDataError)` | 422 | well-formed JSON, wrong shape |
| `JsonRejection(JsonSyntaxError)` | 400 | malformed JSON |
| `JsonRejection(BytesRejection)` | 413 | over `DefaultBodyLimit` |
| `JsonRejection(MissingJsonContentType)` | 415 | no `application/json` |
| `BadRequest(String)` | 400 | the message |
| `NotFound(String)` | 404 | the message |
| `Conflict(String)`, `Db(DbErr)` with SQLSTATE 23505 | 409 | constant `"conflict"`; the constraint name is logged |
| `Db(DbErr)` with SQLSTATE 23503 | 422 | constant `"invalid reference"` |
| `Unauthorized` | 401 | constant |
| `TooManyRequests { retry_after_secs }` | 429 | constant; `Some` adds a `Retry-After` header |
| `Unavailable(String)`, `Db(DbErr)` without a connection, the request timeout | 503 | constant `"service unavailable"`; the cause is logged |
| `Db(DbErr)` otherwise, `Other(anyhow::Error)` | 500 | constant `"internal server error"` |
| `Http(reqwest_middleware::Error)` | 502 | constant `"upstream request failed"` |

`ErrorBody` is `{ "error", "request_id", "details" }` (the last two absent when `None`); `ErrorBody::new(msg)` fills `request_id` from the task-local. `into_response` logs once, with the chain: a 5xx at ERROR, a 409 or a constraint rejection at WARN.

Add-on skills never add a variant: backend down → `Unavailable`, rate limit → `TooManyRequests`, wiring bug → `Other`, bad input → `BadRequest`. A missing parent row (23503) takes its status from the parent id's source: from the path it is `NotFound`, which the service returns by classifying the `DbErr` (`sea-orm-postgres` owns how); from the body the `DbErr` reaches the 422 row; never a pre-flight `SELECT`, which loses the race the constraint wins.

## Request DTOs and validation

`Valid<T>` is `Json<T>` plus `T::validate()`; `ValidQuery<T>` is the query-string sibling: unparseable query string → `BadRequest` (400), failed rule → 422. `Path<T>` is the third, mapping `PathRejection` onto `BadRequest` so a bad id is an `ErrorBody` and not axum's plain-text `Invalid URL: ...`; a bare `axum::extract::Path` is the one extractor that breaks the shape.

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

House style: snake_case JSON (serde's default), `ToSchema` on every DTO, and `deny_unknown_fields` on every new request-body DTO so a typo'd field is a 422 instead of silence. Never add it to an existing public DTO: clients that already send an extra field start failing.

## OpenAPI, settings, middleware

OpenAPI: `OpenApiRouter::with_openapi(ApiDoc::openapi())`, `.routes(routes!(..))` per path, `.merge(api::router())`, `split_for_parts()`, `SwaggerUi::new("/docs").url("/openapi.json", api)`.

Settings come from the environment only (`APP__SERVER__PORT`, `__` between every level); `dotenvy::dotenv().ok()` is the first statement of `main`; secrets are `SecretString`; tests use `Settings::from_map` because `std::env::set_var` is `unsafe` in edition 2024.

Layer order has two opposite rules: inside one `ServiceBuilder` the **first** `.layer()` is outermost; chaining `.layer()` on a `Router` the **last** call is. The stack, outermost first: `OtelAxumLayer`, `OtelInResponseLayer`, `api::request_id`, `error::catch_panic_layer()`, Prometheus, CORS from `server.cors_origins` (empty means no layer; never `permissive()`), compression, `DefaultBodyLimit`, `api::request_timeout` innermost. Everything — `/metrics`, probes, fallbacks, streaming routes — sits inside it.

## Telemetry

`#[tracing::instrument(skip_all, fields(user_id = %id))]`: `skip_all` then opt fields back in, because with `skip(state)` an argument added later silently becomes a field. `SecretString` has no `Display` and redacts `Debug`, so the leak is `%secret.expose_secret()`; never log a whole body.

One middleware, `api::request_id`, keeps an inbound `x-request-id` of 1–64 `[A-Za-z0-9._-]` characters or mints one, opens a `request` span carrying it, scopes the `REQUEST_ID` task-local, and echoes the header.

`OtelAxumLayer` then `OtelInResponseLayer` are the only HTTP span source, outermost and in that order: inbound `traceparent` continued, server span named by `http.route`, the `request` span its child, the same trace written back. `telemetry::init` always installs the tracer provider and `TraceContextPropagator`; `APP__TELEMETRY__OTLP_ENDPOINT` only decides whether an exporter is attached — unset means no exporter, and the SDK's own `OTEL_EXPORTER_OTLP_ENDPOINT` is never consulted.

Metrics are `metrics::counter!` and `histogram!` against the process-wide recorder `build_router` installs once through a `OnceLock` (a second `PrometheusMetricLayer::pair()` panics) and serves on `/metrics`; an unmatched path is labelled `unmatched`. Labels are constants: a user id or raw path multiplies the series.

## Operations

Liveness `/health/live` is 200 with no dependencies. Readiness `/health/ready` returns `{ "status", "checks": { "<name>": ... } }` where every value is `"ok"`, `"degraded"` or `"unavailable"` and `status` is the worst check: a required dependency (the database, pinged under `READINESS_TIMEOUT`) failing is `"unavailable"` and 503; an optional one (cache, messaging) failing is `"degraded"` and still 200. Add-on skills add one entry to `checks`. `/version`: build info; all unauthenticated.

Timeout budget, five constants in `config.rs`, shortest first: `READINESS_TIMEOUT` 2 s, `DB_STATEMENT_TIMEOUT` 10 s (`ConnectOptions`), `HTTP_CLIENT_TIMEOUT` 10 s, `REQUEST_TIMEOUT` 30 s (`api::request_timeout`), then the caller's timeout; `SHUTDOWN_TIMEOUT` 35 s, the drain deadline, under the orchestrator's grace period. Inverted, the client gives up while the query still runs.

Shutdown order: drain in-flight (`with_graceful_shutdown`) until `SHUTDOWN_TIMEOUT`, flush the exporter (`guard.shutdown()`), close the pool.

A new list endpoint pages with `?limit=&offset=`, default 20, maximum 100 via `#[validate(range(min = 1, max = 100))]`, `offset` capped at `MAX_OFFSET` (10 000; the driver panics past `i64::MAX`), in a `Page<T> { items, total, limit, offset }` envelope, never a bare array. An existing list keeps the shape its clients parse.

Work that outlives the response is `tokio::spawn` with the error logged inside the task; a dropped `JoinHandle` discards it.

## axum 0.7 to 0.8 corrections

| 0.7 | 0.8 |
|---|---|
| `/:id`, `/*rest` | `/{id}`, `/{*rest}` — the old form panics at router build |
| `#[async_trait]` on `FromRequest` / `FromRequestParts` | plain `async fn` in trait; the attribute is now a compile error |
| `Option<Query<T>>` swallowed every rejection | does not compile; `Option` or `#[serde(default)]` fields, or `axum_extra::extract::OptionalQuery` |
| handlers required `Send` | handlers and services also require `Sync` |
| a 405 was an empty response you could not customise | `Router::method_not_allowed_fallback` |
| `Serve::tcp_nodelay` | `serve::ListenerExt` |

## Red Flags — STOP

| About to… | Rule to apply |
|---|---|
| Return `Json(model)`, or put `#[serde(skip_serializing)]` on an entity | Architecture — a response DTO with `From<Model>`; the model ships `password_hash`, and codegen overwrites entity attributes |
| Put `Json` or `Valid` before another extractor | Routes — the body extractor goes last, or the only error is `Handler` is not satisfied |
| Put a second query, an if-ladder or reused logic in a handler | Architecture — one query in the handler; more is a `services/` function |
| Add a repository trait over sea-orm | Architecture — the ORM is the abstraction |
| Expect `#[validate(range(max = 100))]` in the schema | Request DTOs — repeat it as `#[schema(maximum = 100)]` |
| Put an `anyhow` chain or a constraint name in a body | Errors — 5xx and 409 render a constant and log the chain |
| Add an `AppError` variant for a cache, broker or provider error | Errors — map onto `Unavailable`, `TooManyRequests`, `Other` |
| Add a second HTTP-span layer next to `OtelAxumLayer`, or `CorsLayer::permissive()` | Middleware — one HTTP span; origins come from settings |
| Export OTLP without `set_text_map_propagator` | Telemetry — the default propagator is a silent no-op |
| Put an id or a raw path in a metric label | Telemetry — labels are constants; ids go in span fields |
| Pass `axum::serve` a bare `Router`, or let a handler own a large value | `references/operations.md` — the make-service form; `Bytes` or `Arc` in state, as handlers clone per request |

Do not flag: `Arc<Settings>` in state, `.clone()` on `DatabaseConnection` or `Client` (handles), or a handler taking `State` by value.
