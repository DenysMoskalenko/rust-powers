# Layout and request flow

- [The tree](#the-tree)
- [A request through the tree](#a-request-through-the-tree)
- [Where a change belongs](#where-a-change-belongs)

## The tree

```
Cargo.toml               workspace root: the app package, the migration member, [workspace.lints]
Cargo.lock               committed; CI and Docker build with --locked
rust-toolchain.toml      compiler pin
clippy.toml              lint thresholds (levels live in Cargo.toml)
rustfmt.toml             formatting
deny.toml                licences, advisories, bans, sources
bacon.toml               the background check loop
.config/nextest.toml     test profiles (default, ci)
.pre-commit-config.yaml  prek hooks
Makefile                 task runner
Dockerfile               cargo-chef build, non-root runtime
docker-compose.yml       local Postgres, optional OTLP collector
.dockerignore            keeps target/ out of the build context
.env.example             every setting, with a safe default
.gitignore               /target, .env, *.pending-snap
.github/workflows/ci.yml lint, test and coverage, cargo-deny, docs
src/
  main.rs                process wiring only: settings, telemetry, pool, serve
  lib.rs                 AppState, build_router, the middleware stack
  config.rs              Settings, its nested structs, the timeout constants
  error.rs               AppError, ErrorBody, the request-id task local
  extract.rs             Valid and ValidQuery
  clock.rs               the Clock trait, SystemClock, FixedClock
  telemetry.rs           subscriber, OTLP exporter, TelemetryGuard
  api/mod.rs             assembles every resource router; the request_id middleware
  api/health.rs          liveness, readiness (status + checks), version
  api/users.rs           one resource: schemas, handlers, router
  entities/              generated from the database; never hand-edited
migration/
  Cargo.toml             [lints] workspace = true, like the root
  src/lib.rs             the Migrator and its list of migrations
  src/main.rs            the binary sea-orm-cli migrate runs
  src/m2026...rs         one file per schema change
tests/
  common/mod.rs          test_app(): throwaway database, frozen clock, mock server
  health.rs              the probes and both router fallbacks
  panic.rs               the catch-panic layer, wired in its own binary
  users.rs               the four cases that prove the wiring
```

Everything testable lives in `lib.rs` and below. `main.rs` holds only what a test cannot
exercise, because an integration test in `tests/` can use the library but never the binary.

## A request through the tree

1. `main.rs` binds the listener and serves `build_router(state)` with
   `into_make_service_with_connect_info`, so every request carries the peer address.
2. `lib.rs::build_router` runs the middleware stack, `request_id` (in `api/mod.rs`) first
   and the timeout last, then matches the route. Both fallbacks answer in `ErrorBody`.
3. `api/<resource>.rs` extracts `State<AppState>`, `Path<T>` and, last, `Valid<T>` from
   `extract.rs`; the handler queries `state.db`, reads time from `state.clock`, calls
   upstreams through `state.http`.
4. The handler returns a DTO or an `AppError`; `error.rs` is the only place a status code
   is chosen, and the body is always `ErrorBody`.

The layer order and its reasons, the status table, the timeout budget in `config.rs` and
the shutdown order are `axum-service`'s; this file only says which file holds each.

## Where a change belongs

| Change | File |
|---|---|
| A new setting | `src/config.rs`, then `.env.example` |
| A new error case | map it onto an existing `AppError` variant; `src/error.rs` only for a new status |
| A new resource | a new `src/api/<name>.rs` plus one line in `src/api/mod.rs` |
| A readiness check for a new dependency | one entry in `checks` in `src/api/health.rs` |
| A schema change | a new file in `migration/src/`, then `make migrate` and `make entity` |
| A new middleware | `build_router` in `src/lib.rs` |
| A new timeout | `src/config.rs`, next to the other four |
| A new test helper | `tests/common/mod.rs` |
| A new configuration file or CI job | see `rust-tooling` |
