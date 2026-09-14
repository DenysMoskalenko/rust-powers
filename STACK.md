# Rust web stack

One pick per concern. Decided 13 September 2026.

Toolchain: Rust 1.98, edition 2024, resolver 3.
Re-validated on rustc 1.98.1 / clippy 0.1.98, 13 September 2026: `cargo generate-lockfile` (895 packages), `cargo check --workspace --all-targets`, `cargo clippy --workspace --all-targets -- -D warnings` with the `[lints]` table below active (zero warnings), and `cargo deny check` (advisories, bans, licenses, sources all ok). The skeleton wires settings, `AppError`, `Valid<T>`/`ValidQuery<T>`, sea-orm + a `migration/` crate, OTLP traces with the W3C propagator and the axum OTel middleware, a traced reqwest client, Prometheus metrics and Swagger UI. Every "add when needed" crate (rig, rmcp, axum-extra, jsonwebtoken, argon2, nutype) is resolved and compiled in the same lock file. The messaging and cache crates (async-nats 0.50, redis 1.7, deadpool-redis 0.23) were compiled clippy-clean and tested against Docker NATS 2.12 / Redis 8 in separate scratch crates on 13–14 September 2026.

## Python to Rust

| Python | Rust | Note |
|---|---|---|
| uv | cargo + rustup + `rust-toolchain.toml` | built in |
| ruff | rustfmt + clippy (`pedantic`) | built in |
| ty | rustc | the compiler is the type checker |
| complexipy | clippy `cognitive_complexity`, `too_many_lines`, `too_many_arguments` | no extra tool |
| pytest | cargo-nextest + rstest | nextest skips doctests; run `cargo test --doc` separately |
| pytest-cov | cargo-llvm-cov | |
| fastapi | axum 0.8 + tower-http 0.7 | |
| pydantic (schemas) | serde + validator + utoipa | |
| pydantic (validation) | validator 0.21 + own `Valid<T>` extractor | 15 lines, see below |
| pydantic-settings | `config` crate + dotenvy | env vars into a `Settings` struct, `.env` loaded in dev |
| sqlalchemy 2.0 | sea-orm 2.0 | sqlx 0.9 + sea-query 1.0 underneath |
| alembic | sea-orm-migration + sea-orm-cli | |
| httpx | reqwest 0.13 | |
| testcontainers | testcontainers-modules 0.15 | |
| polyfactory | fake (`#[derive(Dummy)]`) + bon (builders) | |
| freezegun | `Clock` trait + `tokio::time::pause` | no monkeypatching in Rust |
| pydantic_ai | rig 0.42 + rmcp 2 | add when needed |
| nats-py | async-nats 0.50 | add when needed; `publish`/`subscribe`/`request`, `pull_subscribe`, `msg.ack` map 1:1 |
| redis-py | redis 1.7 (`ConnectionManager`) | add when needed; one multiplexed connection, not a pool |
| opentelemetry | tracing + tracing-opentelemetry + opentelemetry-otlp (traces); metrics + axum-prometheus (`/metrics`) | |
| prek | prek | |
| Makefile | Makefile | calls cargo directly, no task runner underneath |

## Decisions

### Tooling

- **rustup + `rust-toolchain.toml`** pins the compiler per repo. No mise, no asdf.
- **cargo-binstall** installs the cargo tools below as prebuilt binaries (seconds instead of minutes).
- **cargo-nextest** runs tests: one process per test, parallel, retries, JUnit output.
- **cargo-llvm-cov** for coverage, `cargo llvm-cov nextest`.
- **cargo-deny** for licenses, advisories, bans and duplicate versions. Covers what cargo-audit does, so cargo-audit is not installed.
- **cargo-machete** finds unused dependencies.
- **cargo-chef** for Docker layer caching.
- **bacon** for the background check/test loop (cargo-watch is unmaintained).
- **cargo-insta** to review snapshots.
- **prek** runs the pre-commit hooks.
- **Makefile** is the single entrypoint (`make lint`, `make test`). Cargo has no script section, so the Makefile is the task runner. just and mise are not used.
- Tool versions the skills were verified against: cargo-nextest 0.9, cargo-llvm-cov 0.8, cargo-deny 0.20, cargo-machete 0.9, bacon 3.25, prek 0.5, sea-orm-cli 2.0.3.
- **Complexity**: clippy lints only, configured in `[lints.clippy]`.
- **Linker**: default `lld` on x86_64 Linux since Rust 1.90. Add `mold` only after measuring.

### Web

- **axum 0.8** with `macros`. actix-web is ~10 % faster and has its own middleware model; not worth it.
- **tower-http 0.7** for `CorsLayer`, `CompressionLayer`, `TimeoutLayer`, `CatchPanicLayer` (features `cors`, `compression-full`, `timeout`, `catch-panic` only; the request id is one `from_fn` middleware, not tower-http's set/propagate pair, and there is no `TraceLayer`). `CatchPanicLayer::custom(handle_panic)` sits directly inside the request-id middleware (`error::catch_panic_layer()`): a handler panic is logged and answered with the constant 500 `ErrorBody`, request id included, instead of dropping the connection. `TimeoutLayer::new` is deprecated since 0.6.7; use `TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, dur)`, which `-D warnings` otherwise rejects.
- **utoipa 5 + utoipa-axum + utoipa-swagger-ui** (`vendored` feature so the build does not download Swagger UI). Serves `/docs` and `/openapi.json` like FastAPI.
- **validator 0.21** with a hand-written `Valid<T>` extractor. axum-valid is not used because it pins validator 0.20.
- **nutype** for validated newtypes (`Email`, `NonEmptyString`) only where the type is reused.
- **thiserror** for the `AppError` enum, **anyhow** inside services. `AppError` implements `IntoResponse` and is the only place a status is chosen; every failure, both router fallbacks included, renders the same `ErrorBody { error, request_id?, details? }`. The eleven variants and their statuses are in the wiring section below. 5xx and 409 bodies are constants; the cause chain goes to `tracing::error!`.
- `AppError` never grows a variant for an add-on (rig, NATS, Redis): each maps its own errors onto the canonical ones — unreachable or failed backend → `Unavailable(String)` (503; the string is logged, the body is constant), a provider or client rate limit → `TooManyRequests { retry_after_secs }` (429), a programming or wiring error → `Other(anyhow::Error)` (500), bad input → `BadRequest(String)` (400). No 502 except the existing `Http` variant for outbound HTTP.
- Readiness: `GET /health/ready` returns `{ "status": "ok" | "degraded" | "unavailable", "checks": { "<name>": ... } }`. A required dependency (the database) failing is 503 + `unavailable`; an optional one (cache, messaging) failing is 200 + `degraded`. Add-ons add one entry under `checks`.
- Rate limiting is Redis-backed (see the `rust-redis` skill): a `from_fn_with_state` middleware applied per router with `route_layer`, after request id, OTel and auth extraction and before `TimeoutLayer`; `main.rs` serves with `into_make_service_with_connect_info::<SocketAddr>()` so the IP fallback has a `ConnectInfo`.
- **reqwest 0.13** for outbound HTTP, features `["json", "query"]` — `query` and `form` became opt-in in 0.13, and rustls is already the default (no `rustls-tls` feature any more). Retries are first-party: `ClientBuilder::retry(reqwest::retry::for_host(h).max_retries_per_request(3))`. reqwest-retry is not needed.

### Config

- `config::Environment` (separator `__`) deserialised into a serde `Settings` struct with `#[serde(default)]` for defaults. No config files.
- `dotenvy::dotenv().ok()` is the first line of `main`. Missing `.env` is a no-op, so production reads real environment variables only. dotenvy 0.15.7 is from 2023 but the crate is finished, the repo is active (August 2026 commits) and it has 44 M downloads per 90 days; sea-orm-cli reads `.env` through it as well.
- **secrecy** `SecretString` for passwords, tokens and `DATABASE_URL`. Redacted `Debug`, explicit `expose_secret()`.
- Settings tests never touch the process environment: `Settings::from_map(map)` feeds `config::Environment::with_prefix("APP").source(Some(map))` an in-memory map with the same `APP__` keys. `std::env::set_var` is `unsafe` in edition 2024 and `unsafe_code = "forbid"` cannot be relaxed by `#[allow]`, so the alternative would be a `temp-env` dev-dependency.

### Database

- **sea-orm 2.0** on Postgres, features `sqlx-postgres`, `runtime-tokio-rustls`, `macros`, `with-chrono`, `with-uuid`, `with-rust_decimal`, `with-json`. `macros` is a default, but the dense entity format and `raw_sql!` depend on it, so list it.
- Use it as engine + query builder, not as an ActiveRecord: `Entity::find().filter(user::COLUMN.email.eq(..)).paginate()`. Filters take the typed `user::COLUMN.field`; `.column()` and `.cursor_by()` still take the `Column` enum; `OnConflict::column()` accepts either. sea-query for anything complex (subqueries, CTEs, dynamic filters), with `use sea_orm::ExprTrait;` in scope or the `Expr` methods are not found.
- **Relations**: lead with `Entity::load().with(..)` — one query per level, a join for 1-1 and a batched `IN (…)` for 1-N, nested paths in a single call. `LoaderTrait::load_many` / `load_one` when you want plain `Model`s, `find_with_related` for a single parent (never combine it with `paginate`), `has_related` for `WHERE EXISTS` parent filters, `eq_any` for `= ANY($1)`. `HasMany`/`BelongsTo` have an `Unloaded` state, so "did I forget to load this" is a value, not an exception.
- **Writes**: the house style is a struct literal, `ActiveModel { id: Set(Uuid::now_v7()), .. }.insert(db)`. Never `.save()` a new row whose primary key is set in application code — `save()` sees a `Set` PK, issues an `UPDATE`, matches zero rows and fails with `DbErr::RecordNotUpdated`. The nested `ActiveModel::builder()` is worth it only for inserting a whole object graph atomically.
- Pagination: `Entity::find().paginate(&db, page_size)` gives `fetch_page`, `num_pages`, `num_items`.
- Raw SQL: `raw_sql!` macro + `FromQueryResult`; a bare `Statement` goes through `execute_raw` / `query_all_raw`. `sqlx::query_as!` is possible on the same pool but needs `DATABASE_URL` at build time; avoid it.
- Service functions take `&C where C: ConnectionTrait`, so they work with a pool and a transaction alike. `DatabaseConnection` is cheap to clone — put it in axum state, never `Arc<_>`. Map unique violations with `DbErr::sql_err()` → `SqlErr::UniqueConstraintViolation`. Set `ConnectOptions::statement_timeout`.
- **Migrations**: `sea-orm-cli migrate init` creates the `migration/` workspace crate. Migrations are Rust (`SeaQuery` DSL or raw SQL in `up`). Run on startup with `Migrator::up(&db, None)` or `make migrate`. Add an index for every FK column.
- **Entities**: generated from the migrated database, `make entity` (`sea-orm-cli generate entity -o src/entities --with-serde both --entity-format dense`). Without `--entity-format dense` you get 1.x compact entities and none of the 2.0 relation story. **Migration first, always**: sea-orm-cli 2.0.3 has no autogenerate — no `--from-entity`, no `diff`, codegen only points database → entities. The 2.0 Schema Registry `sync` adds missing tables and columns, never alters or drops, leaves no reviewable artifact and is semver-exempt: prototyping and test harnesses only.
- **Types**: chrono 0.4 (`DateTime<Utc>`), uuid v7 primary keys, rust_decimal for money.
- **Cache / Redis**: not baseline; see Cache below.

sea-orm 1.x → 2.0 — the renames that break nearly every pre-2026 snippet:

| 1.x | 2.0 |
|---|---|
| `Column::Field.eq(..)` in a filter | `Entity::COLUMN.field.eq(..)` |
| `Expr` methods available on import | `use sea_orm::ExprTrait;` |
| `Expr::col(Alias::new("x"))` | `Expr::col("x")` |
| `cond.into_condition()` | `cond.into()` preferred; `into_condition()` still compiles |
| `DatabaseConnection` enum | struct (old variants in `conn.inner`) |
| hand-written `Relation` enum + `impl Related` | dense format: relations are fields on the Model |
| `db.execute(Statement)` | `db.execute_raw(Statement)` |
| `insert_many(..).on_empty_do_nothing()` | gone; empty iterators are handled |
| Postgres `serial` primary keys | `GENERATED BY DEFAULT AS IDENTITY` |

### Telemetry

- **tracing** everywhere (`#[instrument]`, `info!`), **tracing-subscriber** JSON output with `EnvFilter` (`RUST_LOG`).
- **Traces**: tracing-opentelemetry 0.33 + opentelemetry-otlp 0.32 over gRPC (tonic). The endpoint is the setting `APP__TELEMETRY__OTLP_ENDPOINT`, passed with `.with_endpoint(..)`; unset means no exporter is built at all (the tracer provider still exists, so trace ids are minted and propagated, then the spans are dropped), and the SDK's own `OTEL_EXPORTER_OTLP_ENDPOINT` is therefore never consulted. `OTEL_SERVICE_NAME` and `OTEL_RESOURCE_ATTRIBUTES` are honoured through `Resource::builder()`. `opentelemetry_sdk` needs `trace`, not `rt-tokio` — since 0.32 the batch processor runs on its own thread. `opentelemetry-otlp` with `default-features = false` must list both `grpc-tonic` and `trace`; `grpc-tonic` does not imply it, and the defaults drag in a blocking reqwest.
- **Propagation**: `global::set_text_map_propagator(TraceContextPropagator::new())` belongs in `init_telemetry`. The default global propagator is a no-op, so without that line nothing propagates in either direction and nothing warns. Inbound: **axum-tracing-opentelemetry 0.39** (`OtelAxumLayer` + `OtelInResponseLayer`), which reads `MatchedPath` for `http.route`. Outbound: **reqwest-middleware 0.5** + **reqwest-tracing 0.7** (`opentelemetry_0_32`).
- `use opentelemetry::trace::TracerProvider as _;` or `provider.tracer(..)` is not found; `OpenTelemetrySpanExt::set_parent` returns `Result` in 0.33, so a bare call trips `unused_must_use`.
- **Metrics**: `metrics` 0.24 facade + axum-prometheus 0.10, which provides the request counter, duration histogram and the `PrometheusHandle` for a `/metrics` route. Custom metrics via `metrics::counter!` / `histogram!`.
- Version set that compiles together: opentelemetry 0.32, opentelemetry_sdk 0.32, opentelemetry-otlp 0.32, tracing-opentelemetry 0.33, axum-tracing-opentelemetry 0.39.1, reqwest-middleware 0.5.2, reqwest-tracing 0.7.1, tonic 0.14, axum-prometheus 0.10.1, metrics 0.24.6, metrics-exporter-prometheus 0.18.3.

### Testing

- **rstest** for fixtures and parametrised cases. **test-log** for tracing output in tests.
- **API tests** with axum-test `TestServer::new(app)`; no HTTP port.
- **Database tests**: nextest runs every test in its own process, so a `static`/`OnceCell` container is per *test*, not per binary — measured, two tests started two containers. Read `TEST_DATABASE_URL` when it is set (docker compose locally, `services:` in CI) and fall back to testcontainers-modules Postgres with `.with_tag("18-alpine")` — the module default is `11-alpine` — plus `.with_container_name(..).with_reuse(ReuseDirective::Always)`. That needs a second dev-dependency, `testcontainers = { version = "0.27", features = ["reusable-containers"] }`, because modules 0.15 does not re-export the feature; reused containers are never cleaned up. Each test then does `CREATE DATABASE test_<uuid>` + `Migrator::up` (~0.17 s against a running server). If migrations get slow, clone a template database (`CREATE DATABASE … TEMPLATE tmpl`) instead. sea-orm `MockDatabase` only for pure logic.
- `TEST_NATS_URL` / `TEST_REDIS_URL` follow the `TEST_DATABASE_URL` pattern: read when set, else a testcontainers-modules container with an explicit `.with_tag(..)`.
- **Test data**: fake `#[derive(Dummy)]` for random valid values, bon builders for "valid default, override one field".
- **Outbound HTTP**: httpmock.
- **Trait mocks**: mockall.
- **Snapshots**: insta with `json` and `redactions`.
- **Diff assertions**: similar-asserts.
- **Time**: inject a `Clock` trait (`fn now(&self) -> DateTime<Utc>`) and swap a fixed clock in tests. For tokio timers use `#[tokio::test(start_paused = true)]` and `tokio::time::advance`, which need `tokio = { features = ["test-util"] }` in `[dev-dependencies]` — `full` does not include it. Cargo unions features across dependency kinds, so the dev entry only turns `test-util` on for test builds of a crate that already depends on tokio normally.
- `clippy.toml`'s `allow-unwrap-in-tests` covers only the body of a `#[test]` / `#[tokio::test]` function. Measured: a helper in `tests/common/mod.rs` called from a test still warns. Put `#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test helpers")]` at the top of every `tests/*.rs` that has non-`#[test]` helpers.
- **Property tests**: proptest, add when needed.

### AI (add when needed)

- **rig 0.42** — the facade crate, not `rig-core`. `rig-core` has no agent layer at all: `Agent`, `Tool`, `Extractor` live in `rig-agent`, reachable only through `rig`. Default features are `rig-core/default`, `agent`, `derive`, `rustls`. Gives agents, tools, structured output, RAG, 20+ providers, OTel GenAI conventions.
- **rmcp 2**: rig-agent 0.42 pins `rmcp ^2`, and two rmcp majors in one graph produce two incompatible `Peer<RoleClient>` types. Enable rig's `rmcp` feature to use MCP tools inside an agent. rmcp 3 is only for a standalone MCP server with no rig in the graph.

### Auth (add when needed)

- **axum-extra 0.12** (`typed-header`) for `TypedHeader<Authorization<Bearer>>`. A naive `"0.10"` resolves to 0.10.3, a year stale; 0.12.6 declares `axum ^0.8.9`.
- **jsonwebtoken 11** to validate IdP tokens against JWKS.
- **argon2 0.6** for password hashing.
- **tower-sessions** if server sessions are required.

### Messaging (add when needed)

- **async-nats 0.50** (`nats-io/nats.rs`, the official client), default features: `jetstream`, `kv`, `object-store` and `service` (the `nats.micro` analog) are all defaults, so plain `async-nats = "0.50"` is the whole API. MSRV 1.88, pure tokio 1, rustls only (no TLS feature to enable, no openssl path). Pre-1.0 with a breaking minor every 4–8 weeks: pin the minor.
- `async_nats::Client` is a cheap `Clone` over one multiplexed connection: in `AppState` by value, one per process.
- `retry_on_initial_connect()` is **off by default**; without it the pod dies when NATS boots second. Also set `.name(..)`, `.request_timeout(Some(..))` (default 10 s) and `.event_callback(..)` to log reconnects.
- JetStream for anything that must not be lost, core NATS for fire-and-forget and request-reply. Durable pull consumers only. `Nats-Msg-Id` deduplication makes the publish idempotent, not the handler: pair it with a unique key in Postgres.
- **Tests**: `TEST_NATS_URL` when set, else the testcontainers-modules `nats` module with `Nats::default().with_cmd(&NatsServerCmd::default().with_jetstream())` and `.with_tag("2.12-alpine")` — the module default is `2.10.14` with JetStream off.
- Rejected: the legacy `nats` crate (blocking; crates.io marks it deprecated in favour of async-nats), `lapin`/RabbitMQ (a second broker to run and learn), `rdkafka` (C librdkafka in the build, heavier ops, the workload is not log-shaped).

### Cache (add when needed)

- **redis 1.7** (redis-rs), features `tokio-comp`, `tokio-rustls-comp` (`rediss://` to a managed cache), `connection-manager`, `script` (a default, listed so `default-features = false` cannot drop `redis::Script`). BSD-3-Clause, already in `deny.toml`. MSRV 1.88.
- One `aio::ConnectionManager` per process, in `AppState` by value: `Clone`, one multiplexed socket, reconnects itself with backoff. Commands take `&mut self`, so clone per call, never a `Mutex`. `get_connection_manager_with_config` connects eagerly; set the `ConnectionManagerConfig` response timeout to ~100 ms (default 500 ms) so a slow cache is not a slow API.
- A pool only for blocking commands: a multiplexed connection interleaves every caller, so one `BLPOP key 2` stalled an unrelated `GET` on a clone by 1.94 s (measured). A module that issues `BLPOP`/`BRPOP`/`BLMOVE`/`BZPOPMIN`/`XREAD BLOCK`/`WAIT` gets its own small `deadpool-redis` 0.23 pool.
- The `json` feature is the RedisJSON server module, not serde support; a `Json<T>` newtype over `serde_json` needs no feature. Keys `app:v1:entity:id`, every write has a TTL, `SCAN` never `KEYS`, an outage degrades to a miss and never a 500.
- Pub/sub and streams belong to NATS, not Redis.
- **Tests**: `TEST_REDIS_URL` when set, else the testcontainers-modules `redis` module with `.with_tag("8-alpine")` — the module default is `5.0`. Isolate with a per-test key prefix, not `FLUSHDB`.
- Rejected: fred (last release February 2025, 18 months silent while redis-rs shipped 1.0→1.7), rustis (0.x, six breaking minors in six weeks).

## Cargo.toml

```toml
[package]
name = "app"
version = "0.1.0"
edition = "2024"
rust-version = "1.98"
publish = false           # also required by cargo-deny, see deny.toml

[workspace]
members = ["migration"]   # created by `sea-orm-cli migrate init`

# Every member opts in with `[lints] workspace = true`; a member without that
# line inherits nothing.
[lints]
workspace = true

[dependencies]
# runtime + web
tokio = { version = "1.53", features = ["full"] }
tokio-stream = "0.1"
futures = "0.3"
async-trait = "0.1"
axum = { version = "0.8", features = ["macros"] }
tower = "0.5"
tower-http = { version = "0.7", features = ["cors", "compression-full", "timeout", "catch-panic"] }
reqwest = { version = "0.13", features = ["json", "query"] }
# serialization / validation / openapi
serde = { version = "1", features = ["derive"] }
serde_json = "1"
validator = { version = "0.21", features = ["derive"] }
utoipa = { version = "5", features = ["axum_extras", "chrono", "uuid", "decimal"] }
utoipa-axum = "0.2"
utoipa-swagger-ui = { version = "9", features = ["axum", "vendored"] }
# config / errors / secrets
dotenvy = "0.15"
config = { version = "0.15", default-features = false }
secrecy = { version = "0.10", features = ["serde"] }
thiserror = "2"
anyhow = "1"
# database + data types
sea-orm = { version = "2", features = ["sqlx-postgres", "runtime-tokio-rustls", "macros", "with-chrono", "with-uuid", "with-rust_decimal", "with-json"] }
migration = { path = "migration" }
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1", features = ["v7", "serde"] }
rust_decimal = { version = "1", features = ["serde"] }
# telemetry
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
tracing-opentelemetry = "0.33"
opentelemetry = "0.32"
opentelemetry_sdk = { version = "0.32", features = ["trace"] }
opentelemetry-otlp = { version = "0.32", default-features = false, features = ["grpc-tonic", "trace"] }
axum-tracing-opentelemetry = "0.39"
reqwest-middleware = { version = "0.5", features = ["json", "query"] }   # its own features, not reqwest's
reqwest-tracing = { version = "0.7", features = ["opentelemetry_0_32"] }
metrics = "0.24"
axum-prometheus = "0.10"
# utilities
bon = "3"
itertools = "0.15"
derive_more = { version = "2", features = ["full"] }
strum = { version = "0.28", features = ["derive"] }
# add when needed (all resolved and compiled in the same lock file):
# axum-extra = { version = "0.12", features = ["typed-header"] }
# rig = { version = "0.42", features = ["rmcp"] }   # the facade; rig-core has no agents. Add "memory" for stores, "test-utils" as a dev feature
# rmcp = { version = "2", features = ["client", "macros", "transport-streamable-http-client-reqwest"] }   # rig-agent pins ^2
# jsonwebtoken = "11"   |   argon2 = "0.6"   |   nutype = { version = "0.7", features = ["serde"] }   |   sha2 = "0.11"
# async-nats = "0.50"   # jetstream, kv, object-store, service are defaults
# redis = { version = "1.7", features = ["tokio-comp", "tokio-rustls-comp", "connection-manager", "script"] }
# deadpool-redis = "0.23"   # only for blocking commands (BLPOP & co.)

[dev-dependencies]
rstest = "0.27"
insta = { version = "1.48", features = ["json", "redactions"] }
similar-asserts = "2"
testcontainers-modules = { version = "0.15", features = ["postgres", "redis", "nats"] }
testcontainers = { version = "0.27", features = ["reusable-containers"] }   # modules does not re-export it
axum-test = "21"
httpmock = "0.8"
mockall = "0.15"
fake = { version = "5", features = ["derive", "chrono", "uuid", "rust_decimal"] }
rand = "0.10"                                            # the version fake 5 builds on
test-log = { version = "0.2", features = ["trace"] }
tokio = { version = "1.53", features = ["test-util"] }   # `full` does NOT include test-util

[package.metadata.cargo-machete]
# The stack ships these for the code you are about to write; `rust_decimal` is
# additionally reachable only through sea-orm's `with-rust_decimal` feature, which
# machete cannot see. Delete a name here when you start using the crate.
ignored = ["bon", "derive_more", "itertools", "rust_decimal", "strum", "tokio-stream", "futures", "async-trait"]

[workspace.lints.rust]
unsafe_code = "forbid"
unused_must_use = "deny"

[workspace.lints.clippy]
all = { level = "warn", priority = -1 }
pedantic = { level = "warn", priority = -1 }
unwrap_used = "warn"
expect_used = "warn"
panic = "warn"
todo = "warn"
unimplemented = "warn"
dbg_macro = "warn"
print_stdout = "warn"
print_stderr = "warn"
allow_attributes_without_reason = "warn"
cognitive_complexity = "warn"
redundant_clone = "warn"
module_name_repetitions = "allow"
must_use_candidate = "allow"
missing_errors_doc = "allow"
missing_panics_doc = "allow"
```

Every lint name was checked against `clippy-driver -Whelp` on clippy 0.1.98. The tables are `[workspace.lints.*]` and both packages opt in with `[lints] workspace = true`; a member without that line inherits nothing. `too_many_lines` and `too_many_arguments` already come from `pedantic` and `all`; `mod_module_files` is deliberately absent because sea-orm-cli generates `entities/mod.rs`.

The `migration/` crate depends on `sea-orm-migration = { version = "2", features = ["sqlx-postgres", "runtime-tokio-rustls"] }` plus `tokio = { version = "1.53", features = ["macros", "rt", "rt-multi-thread"] }` for the `migration/src/main.rs` binary that `sea-orm-cli migrate up` runs, and carries `[lints] workspace = true`.

Levels stay in `Cargo.toml`; thresholds go in `clippy.toml` at the repo root: `allow-unwrap-in-tests`, `allow-expect-in-tests`, `allow-panic-in-tests`, `allow-dbg-in-tests` (all `true`, all scoped to the body of a `#[test]` function), `cognitive-complexity-threshold = 15` (default 25 is very permissive), `too-many-arguments-threshold = 5` (default 7; past 5, pass a struct).

## rust-toolchain.toml

```toml
[toolchain]
channel = "1.98.1"
components = ["rustfmt", "clippy", "llvm-tools-preview"]
```

## Makefile

```make
.PHONY: install-tools fmt lint check test cov dev run migrate entity

install-tools:
	cargo install cargo-binstall
	cargo binstall -y cargo-nextest cargo-llvm-cov cargo-deny cargo-machete cargo-chef bacon cargo-insta sea-orm-cli
	uv tool install prek
	prek install

fmt:
	cargo fmt --all

lint:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets --all-features -- -D warnings

check: lint
	cargo deny check
	cargo machete

test:
	cargo nextest run --workspace --all-features
	cargo test --doc --workspace

cov:
	cargo llvm-cov nextest --workspace --all-features --no-report
	cargo llvm-cov report --lcov --output-path target/lcov.info --ignore-filename-regex '(entities|migration)/|main\.rs|telemetry\.rs'
	cargo llvm-cov report --summary-only --fail-under-lines 80 --ignore-filename-regex '(entities|migration)/|main\.rs|telemetry\.rs'

dev:
	bacon

run:
	cargo run

migrate:
	sea-orm-cli migrate up

entity:
	sea-orm-cli generate entity -o src/entities --with-serde both --entity-format dense
```

## .pre-commit-config.yaml (prek)

```yaml
repos:
  - repo: https://github.com/pre-commit/pre-commit-hooks
    rev: v6.0.0
    hooks:
      - id: trailing-whitespace
        args: [--markdown-linebreak-ext=md]
      - id: end-of-file-fixer
      - id: check-yaml
      - id: check-toml
      - id: check-json
      - id: check-merge-conflict
      - id: check-added-large-files
        args: [--maxkb=512]

  - repo: local
    hooks:
      - id: fmt
        name: cargo fmt
        entry: cargo fmt --all -- --check
        language: system
        types: [rust]
        pass_filenames: false
      - id: clippy
        name: cargo clippy
        entry: cargo clippy --all-targets --all-features -- -D warnings
        language: system
        types: [rust]
        pass_filenames: false
      - id: deny
        name: cargo deny
        entry: cargo deny check
        language: system
        files: Cargo\.(toml|lock)$
        pass_filenames: false
      - id: machete
        name: cargo machete
        entry: cargo machete
        language: system
        files: Cargo\.toml$
        pass_filenames: false
```

## deny.toml

`cargo deny init` writes `allow = []`, so the first `cargo deny check` always fails. This one is green on the whole stack (all four checks). Unknown keys are a hard parse error in cargo-deny 0.20, and `[[licenses.exceptions]]` has no `reason` field.

```toml
[graph]
all-features = true

[advisories]
db-urls = ["https://github.com/rustsec/advisory-db"]
unmaintained = "workspace"   # a scope, not a level: all | workspace | transitive | none
unsound = "all"
yanked = "deny"
# Explicit and empty: an advisory is only ever waived here, with its RUSTSEC id
# and a reason, so the waiver stays reviewable.
ignore = []

[licenses]
confidence-threshold = 0.93
# `cargo deny init` writes `allow = []`, so its first run always fails. This list
# is green on the whole stack.
allow = [
    "MIT", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "BSD-2-Clause", "BSD-3-Clause",
    "ISC", "Unicode-3.0", "Zlib", "CDLA-Permissive-2.0", "Unlicense", "BSL-1.0",
    "CC0-1.0",                   # CC0: axum-tracing-opentelemetry and its sdk
]

[licenses.private]
ignore = true                    # needs `publish = false` on both crates

[bans]
multiple-versions = "warn"       # ~25 duplicates on this stack; "deny" is unusable
wildcards = "deny"
allow-wildcard-paths = true      # for `migration = { path = "migration" }`

[sources]
unknown-registry = "deny"
unknown-git = "deny"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
```

## Reference wiring

The scaffold in the `rust-scaffolding` skill (`assets/app/src/`) is the reference wiring and is what every snippet compiles against: `config.rs` (nested `Settings` from `APP__` variables plus the four timeout constants), `error.rs` (`AppError`, `ErrorBody`, the request-id task local), `extract.rs` (`Valid<T>`, `ValidQuery<T>`), `telemetry.rs` (subscriber, optional OTLP exporter, `TelemetryGuard`), `lib.rs` (`AppState`, `build_router` with the layer stack), `api/health.rs` (the readiness body above), `main.rs` (settings → telemetry → pool → migrations → serve with `into_make_service_with_connect_info` → drain → flush spans → close pool).

The error type, verbatim from `error.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// Body parsed but failed a `validator` rule.
    #[error("validation failed")]
    Validation(#[from] validator::ValidationErrors),
    /// Body was malformed, the wrong content type, or too large.
    #[error(transparent)]
    JsonRejection(#[from] JsonRejection),
    /// Anything else the client got wrong: an unparseable query string, a path
    /// segment that is not a uuid, a combination of parameters that cannot hold.
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("unauthorized")]
    Unauthorized,
    /// The caller is over a rate limit; `Some` becomes a `Retry-After` header.
    #[error("too many requests")]
    TooManyRequests { retry_after_secs: Option<u64> },
    /// A dependency this request cannot do without (a cache, a lock, a queue) is
    /// down. The string names it in the log; the client sees a constant.
    #[error("{0}")]
    Unavailable(String),
    #[error(transparent)]
    Db(#[from] sea_orm::DbErr),
    #[error(transparent)]
    Http(#[from] reqwest_middleware::Error),
    /// Anything unexpected. Never rendered to the client.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
```

| Variant | Status |
|---|---|
| `Validation`, `JsonRejection::JsonDataError` | 422 |
| other `JsonRejection`, `BadRequest` | 400 |
| `NotFound` | 404 |
| `Conflict`, `Db` when the driver reports 23505 | 409 (constant body) |
| `Unauthorized` | 401 |
| `TooManyRequests` | 429, `Retry-After` when `Some` |
| `Unavailable` | 503 (constant body) |
| `Http` | 502 (constant body) |
| `Db` otherwise, `Other` | 500 (constant body) |

## Compatibility notes

- The lock file contains tower-http 0.6.11 (pulled by reqwest) next to 0.7.1. Both are on http 1 / tower 0.5; harmless.
- `sea-query-binder` is stuck on sqlx 0.8. Its successor is `sea-query-sqlx` 0.9, which sea-orm 2.0 already brings in. Do not add either directly.
- `testcontainers-modules` 0.15 requires testcontainers 0.27 while the standalone crate is at 0.28. Use the modules re-export for everything except container reuse: `ReuseDirective` / `with_reuse` sit behind testcontainers' `reusable-containers` feature, which modules does not re-export, so pin `testcontainers = "0.27"` explicitly next to it.
- `reqwest-middleware` gates `json`, `query` and `form` behind its **own** features; enabling them on reqwest is not enough. `axum-tracing-opentelemetry` and its sdk are CC0-1.0, which no default cargo-deny allow list has.
- `axum-valid` 0.25 pins validator 0.20; with validator 0.21 you get two `Validate` traits. Hence the own extractor.
- cargo-nextest does not run doctests.

## Rejected

| Crate / tool | Reason | Instead |
|---|---|---|
| sqlx alone | no query builder, relations by hand | sea-orm (sqlx underneath) |
| diesel | typed DSL, async is second-class | sea-orm |
| Toasty | Tokio team, but 23k downloads / 90 days | sea-orm; revisit 2027 |
| actix-web | own middleware model for ~10 % throughput | axum |
| Rocket, Poem | no release in 14+ months | axum |
| garde + axum-valid | fine, 7x smaller community | validator + `Valid<T>` |
| jiff | better API, but sqlx bridge is a small separate crate | chrono |
| figment, envy | no release in 2+ years / dead | config |
| mise, just | extra tools; Makefile is enough | Makefile + rust-toolchain.toml |
| cargo-audit | covered by cargo-deny | cargo-deny |
| cargo-shear | fine, 10x less used | cargo-machete |
| cargo-watch | unmaintained | bacon |
| clap | no CLI needed yet | config for env |
| mock_instant | `Clock` trait covers it | own trait |
| pretty_assertions | 2 years idle | similar-asserts |
| wiremock | 13 months idle | httpmock |
| color-eyre | 16 months idle | anyhow |
| fred | last release February 2025, 18 months silent while redis-rs shipped 1.0→1.7 | redis |
| rustis | 0.x, six breaking minors in six weeks | redis |
| `nats` (legacy) | blocking client, deprecated on crates.io in favour of async-nats | async-nats |
| lapin / RabbitMQ | a second broker to run and learn | async-nats |
| rdkafka | C librdkafka in the build, heavier ops, workload is not log-shaped | async-nats |
| axum-login | 13+ months idle | tower-sessions |
| tower_governor | 13+ months idle, and an in-process counter is per replica | rate limiting: Redis-backed, see `rust-redis` |
| aide | smaller than utoipa, axum-only | utoipa |
| swiftide, genai, rust-mcp-sdk | 10–100x smaller than rig / rmcp | rig, rmcp |
| rig-core alone | no agent layer at all; `Agent`/`Tool` live in rig-agent | the `rig` facade |
| reqwest-retry | reqwest 0.13 has `ClientBuilder::retry` with a token budget | built in |
| temp-env | only needed to mutate the process env in settings tests | `Environment::source(map)` |
| async-std | discontinued | tokio |
