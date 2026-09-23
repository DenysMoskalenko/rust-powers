# axum-service

### Triggering

**Should load**

1. "Add `GET /orders` that returns orders twenty at a time, with the page size and offset showing up in Swagger."
2. "This handler returns the whole user row including `password_hash` — how do I keep that field off the wire?"
3. "Getting `error[E0277]: the trait bound `fn(Valid<CreateUser>, State<AppState>) -> impl Future {create_user}: Handler<_, _>` is not satisfied` on my route."
4. "The service panics at startup: `Path segments must not start with `:`. For capture groups, use `{capture}`.`"
5. "Send the confirmation email after the 202 goes out, without making the caller wait for it."
6. "Our axum service logs fine locally but nothing shows up in Tempo. We set the OTLP endpoint and the collector is up. Where do I even start looking?"
7. "The checkout service calls the billing service and they end up as two separate traces in Jaeger instead of one. Both are Rust, both use axum."
8. "In production our logs should be one JSON object per line for Loki, but locally I want them readable, and `RUST_LOG` should still work. How do I set that up?"
9. "Adding a Prometheus `/metrics` endpoint to our axum app and a counter for orders by status. What naming convention should I use, and is it safe to add the customer id as a label?"
10. "Pasted error: `error[E0599]: no method named 'tracer' found for struct 'SdkTracerProvider' in the current scope` — this is in my `init_telemetry` function."

**Should not load**

1. "Add a migration that puts a foreign key from `posts.user_id` to `users.id`, then regenerate the entities." -> `sea-orm-postgres`
2. "Write an API test that creates a user twice and asserts the duplicate is rejected." -> `rust-testing`
3. "Move the confirmation emails onto a durable queue with a worker that retries, so a restart does not lose them." -> `rust-nats`
4. "Add a Dockerfile and a CI job that runs clippy and nextest." -> `rust-tooling`
5. "Give the agent a tool that looks up an order and return its answer." -> `building-rig-agents`
6. "`cargo deny check` fails on `axum-tracing-opentelemetry` with a CC0-1.0 licence error." -> `rust-tooling`
7. "How do I find the N+1 in this sea-orm query? The trace shows forty spans for one request." -> `sea-orm-postgres`
8. "I get `future cannot be sent between threads safely` on a function I just put `#[instrument]` on." -> `rust-code-style`
9. "Start a new axum service for invoices from an empty directory, with Postgres, tracing and a health check already wired." -> `rust-scaffolding`
10. "Snapshot the 422 body of `POST /users` with `insta`, redacting the request id so the snapshot stays stable." -> `rust-testing`

### Eval 1 - a new endpoint

**Prompt**: "Add `POST /orders` that validates the body (`customer_email` must be an email, `quantity` between 1 and 50) and returns 201 with the created order, plus a `GET` that fetches one order by its id."

**Must produce**:

- The create handler returns `Result<(StatusCode, Json<OrderResponse>), AppError>` and answers 201.
- `Valid<CreateOrder>` is the last argument of the create handler.
- `CreateOrder` carries `#[serde(deny_unknown_fields)]`.
- `customer_email` carries `#[validate(email)]`.
- `quantity` carries `#[validate(range(min = 1, max = 50))]`.
- The quantity bounds are repeated as `#[schema(...)]` constraints, so the OpenAPI document shows them.
- `#[utoipa::path(post, path = "/orders", ...)]` documents the 422 response with `body = ErrorBody`.
- The routes are registered through `routes!` on an `OpenApiRouter<AppState>` merged into `api::router()`.
- The fetch route is spelled `/orders/{id}`.
- The fetch handler takes the id through the crate's own `Path` extractor, not a bare `axum::extract::Path`.
- A separate `OrderResponse` DTO with an explicit `From` conversion from the entity model.
- `#[tracing::instrument(skip_all, ...)]` on the handlers or the service functions, never `skip(state)`.

**Must not produce**:

- `/orders/:id`, or any other `/:name` capture.
- `#[async_trait]` on a `FromRequest` or `FromRequestParts` impl.
- A repository trait or an `OrderRepository`.
- Multi-statement logic or an if-ladder inline in a handler.
- `Json(order_model)`, returning the entity itself.
- A status mapping for rejected bodies outside `error.rs`.

### Eval 2 - status codes and leaking fields

**Prompt**: "A colleague added `GET /users/{id}/profile` that returns `Json<Option<user::Model>>` — the whole row, `null` on a miss. `user::Model` now has a `password_hash` column. Hide it and return 404 when the user does not exist."

The fixture has neither the column nor the endpoint, so the answer writes the fixed handler from this description; it is judged on that handler, not on the scaffold's `get_user`, which already follows both rules.

**Must produce**:

- A response DTO (`ProfileResponse`, or the existing `UserResponse`) with an explicit `From<user::Model>` that omits `password_hash`.
- A miss returns `AppError::NotFound(..)` from `find_by_id(..).one(..).await?.ok_or_else(..)`, answered 404 by the existing `IntoResponse`.
- The `NotFound` message is the full sentence (`format!("user {id} not found")`), not a bare noun.
- The id is taken through the crate's own `Path<Uuid>` (`crate::extract::Path`), so `/users/not-a-uuid` is a 400 in the `ErrorBody` shape.
- `#[tracing::instrument(skip_all, fields(user_id = %id))]` on the handler, without `err`.
- The 404 is documented as `(status = 404, body = ErrorBody)`.

**Must not produce**:

- `Json(model)` or `Json<Option<_>>` carrying the entity.
- `#[serde(skip_serializing)]` on `password_hash` in the entity (codegen overwrites it).
- A bare `axum::extract::Path<Uuid>` in the handler.
- A 404 built with a bare `StatusCode` tuple that bypasses `AppError`.
- `panic!` or `unwrap` on the missing row.
- `#[instrument(err)]` on the handler.
- A second error-to-status mapping outside `error.rs`.

### Eval 3 - settings and secrets

**Prompt**: "Add a required `APP__BILLING__API_KEY` secret for the billing client, and a test that the other settings keep their defaults when it is set."

**Must produce**:

- `api_key: SecretString` in a `BillingSettings` sub-struct, read from `APP__BILLING__API_KEY`.
- A test that calls `Settings::from_map(...)` with an in-memory map.
- `expose_secret()` called only at the point of use.

**Must not produce**:

- `std::env::set_var`, an `unsafe` block, or `#[allow(unsafe_code)]` in the test.
- A `config.toml` or any other file source.
- A plain `String` for the secret.
- The secret printed in a log line or an assertion message.

### Eval 4 - work after the response

**Prompt**: "Send the confirmation email after we return 202, and make sure a failure is not silent."

**Must produce**:

- `tokio::spawn`, with the values the task needs moved or cloned out of the state first.
- The error handled inside the task with `tracing::error!`.
- The spawned task instrumented with a span.
- A note that the work is lost if the process shuts down mid-send.
- An outbox table or a durable queue named as the fix when a lost email is unacceptable.

**Must not produce**:

- The `JoinHandle` awaited inside the handler, which defeats the point.
- A discarded handle with no error logging inside the task.
- A blocking send before the response.
- A new background-task abstraction layer.

### Eval 5 - wiring telemetry from nothing

**Prompt**: "Set up telemetry for my axum service: JSON logs in production, OTLP traces to our collector over gRPC, and a `/metrics` endpoint."

**Fixture**: empty

**Must produce**:

- A single `telemetry::init` returning a guard, with every `global::set_*` and the subscriber `init` reached only from it; a private helper such as `build_tracer_provider` counts.
- `global::set_text_map_propagator(TraceContextPropagator::new())`.
- The reason for the propagator: the default global propagator is a no-op.
- `use opentelemetry::trace::TracerProvider as _;`.
- `Resource::builder()` with no hardcoded service name.
- `guard.shutdown()` called after `axum::serve` returns, not before.
- `OtelAxumLayer` + `OtelInResponseLayer` as the only HTTP span source.
- `/metrics` from one `PrometheusMetricLayer::pair()` call in `build_router`.

**Must not produce**:

- `.with_sampler(..)` on the provider.
- `opentelemetry_sdk` with the `rt-tokio` feature.
- `global::shutdown_tracer_provider()`.
- A second HTTP tracing layer (tower-http's `trace` feature) in the same stack as `OtelAxumLayer`.
- A provider built in a `LazyLock`, a `OnceLock` or a `static`.
- `PrometheusMetricLayer::pair()` called more than once.
- `opentelemetry-otlp` with `default-features = false` and only `grpc-tonic` listed.

### Eval 6 - instrumenting a function that handles credentials

**Prompt**: "Add tracing to this function so I can see failures in the backend: `pub async fn rotate_key(db: &DatabaseConnection, user_id: Uuid, current_secret: SecretString, new_secret: SecretString) -> Result<(), AppError>`"

**Must produce**:

- `#[instrument(skip_all, fields(user_id = %user_id))]`, or another form that opts fields back in explicitly.
- The reason `err` stays off: the returned `AppError` reaches `into_response`, which already logs every 5xx once.

**Must not produce**:

- A bare `#[instrument]`, or `skip(db)`, which leaves the secrets as fields.
- `current_secret.expose_secret()` or `new_secret.expose_secret()` in a span field or a log line.
- `err` in the `#[instrument]` attribute.
- A second log of the same error at the call site.
- The claim that `%` on a `SecretString` prints the secret: it has no `Display`, so `%current_secret` does not compile.

### Eval 7 - a metric label that would melt Prometheus

**Prompt**: "I added `counter!(\"api_requests_total\", \"path\" => req.uri().path().to_string(), \"user\" => user.email.clone()).increment(1);` to a middleware in our axum service and Prometheus is now using 40 GB of RAM. What happened?"

**Fixture**: empty

**Must produce**:

- Cardinality named as the cause: one time series per distinct label combination, so a raw path and an email address create unbounded series.
- The path label replaced by the route template from `MatchedPath`.
- The email label dropped entirely.
- Per-request identifiers (a user id, the raw path) moved to span fields or logs, where cardinality costs nothing.

**Must not produce**:

- A fix on the Prometheus side that keeps the labels: more memory, shorter retention, or sampling.
- Hashing or truncating the email so it is "safe" as a label.

### Eval 8 - the request id and the error body

**Prompt**: "Support tells me users quote a request id from the error body, but ours is always missing. We have `SetRequestIdLayer` and `PropagateRequestIdLayer` in the stack."

**Fixture**: empty

**Must produce**:

- One `from_fn` middleware (`api::request_id`) as the outermost layer.
- `api::request_id` trusts an inbound `x-request-id` or mints one, and echoes it on the response.
- `api::request_id` opens `info_span!("request", request_id = %id)` and `.instrument`s the rest of the request.
- `api::request_id` scopes the `REQUEST_ID` task-local declared in `error.rs`.
- `ErrorBody::new` reads the task-local, so every error, the 404 fallback included, carries the id.

**Must not produce**:

- A second `tokio::task_local!` outside `error.rs`.
- `Span::current().record("request_id", ..)` onto a span that does not declare the field.
- Tower-http's two request-id layers kept next to the `from_fn`.

### Probe 1 - path capture syntax

**Prompt**: "Add a `GET /posts/:id` route that returns one post as JSON."

**Wrong answer**: `(\.route\(\s*|path\s*=\s*)"[^"]*/:\w`

**Right answer**: `(\.route\(\s*|path\s*=\s*)"[^"]*/\{\w+\}`

### Probe 2 - an extractor without async_trait

**Prompt**: "Write an extractor that reads the `x-tenant-id` header into a `TenantId` and rejects a missing header with 400."

**Wrong answer**: `#\[\s*(async_trait::)?async_trait\s*\]\s*impl\b`

### Probe 3 - OTLP export on current APIs

**Prompt**: "Wire OTLP gRPC trace export so one trace continues across my two axum services."

**Fixture**: empty

**Wrong answer**: `\.install_batch\s*\(|new_pipeline\(\)|shutdown_tracer_provider\(\);|"rt-tokio"|\.with_service_name\(\s*"`

**Right answer**: `set_text_map_propagator\s*\(\s*TraceContextPropagator::new\(\)\s*\)`

### Probe 4 - CORS from settings

**Prompt**: "Our React dev server on `http://localhost:5173` gets CORS errors when it calls the API. Fix it."

**Wrong answer**: `(\.layer\(|=|Some\()\s*CorsLayer::(very_)?permissive\(\)`

**Right answer**: `(APP__SERVER__CORS_ORIGINS|cors_origins)`
