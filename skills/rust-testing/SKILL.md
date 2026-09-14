---
name: rust-testing
description: "Use when writing or fixing tests for an axum service, or stabilizing a flaky test — API tests through axum-test, the test_app helper and its dependency overrides, rstest fixtures and cases, fake and bon factories, httpmock, insta snapshots, an injected Clock, nextest filters, coverage exclusions. Also for PoolTimedOut, or a test that leaks state under nextest. Not for nextest.toml, llvm-cov flags or CI (rust-tooling), the Postgres container and per-test database (sea-orm-postgres), or rig mocks (building-rig-agents)."
metadata:
  version: "0.1.0"
---

# Testing an axum service

Assumes Rust 1.98, edition 2024, axum 0.8, axum-test 21, rstest 0.27, sea-orm 2, tokio 1.53, fake 5, bon 3, httpmock 0.8, mockall 0.15, insta 1.48, cargo-nextest 0.9.

## Important

- Test through HTTP against a real Postgres via `test_app()`; never `sea_orm::MockDatabase` or a mock of your own service layer.
- Mock only what cannot be controlled: outbound HTTP (`httpmock`), the clock (`FixedClock`), a payment or LLM provider.
- No `tokio::time::sleep` to wait for spawned work: hold the `JoinHandle` (or a oneshot) and await it. `start_paused` is for pure timer logic with no real I/O.
- Settings come from a map, never `std::env::set_var`; assert the status before the body.
- Helpers in `tests/common/mod.rs`, never `tests/common.rs`.

## Philosophy

Test through HTTP against a real Postgres. One request exercises routing, extractors, validation, serialisation, error mapping and the query at once; a handler unit test proves little.

- **Unit-test pure logic only** — parsers, money arithmetic, state machines — in a `#[cfg(test)] mod tests` in the same file, where private items are visible.
- **Never mock the database.** `sea_orm::MockDatabase` asserts on the SQL emitted today, so it fails on every refactor and passes on every schema mistake.
- **Mock only what cannot be controlled** — outbound HTTP, the clock, an email or payment provider.
- **No sleeps to wait for work.** A `tokio::time::sleep` before an assertion is a flake waiting for a slow CI box; a bounded retry loop in a harness (the container ping) is not a wait.
- **Deterministic data.** Random-but-valid for "any valid user"; the moment an assertion depends on a value, pin it.
- **Explicit over DRY.** Repeat setup when it clarifies intent; extract an assertion helper at the fifth repetition, not the second.
- **Name the outcome**: `creates_user_returns_201`, `duplicate_email_returns_409`. Arrange, act, assert, one blank line between them, status before body.

## Layout

```text
src/lib.rs            everything public, build_router included
src/main.rs           process wiring only
tests/common/mod.rs   test_app() and factories
tests/users.rs        one file per bounded area
```

Integration tests reach only `pub` items of a **library** target, so a binary-only crate cannot be integration-tested. Shared helpers go in `tests/common/mod.rs`, never `tests/common.rs`: every `.rs` file directly in `tests/` becomes its own test binary, so `tests/common.rs` shows up as a spurious empty target. Each test file is one more link step, and link time dominates test builds: three to five files per service. nextest forks per test, not per binary, so parallelism is unaffected.

`allow-unwrap-in-tests` in `clippy.toml` covers only the body of a `#[test]` function. Any `tests/*.rs` holding helpers, `tests/common/mod.rs` included, opens with the scaffold's attribute: `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, reason = "test helpers")]`.

## test_app(): overriding dependencies

There is no override registry in axum and none is needed: the router is a function of its state, so a test builds the **real** router over a state it owns.

```rust
pub struct TestApp {
    pub server: axum_test::TestServer,  // drives the real router, no socket
    pub state: AppState,                // db, clock and http still reachable
    pub db_name: String,                // this test's own database
    pub mock: httpmock::MockServer,     // a running stub; give out base_url()
}

pub async fn test_app() -> TestApp { /* in tests/common/mod.rs */ }
```

`test_app()` creates a fresh database and runs the migrations, pins the clock to `FixedClock(2026-09-13T12:00:00Z)` so `created_at` is assertable, and starts a `MockServer`: give its `base_url()` to whatever setting the code under test reads for an upstream. Two dependencies swapped, one stub started, nothing global. For how the database is provisioned see `sea-orm-postgres`.

Do **not** add an `Arc<dyn Trait>` to `AppState` to make something swappable. A one-implementor trait is justified only by a side effect a test cannot exercise: time, email, payments, LLM calls. Postgres and outbound HTTP have real local stand-ins.

Tests never call `std::env::set_var`: it is `unsafe` in edition 2024 under `unsafe_code = "forbid"`. Build settings from an in-memory map, as `test_app()` does.

## The shape of a test

```rust,verify,test
mod common;

use axum::http::StatusCode;
use serde_json::json;

#[tokio::test]
async fn creates_user_returns_201() {
    let app = common::test_app().await;

    let response = app
        .server
        .post("/users")
        .json(&json!({ "email": "ada@example.com", "name": "Ada Lovelace" }))
        .await;

    response.assert_status(StatusCode::CREATED);
    let body = response.json::<serde_json::Value>();
    assert_eq!(body["email"], "ada@example.com");
    // Proof the handler read the injected clock, not the wall clock.
    assert_eq!(body["created_at"], json!(common::frozen_now()));
}

#[tokio::test]
async fn duplicate_email_returns_409() {
    let app = common::test_app().await;
    let payload = json!({ "email": "ada@example.com", "name": "Ada" });
    app.server.post("/users").json(&payload).await.assert_status(StatusCode::CREATED);

    let response = app.server.post("/users").json(&payload).expect_failure().await;

    response.assert_status(StatusCode::CONFLICT);
}
```

`TestServer::new` asserts nothing about the status — a 500 comes back like any other response, hence status before body. `.expect_failure()` fails the request on an unexpected 2xx, `.expect_success()` the reverse; `TestServer::builder().expect_success_by_default().build(app)` flips the default for a whole server. Also on the request: `.add_query_param`, `.add_header`, `.authorization_bearer`, `.form`. Also on the response: `.json::<T>()`, `.text()`, `.assert_json(&value)`, `.assert_text`, `.status_code()`, `.headers()`.

## rstest

Put `#[rstest]` first, above `#[tokio::test]`. Plain cases tolerate the wrong order; `#[awt]` does not: `#[tokio::test]` on top fails with `error[E0728]: await is only allowed inside async functions and blocks`.

| Attribute | Use |
|---|---|
| `#[fixture] fn server() -> TestServer` | injected by parameter name |
| `#[once]` on a fixture | sync only — rstest rejects `async`, so it cannot wrap `test_app()`; once per test *process*, which under nextest is once per test, so nothing is built once for a whole run: share a server through `TEST_DATABASE_URL` |
| `#[future]` on the parameter, `#[awt]` on the test | await an async fixture so the body sees `T` |
| `#[case::missing_at("not-an-email")]` | one named case per tuple of inputs |
| `#[values(a, b)]` on a parameter | cartesian product; prefer named `#[case]`s |
| `#[with(arg)]` | override a fixture's arguments at the call site |
| `#[timeout(Duration::from_secs(5))]` | fail rather than hang |

An underscore-prefixed fixture parameter trips `clippy::used_underscore_binding` under pedantic; name it normally.

| Need | Here |
|---|---|
| assert a call failed | API: `.expect_failure()` then `assert_status`; unit: `let err = f().unwrap_err(); assert!(matches!(err, AppError::NotFound(_)))` |
| a known failure, unfixed | `#[should_panic(expected = "..")]` for a known panic, `#[ignore = ".."]` otherwise |
| an implicit fixture | none; every fixture is a named parameter |

## Running tests

| Goal | Command |
|---|---|
| Everything | `cargo nextest run` |
| One test by name | `cargo nextest run -E 'test(creates_user)'` |
| One binary, or a regex | `cargo nextest run -E 'binary(users)'`, `-E 'test(/^db_/)'` |
| Combine | `-E 'binary(users) and not test(slow)'` |
| Include `#[ignore]`d tests | `cargo nextest run --run-ignored all` |
| See output live, serially | `cargo nextest run --no-capture` |
| Doctests | `cargo test --doc`; nextest skips them |

Mark a test needing a live external dependency `#[ignore = "requires a live IdP"]` so the default run stays green offline. nextest forks a process per test, so `static`, `OnceLock` and `LazyLock` re-initialise per test and an in-process `Mutex` serialises nothing; a test group is for a genuinely shared, uncloneable resource only. For `.config/nextest.toml` see `rust-tooling`.

## Coverage

```bash
cargo llvm-cov nextest --workspace --all-features --no-report
cargo llvm-cov report --summary-only --fail-under-lines 80 \
  --ignore-filename-regex '(entities|migration)/|main\.rs|telemetry\.rs'
```

The denominator leaves out four things: `entities/` is generated by `sea-orm-cli`; `migration/` is DDL every test already runs, so it only inflates the number; `main.rs` is process wiring no test calls; `telemetry.rs` is exporter setup that only executes against a live collector. The Makefile `cov` recipe and the CI job carry this regex character for character; change it in one place, see `rust-tooling`. 80% is a regression alarm, not a target. To scope `report` to one crate pass `-p`; it does not accept `--workspace`.

## Stabilising a flaky test

| Symptom | Cause | Fix |
|---|---|---|
| Passes alone, fails in the suite | shared database or fixture data | one database per test through `test_app()` |
| Fails only on CI | real time, real sleeps | `FixedClock`; `#[tokio::test(start_paused = true)]` for timer-only logic |
| Spawned task + `sleep` before the assertion | the sleep is a bet on the CI box | keep the `JoinHandle` and `.await` it; `start_paused` breaks `test_app()` (`RequestTimeoutError` / `PoolTimedOut`) |
| Fails when reordered | assertion depends on insertion order | order the query or assert on a set; under `FixedClock` every row shares `created_at`, so a `created_at DESC` list is ordered by the uuid v7 `id` tiebreak alone |
| Address already in use | a real socket was bound | `TestServer::new(app)` is in-process; drop `http_transport()` |
| Assertion on a random value | unpinned factory data | override the field or seed the RNG |
| `OnceCell` re-initialises | nextest forks per test | move the resource out of the process |
| Every test hangs at startup after a Docker restart | the reused `rust-powers-test-postgres` container is stale and never health-checked again | `docker rm -f rust-powers-test-postgres`, then rerun |

## Older examples that no longer pass `-D warnings`

| Old | Now |
|---|---|
| `TestServer::new(app).unwrap()` | `TestServer::new(app)` returns `Self`; `try_new` returns `Result` |
| `mock.hits()`, `mock.assert_hits(n)` | deprecated, still compile: `mock.calls()`, `assert_calls(n)` |
| `when.path_contains(..)`, `when.json_body_partial(..)` | removed: `path_includes(..)`, `json_body_includes(..)` |
| `tokio = { features = ["full"] }` for paused time | `full` excludes `test-util`; add it in dev-dependencies |

## Red Flags — STOP

| About to… | Do this instead |
|---|---|
| Mock the database or your own service layer | real Postgres through `test_app()`; mock only what cannot be controlled |
| Add `Arc<dyn Repository>` so a test can swap the store | swap the whole `AppState`; one-implementor traits are for genuine side effects only |
| `tokio::time::sleep` to wait for background work | await the `JoinHandle` or a oneshot; `start_paused` only with no real I/O — it auto-advances past every timeout on the way to Postgres, so `test_app()` fails at once (`RequestTimeoutError`, `PoolTimedOut`) |
| Assert on `Utc::now()` output | inject the `Clock`; nothing can monkey-patch `Utc::now()` |
| `std::env::set_var` to configure a test | build settings from a map; `set_var` is `unsafe` under a forbid lint |

## References

- `references/factories.md` — fake `Dummy` derives, bon factories, `ActiveModel` inserts, seeded RNG, parametrized cases. Read before the second test that needs a valid entity.
- `references/mocking.md` — httpmock, mockall versus a hand-written fake, the `Clock` trait, awaiting background work, paused tokio time, test-log. Read when a test touches the network, the clock or a trait.
- `references/snapshots.md` — insta redactions, `cargo insta review`, similar-asserts. Read when asserting on a whole response body: a snapshot is right when the shape matters more than any one field, wrong when one field is the point.
