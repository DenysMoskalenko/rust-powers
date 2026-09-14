# axum-service

### Triggering

**Should load**

1. "Add a paginated list endpoint `GET /api/v1/orders` and document its query parameters in the OpenAPI spec."
2. "This handler returns the whole user row including `password_hash` — how do I keep that field off the wire?"
3. "Getting `error[E0277]: the trait bound `fn(Valid<CreateUser>, State<AppState>) -> impl Future {create_user}: Handler<_, _>` is not satisfied` on my route."
4. "The service panics at startup: `Path segments must not start with `:`. For capture groups, use `{capture}`.`"
5. "Send the confirmation email after the 202 goes out, without making the caller wait for it."
6. "Our axum service logs fine locally but nothing shows up in Tempo. We set the OTLP endpoint and the collector is up. Where do I even start looking?"
7. "The checkout service calls the billing service and they end up as two separate traces in Jaeger instead of one. Both are Rust, both use axum."
8. "I want structured JSON logs in prod and the pretty format locally, plus `RUST_LOG` working. How do I set up the tracing subscriber?"
9. "Adding a Prometheus `/metrics` endpoint to our axum app and a counter for orders by status. What naming convention should I use, and is it safe to add the customer id as a label?"
10. "Pasted error: `error[E0599]: no method named 'tracer' found for struct 'SdkTracerProvider' in the current scope` — this is in my `init_telemetry` function."

**Should not load**

1. "Why is this N+1? Every row in the list does its own `SELECT`." -> `sea-orm-postgres`
2. "Write an API test that creates a user twice and asserts the duplicate is rejected." -> `rust-testing`
3. "`error[E0502]: cannot borrow `self.items` as mutable because it is also borrowed as immutable`." -> `rust-code-style`
4. "Add a Dockerfile and a CI job that runs clippy and nextest." -> `rust-tooling`
5. "Give the agent a tool that looks up an order and return its answer." -> `building-rig-agents`
6. "`cargo deny check` fails on `axum-tracing-opentelemetry` with a CC0-1.0 licence error." -> `rust-tooling`
7. "How do I find the N+1 in this sea-orm query? The trace shows forty spans for one request." -> `sea-orm-postgres`
8. "I get `future cannot be sent between threads safely` on a function I just put `#[instrument]` on." -> `rust-code-style`
9. "Add the opentelemetry deps to `Cargo.toml` and set up the CI job that runs the lint suite." -> `rust-tooling`
10. "My integration test is flaky — sometimes the `/metrics` assertion passes and sometimes it doesn't, depending on which tests ran first." -> `rust-testing`

### Eval 1 - a new endpoint

**Prompt**: "Add `POST /api/v1/orders` that validates the body (`customer_email` must be an email, `quantity` between 1 and 50) and returns 201 with the created order."

**Must produce**:
- A handler returning `Result<(StatusCode, Json<OrderResponse>), AppError>` with the body extractor (`Valid<CreateOrder>`) as the **last** argument.
- `CreateOrder` deriving `Deserialize, Validate, ToSchema` with `#[serde(deny_unknown_fields)]`, `#[validate(email)]` and `#[validate(range(min = 1, max = 50))]`, and the same bounds repeated as `#[schema(...)]`.
- `#[utoipa::path(post, path = "/orders", ...)]` with a 422 response documented as `body = ErrorBody`, registered through `routes!` on an `OpenApiRouter<AppState>`.
- Every failure answered by the existing `AppError`, whose `JsonRejection` arm defers to `rejection.status()`: 422 for the wrong shape, 400 for bad syntax, 413 over `DefaultBodyLimit`, 415 for a missing `application/json`.
- `#[tracing::instrument(skip_all, ...)]` on the handler or the service function, never `skip(state)`.
- A separate `OrderResponse` DTO; the single `insert` may sit in the handler, and anything beyond one statement (a lookup then an insert, reused logic) moves to a `services/` function.

**Must not produce**: `/orders/:id` style paths, `#[async_trait]` on anything, a repository trait or `OrderRepository`, multi-statement logic or an if-ladder inline in the handler, or `Json(order_model)` returning the entity.

### Eval 2 - status codes and leaking fields

**Prompt**: "A colleague added `GET /users/{id}/profile` that returns `Json<Option<user::Model>>` — the whole row, `null` on a miss. `user::Model` now has a `password_hash` column. Hide it and return 404 when the user does not exist."

The scaffold's `get_user` already satisfies the DTO and 404 rules, so the runner adds the `password_hash` column and the regressed endpoint (or asks for a prose answer); the eval is judged on the new handler, not on `get_user`.

**Must produce**:
- A `ProfileResponse` DTO (or the existing `UserResponse`) with an explicit `From<user::Model>` that omits `password_hash`; the entity file untouched.
- `AppError::NotFound(...)` from the handler's `find_by_id(..).one(..).await?.ok_or_else(..)` (one statement, so it stays in the handler), mapped to 404 by the single `IntoResponse` impl, with the full message passed in (`format!("user {id} not found")`).
- The id taken through the crate's own `Path<Uuid>` (`crate::extract::Path`), whose `PathRejection` becomes `AppError::BadRequest` — so `/users/not-a-uuid` is a 400 in the `ErrorBody` shape, not axum's plain-text `Invalid URL: ...`.
- `#[tracing::instrument(skip_all, fields(user_id = %id))]` without `err`, the 404 documented as `(status = 404, body = ErrorBody)`.

**Must not produce**: `Json(model)` or `Json<Option<_>>` with the entity, `#[serde(skip_serializing)]` on `password_hash` in the entity (codegen overwrites it), a bare `axum::extract::Path<Uuid>` in the handler, a 404 built with a bare `StatusCode` tuple that bypasses `AppError`, `panic!` or `unwrap` on the missing row, `#[instrument(err)]` on the handler, or a second error-to-status mapping outside `error.rs`.

### Eval 3 - settings and secrets

**Prompt**: "Where does the JWT secret come from, and how do I test that the defaults still apply when it is set?"

**Must produce**:
- `jwt_secret: SecretString` inside an `AuthSettings` sub-struct, read from `APP__AUTH__JWT_SECRET`.
- `config::Environment::with_prefix("APP").prefix_separator("__").separator("__").try_parsing(true)`.
- A test that calls `Settings::from_map(...)` with an in-memory map, and `expose_secret()` only at the point of use.

**Must not produce**: `std::env::set_var` (or an `unsafe` block, or `#[allow(unsafe_code)]`), a `config.toml` or any file source, a plain `String` for the secret, or the secret printed in a log or assertion message.

### Eval 4 - work after the response

**Prompt**: "Send the confirmation email after we return 202, and make sure a failure is not silent."

**Must produce**:
- `tokio::spawn` with the values the task needs moved or cloned out of the state first.
- The error handled **inside** the task with `tracing::error!`, and the task instrumented with a span.
- A note that the work is lost if the process shuts down, with durable delivery pointed at a queue or outbox table.

**Must not produce**: a `JoinHandle` awaited inside the handler (which defeats the point), a discarded handle with no error logging, a blocking send before the response, or a new background-task abstraction layer.

### Eval 5 - wiring telemetry from nothing

**Prompt**: "Set up telemetry for my axum service: JSON logs in production, OTLP traces to our collector over gRPC, and a `/metrics` endpoint. I'm on opentelemetry 0.32."

**Must produce**:
- A single `telemetry::init` returning a guard, with every `global::set_*` and subscriber `init` inside it.
- `global::set_text_map_propagator(TraceContextPropagator::new())`, with the reason (the default global propagator is a no-op).
- `use opentelemetry::trace::TracerProvider as _;`.
- `Resource::builder()` with no hardcoded service name, and no `.with_sampler(..)`.
- `guard.shutdown()` called after `axum::serve` returns, not before.
- `OtelAxumLayer` + `OtelInResponseLayer` as the only HTTP span source.
- `/metrics` from one `PrometheusMetricLayer::pair()` call in `build_router`.

**Must not produce**:
- `opentelemetry_sdk` with the `rt-tokio` feature, or `global::shutdown_tracer_provider()`.
- A second HTTP tracing layer (tower-http's `trace` feature) in the same stack as `OtelAxumLayer`.
- A provider built in a `LazyLock` / `OnceLock` / `static`, or `PrometheusMetricLayer::pair()` called more than once.
- `opentelemetry-otlp` with `default-features = false` and only `grpc-tonic` listed.

### Eval 6 - instrumenting a function that handles credentials

**Prompt**: "Add tracing to this function so I can see failures in the backend:
`pub async fn rotate_key(db: &DatabaseConnection, user_id: Uuid, current_secret: SecretString, new_secret: SecretString) -> Result<(), AppError>`"

**Must produce**:
- `#[instrument(skip_all, fields(user_id = %user_id), err)]` or an equivalent that opts fields back in explicitly.
- An explanation that `err` fires only on `Err` and produces the `error` field the OTel exception mapping needs.

**Must not produce**:
- A bare `#[instrument]`, or `skip(db)` that leaves the secrets as fields.
- `%current_secret` or `?current_secret` anywhere (`secrecy` redacts `Debug`, and `Display` prints the secret).
- A second log of the same error at the call site.

### Eval 7 - a metric label that would melt Prometheus

**Prompt**: "I added `counter!(\"api_requests_total\", \"path\" => req.uri().path().to_string(), \"user\" => user.email.clone()).increment(1);` to a middleware and Prometheus is now using 40 GB of RAM. What happened?"

**Must produce**:
- Cardinality named as the cause: one time series per distinct label combination, so a raw path and an email address create unbounded series.
- The replacement: the route template from `MatchedPath`, method, status, and a fixed outcome enum; drop the email entirely.
- The distinction that these values are fine as span fields on `#[instrument]`, because a trace holds one request while a metric holds all of them.

**Must not produce**:
- Advice to raise the Prometheus memory limit, shorten retention, or keep the labels behind a sampling flag.
- A suggestion to hash or truncate the email so it is "safe" as a label.

### Eval 8 - the request id and the error body

**Prompt**: "Support tells me users quote a request id from the error body, but ours is always missing. We have `SetRequestIdLayer` and `PropagateRequestIdLayer` in the stack."

**Must produce**:
- One `from_fn` middleware (`api::request_id`) as the outermost layer that trusts or mints `x-request-id`, opens `info_span!("request", request_id = %id)` and `.instrument`s the rest of the request, scopes the `REQUEST_ID` task-local declared in `error.rs`, and echoes the header.
- `ErrorBody::new` reading the task-local, so every error, the 404 fallback included, carries the id.

**Must not produce**:
- A second `tokio::task_local!` outside `error.rs`.
- `Span::current().record("request_id", ..)` onto a span that does not declare the field.
- Keeping tower-http's two request-id layers next to the `from_fn`.
