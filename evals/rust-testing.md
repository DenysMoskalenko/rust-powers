# rust-testing

### Triggering

**Should load**

1. "how do I freeze time in the axum service tests so I can assert on `created_at` in the POST /users response?"
2. "I need lots of valid users in tests, random except for the one field each test checks. what's the pattern in this repo?"
3. "getting `error[E0599]: no method named `unwrap` found for struct `TestServer`` on every api test after bumping axum-test, what changed"
4. "tests/users.rs passes when I run it alone and fails under `cargo nextest run`. looks like one test sees rows another one created"
5. "in tests, how do I point the app at a throwaway database and a stub for the billing API instead of the real ones?"
6. "write the first tests for our new `GET /orders` endpoint"

**Should not load**

1. "tune `.config/nextest.toml` (slow-timeout, retries) and wire the llvm-cov job into the CI workflow" -> `rust-tooling`
2. "start the Postgres testcontainer and give every test its own database with the migrations applied" -> `sea-orm-postgres`
3. "my rig agent hits OpenAI when the suite runs — how do I stub the completion model out?" -> `building-rig-agents`
4. "POST /users returns 500 instead of 409 on a duplicate email, fix the AppError to status mapping" -> `axum-service`
5. "a test helper holds a `std::sync::MutexGuard` across `.await` and clippy flags `await_holding_lock` — what's the idiomatic fix?" -> `rust-code-style`
6. "raise the coverage gate from 80% to 85% in the Makefile and CI" -> `rust-tooling`
7. "`PoolTimedOut` in production under load — how big should the sea-orm connection pool be?" -> `sea-orm-postgres`
8. "our JetStream consumer test against the NATS container never sees the published message" -> `rust-nats`
9. "cache tests against the Redis testcontainer see each other's keys" -> `rust-redis`

### Eval 1 - first API test for a new endpoint

**Prompt**: "Add integration tests for POST /users: success, duplicate email, and an invalid email address."

The fixture's `tests/users.rs` already has the 201 and duplicate-409 tests, so a good answer adds only the third.

**Must produce**:
- says the success and duplicate cases already exist in `tests/users.rs` and adds only the invalid-email case there
- the new test starting from `test_app().await`
- requests sent through `app.server.post("/users").json(&..)`
- every response's status asserted before its body is read, follow-up reads (such as a `GET /users` after the POST) included
- the invalid-email case asserting 422 with `assert_status`
- the new test's name stating the outcome, such as `invalid_email_returns_422`

**Must not produce**:
- `sea_orm::MockDatabase`
- a mock of the service or repository layer
- `TestServer::new(app).unwrap()`
- `tests/common.rs`
- a hand-rolled `reqwest` client against a bound port

### Eval 2 - stabilising a flaky test

**Prompt**: This test in `tests/users.rs` is flaky on CI. Make it deterministic: `#[tokio::test] async fn lists_newest_user_first() { let app = common::test_app().await; app.server.post("/users").json(&json!({ "email": "ada@example.com", "name": "Ada" })).await; let db = app.state.db.clone(); tokio::spawn(async move { user::ActiveModel { id: Set(Uuid::now_v7()), email: Set("grace@example.com".to_owned()), name: Set("Grace".to_owned()), created_at: Set(Utc::now().into()) }.insert(&db).await.unwrap() }); tokio::time::sleep(Duration::from_millis(200)).await; let page = app.server.get("/users").await.json::<serde_json::Value>(); assert_eq!(page["items"][0]["email"], "grace@example.com"); assert_eq!(page["items"][0]["created_at"], json!(Utc::now())); }`

**Must produce**:
- the `tokio::time::sleep` replaced by awaiting the background task's `JoinHandle` or a oneshot it signals, or by doing the work inline before the request
- the background insert stamping `created_at` from the injected clock (`app.state.clock.now()`) or a value pinned relative to `common::frozen_now()`, never `Utc::now()`
- the `created_at` assertion pinned to the fixed clock (`common::frozen_now()`) rather than to `Utc::now()`
- an order the test can rely on: the endpoint's `created_at DESC, id DESC` ordering with both rows on the fixed clock, or an order-independent assertion

**Must not produce**:
- a longer sleep
- a retry loop that polls until the row appears
- `--test-threads 1`
- `start_paused = true` on a test that opens a database connection
- `serial_test`
- a global `Mutex` to serialise tests (nextest forks a process per test, so it serialises nothing)

### Eval 3 - test data factory

**Prompt**: "We keep copy-pasting the same 12-line user literal into tests. Give us a factory."

**Must produce**:
- a `bon::Builder` struct with `#[builder(default = ..)]` defaults drawn from `fake` fakers
- a conversion into `ActiveModel` (or the DTO)
- rows inserted through the database, not through the endpoint under test
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
- `app.mock` from `test_app()` used as the upstream, with `when`/`then` matchers
- the billing base URL setting given the `MockServer`'s `base_url()` inside `test_app()`
- the request sent through `app.server`, so the handler makes the outbound call
- `assert()`, `assert_async()` or `assert_calls(n)` on the mock proving the upstream was actually hit
- the billing-503 case asserts, with `assert_status`, the status the handler's `AppError` mapping gives (503 via `Unavailable` or 502 via `Http`), naming which

**Must not produce**:
- a real network request
- a hard-coded external host
- `mockall` on a one-implementor HTTP client trait added just for the test
- `mock.hits()`, `path_contains(..)` or `json_body_partial(..)`
- the test calling `app.state.http` or the mock URL itself

### Probe 1 - building a TestServer

**Prompt**: "Write a standalone test for GET /health/live that creates its own `TestServer` from the router instead of calling `test_app()`."

**Wrong answer**: `let\s+\w+\s*=\s*(?:axum_test::)?TestServer::new\([^;]*?\)\s*\.(?:unwrap|expect)\(`

**Right answer**: `TestServer::new\([^;]*?\)\s*;`

### Probe 2 - counting calls on a mock

**Prompt**: "Our test stubs the billing API with httpmock: `let billing = app.mock.mock(|when, then| { ... });`. Assert that it was called exactly twice."

**Wrong answer**: `(?:^|\n)[^\n`]*?billing\s*\.\s*(?:assert_)?hits(?:_async)?\(`

**Right answer**: `\.(?:assert_calls(?:_async)?\(\s*2\s*\)|calls(?:_async)?\(\s*\))`

### Probe 3 - matching part of a JSON body

**Prompt**: "Stub POST on any path that contains `/events` with httpmock, matching only when the JSON body has `kind` set to `signup`, whatever else it holds."

**Wrong answer**: `\.(?:path_contains|json_body_partial)\(\s*(?:r#*)?"`

**Right answer**: `json_body_includes\(|path_includes\(`

### Probe 4 - work a handler spawns

**Prompt**: "This test calls `test_app()`, POSTs to /users, whose handler spawns the welcome email, and then sleeps 200 ms before asserting the email mock was hit. Make it deterministic."

**Wrong answer**: `(?:#\[tokio::test\(\s*start_paused\s*=\s*true\s*\)\]\s*(?:pub\s+)?async\s+fn|sleep\(\s*(?:std::time::)?Duration::from_\w+\(\s*\d+\s*\)\s*\)\s*\.await\s*;)(?![\s\S]*\n\x60\x60\x60[^\n]*\n[\s\S]*\n\x60\x60\x60)`

**Right answer**: `JoinHandle|oneshot|Notify`

### Probe 5 - a setting for one test

**Prompt**: "One integration test needs `Settings::load()` to see `APP__DATABASE__URL` pointing at a second database. How do I set it inside the test?"

**Wrong answer**: `\bset_var\s*\(\s*"APP__`

**Right answer**: `from_map\(|HashMap::from\(`
