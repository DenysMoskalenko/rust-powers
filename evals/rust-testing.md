# rust-testing

### Triggering

**Should load**

1. "our pytest suite used freezegun for this. how do I freeze time in the axum service tests so I can assert on `created_at` in the POST /users response?"
2. "I want the polyfactory equivalent for building test users in this repo — fake and bon are already in dev-dependencies, what does the factory look like?"
3. "getting `error[E0599]: no method named `unwrap` found for struct `TestServer`` on every api test after bumping axum-test, what changed"
4. "tests/users.rs passes when I run it alone and fails under `cargo nextest run`. something is leaking between tests and I can't see what"
5. "we're porting a FastAPI service and I don't know what replaces `app.dependency_overrides` — I need the handler under test to hit a scratch database and a fake upstream instead of the real one"

**Should not load**

1. "tune `.config/nextest.toml` (slow-timeout, retries) and wire the llvm-cov job into the CI workflow" -> `rust-tooling`
2. "start the Postgres testcontainer and give every test its own database with the migrations applied" -> `sea-orm-postgres`
3. "my rig agent hits OpenAI when the suite runs — how do I stub the completion model out?" -> `building-rig-agents`
4. "POST /users returns 500 instead of 409 on a duplicate email, fix the AppError to status mapping" -> `axum-service`
5. "clippy is complaining `needless_pass_by_value` on my service function, what's the idiomatic signature" -> `rust-code-style`

### Eval 1 - first API test for a new endpoint

**Prompt**: "Add integration tests for POST /users: success, duplicate email, and an invalid email address."

**Must produce**:
- a file under `tests/`, with `mod common;` and `common::test_app().await`
- `app.server.post("/users").json(&..)`, the status asserted before the body
- `.expect_failure()` on the 409 and 422 cases (opt-in: `TestServer` asserts nothing about the status by itself), and `assert_status` still stated
- test names stating the outcome, such as `creates_user_returns_201` / `duplicate_email_returns_409`

**Must not produce**:
- `sea_orm::MockDatabase`, or any mock of the service or repository layer
- `TestServer::new(app).unwrap()`
- `tests/common.rs`
- a hand-rolled `reqwest` client against a bound port

### Eval 2 - stabilising a flaky test

**Prompt**: "This test passes locally and fails about one run in five on CI. Make it deterministic." (test seeds two rows, sleeps 200 ms waiting for a background task, then asserts on the first row returned by a list endpoint and on `created_at`)

**Must produce**:
- removal of `tokio::time::sleep`: the background task's `JoinHandle` (or a oneshot it signals) is awaited, or the work is done inline before the request; `#[tokio::test(start_paused = true)]` is explicitly rejected because the test uses `test_app()` (real Postgres — paused time fires every connect timeout instantly: `RequestTimeoutError` / `PoolTimedOut`)
- the `created_at` assertion pinned to the fixed clock rather than to `Utc::now()`
- an explicit ordering on the query, or an order-independent assertion
- a per-test database via `test_app()` rather than shared state
- if the suite hangs at startup after a Docker restart instead of flaking: `docker rm -f rust-powers-test-postgres` so the reused container is recreated

**Must not produce**:
- a longer sleep, a retry loop, `--test-threads 1`, or `start_paused = true` on a test that opens a database connection
- `serial_test` or a global `Mutex` (nextest forks a process per test, so neither serialises anything)

### Eval 3 - test data factory

**Prompt**: "We keep copy-pasting the same 12-line user literal into tests. Give us a factory."

**Must produce**:
- a `bon::Builder` struct with `#[builder(default = ..)]` defaults drawn from `fake` fakers
- a conversion into `ActiveModel` (or the DTO) and insertion through the database, not through the endpoint under test
- placement in `tests/common/mod.rs`
- a note that any field an assertion depends on is overridden explicitly or seeded

**Must not produce**:
- a `#[derive(Factory)]` attribute, which does not exist in this stack
- a `static`/`LazyLock` shared instance reused across tests
- `rand = "0.9"` alongside fake 5

### Eval 4 - stubbing an outbound call

**Prompt**: "The `/users/{id}/profile` endpoint calls our billing API. Write a test for the happy path and for a 503 from billing."

Reasoning check only: the scaffold has no such endpoint or upstream setting, so this eval is judged on the shape of the answer, not run under the snippet harness.

**Must produce**:
- `app.mock` from `test_app()` used as the upstream, with `when`/`then` matchers, and the billing base URL setting pointed at `app.mock.base_url()` inside `test_app()`
- the request sent through `app.server` so the handler makes the outbound call; the test never drives `app.state.http` itself
- `assert_async()` or a call-count assertion proving the upstream was actually hit
- the 503 case asserting the endpoint's own mapped status, with `.expect_failure()`

**Must not produce**:
- a real network request, or a hard-coded external host
- `mockall` on a one-implementor HTTP client trait added just for the test
- `mock.hits()`, `path_contains(..)` or `json_body_partial(..)`
