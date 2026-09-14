# Mocking the boundaries

- [What earns a mock](#what-earns-a-mock)
- [Outbound HTTP with httpmock](#outbound-http-with-httpmock)
- [Trait doubles: mockall or a hand-written fake](#trait-doubles-mockall-or-a-hand-written-fake)
- [Waiting for background work](#waiting-for-background-work)
- [Time](#time)
- [Tracing output in tests](#tracing-output-in-tests)

## What earns a mock

| Dependency | Test double |
|---|---|
| Postgres | none — a real database per test |
| The service or repository layer | none — call it for real |
| Outbound HTTP | `httpmock::MockServer`, exposed by `test_app()` as `app.mock` |
| The clock | the injected `Clock` trait |
| Email, payments, SMS, an LLM | a trait with a hand-written fake, or mockall |
| A rig agent | see `building-rig-agents` |

The rule behind the table: a double is justified when the real implementation has a side effect a test cannot exercise. Postgres and HTTP both have real, cheap, local stand-ins, so neither qualifies.

## Outbound HTTP with httpmock

`test_app()` starts a `MockServer` and exposes it as `app.mock`; `app.state.http` is the production client, untouched. When the service gains an upstream, add its base URL to `Settings`, have `test_app()` hand that setting `app.mock.base_url()`, and test through the route so the handler makes the call:

```rust,ignore
// Eval shape for an endpoint that calls a billing API. Nothing here talks to
// the mock server directly: the handler does, through the settings it read.
// The 503 assumes the handler maps an upstream 5xx onto `AppError::Unavailable`;
// a bare `?` on the reqwest error is `AppError::Http`, which renders 502.
#[tokio::test]
async fn profile_maps_a_billing_outage_to_503() {
    let app = common::test_app().await;
    let user_id = seed_user(&app).await;
    let billing = app.mock.mock(|when, then| {
        when.method(GET).path(format!("/accounts/{user_id}"));
        then.status(503);
    });

    let response = app
        .server
        .get(&format!("/users/{user_id}/profile"))
        .expect_failure()
        .await;

    response.assert_status(StatusCode::SERVICE_UNAVAILABLE);
    billing.assert(); // the handler really went to billing, not somewhere else
}
```

The scaffold has no such endpoint yet, so the block below demonstrates the matcher API by driving `app.state.http` at `app.mock.base_url()` by hand. It tests httpmock and reqwest, not the service; a real test has the shape above.

```rust,verify,test
mod common;

use httpmock::Method::{GET, POST};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn upstream_profile_is_parsed() {
    let app = common::test_app().await;
    let id = Uuid::now_v7();
    let upstream = app
        .mock
        .mock_async(|when, then| {
            when.method(GET).path(format!("/profiles/{id}"));
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "id": id.to_string(), "plan": "pro" }));
        })
        .await;

    let body: serde_json::Value = app
        .state
        .http
        .get(format!("{}/profiles/{id}", app.mock.base_url()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["plan"], "pro");
    upstream.assert_async().await;
    assert_eq!(upstream.calls_async().await, 1);
}

#[tokio::test]
async fn the_event_we_send_carries_the_signup_kind() {
    let app = common::test_app().await;
    let upstream = app
        .mock
        .mock_async(|when, then| {
            when.method(POST)
                .path("/events")
                .json_body_includes(r#"{ "kind": "signup" }"#);
            then.status(202);
        })
        .await;

    let response = app
        .state
        .http
        .post(format!("{}/events", app.mock.base_url()))
        .json(&json!({ "kind": "signup", "user": "ada" }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 202);
    upstream.assert_async().await;
}
```

`assert_async()` fails the test when the mock was never called, which catches a request that silently went somewhere else — the failure mode a permissive stub hides. The matcher surface is wide and consistently named: `path_prefix`, `path_suffix`, `path_matches`, `query_param`, `header`, `body_includes`, `json_body_includes`, `json_body_obj(&T)`, plus `is_true(fn)` / `is_false(fn)` as escape hatches (`matches(fn)` is deprecated and fails `-D warnings`).

A body matcher, as in the second test, asserts the outbound contract: when the handler's payload drifts, the mock stops matching, the call gets a 404 and `assert_async()` reports zero calls. That only means something when the handler builds the payload — in the demo the test builds it, so the matcher can never fail.

The blocking `mock(|when, then| ..)` builder works inside a `#[tokio::test]`; `mock_async` only matters when several mocks are built concurrently.

## Trait doubles: mockall or a hand-written fake

mockall earns its place on a wide trait where the test needs per-call argument matching:

```rust,verify,test
#![allow(
    clippy::unused_async_trait_impl,
    reason = "mockall generates trivially-sync impls"
)]

use mockall::predicate::eq;
use mockall::automock;

/// Kept private: `async fn` in a public trait trips `async_fn_in_trait`,
/// because the returned future carries no `Send` bound.
#[automock]
trait EmailSender: Send + Sync {
    async fn send(&self, to: &str, subject: &str) -> Result<(), String>;
    fn is_configured(&self) -> bool;
}

/// The code under test: generic, so production passes the real sender and the
/// test passes the mock with no `dyn` anywhere.
async fn welcome(sender: &impl EmailSender, email: &str) -> Result<(), String> {
    if !sender.is_configured() {
        return Err("email is not configured".to_owned());
    }
    sender.send(email, "Welcome").await
}

#[tokio::test]
async fn welcome_email_goes_to_the_new_address() {
    let mut sender = MockEmailSender::new();
    sender.expect_is_configured().return_const(true);
    sender
        .expect_send()
        .with(eq("ada@example.com"), eq("Welcome"))
        .times(1)
        .returning(|_, _| Ok(()));

    welcome(&sender, "ada@example.com").await.unwrap();
    // `with(..)` asserted the arguments `welcome` derived; `times(1)` is
    // verified when the mock drops.
}
```

Native `async fn` in a trait is the default (static dispatch, no boxing) and `#[async_trait]` is only for a trait genuinely stored as `dyn` — `Arc<dyn Notifier>` in `AppState` — because a native `async fn` trait is not dyn-compatible; see `rust-code-style`. For that case put `#[async_trait::async_trait]` **under** `#[automock]`:

```rust,verify,test
use std::sync::Arc;

use mockall::automock;
use mockall::predicate::eq;

#[automock]
#[async_trait::async_trait]
trait Notifier: Send + Sync {
    async fn notify(&self, message: &str) -> Result<u64, String>;
}

async fn announce_signup(notifier: &dyn Notifier, name: &str) -> Result<u64, String> {
    notifier.notify(&format!("{name} signed up")).await
}

#[tokio::test]
async fn signup_announcement_names_the_user() {
    let mut mock = MockNotifier::new();
    mock.expect_notify()
        .with(eq("Ada signed up"))
        .times(1)
        .returning(|_| Ok(1));
    let notifier: Arc<dyn Notifier> = Arc::new(mock);

    let delivered = announce_signup(notifier.as_ref(), "Ada").await.unwrap();

    assert_eq!(delivered, 1);
}
```

For a one- or two-method trait a recording fake is shorter than the mockall setup and lets the test assert on what actually happened instead of pre-declaring it:

```rust,verify,test
#![allow(clippy::unwrap_used, reason = "the fake's own methods are not #[test] bodies")]

use std::sync::{Arc, Mutex};

trait EmailSender: Send + Sync {
    fn send(&self, to: &str) -> Result<(), String>;
}

#[derive(Default, Clone)]
struct RecordingSender(Arc<Mutex<Vec<String>>>);

impl RecordingSender {
    fn sent(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

impl EmailSender for RecordingSender {
    fn send(&self, to: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(to.to_owned());
        Ok(())
    }
}

/// The code under test.
fn notify_all(sender: &dyn EmailSender, recipients: &[&str]) -> Result<usize, String> {
    recipients.iter().try_for_each(|to| sender.send(to))?;
    Ok(recipients.len())
}

#[test]
fn every_recipient_gets_one_email() {
    let sender = RecordingSender::default();

    let count = notify_all(&sender, &["ada@example.com", "grace@example.com"]).unwrap();

    assert_eq!(count, 2);
    similar_asserts::assert_eq!(
        sender.sent(),
        vec!["ada@example.com".to_owned(), "grace@example.com".to_owned()]
    );
}
```

Rule of thumb: a clock or an email sender is a hand-written fake; a six-method port with per-call argument matching is mockall.

## Waiting for background work

A `tokio::time::sleep` before the assertion is a bet on the CI box. Keep the `JoinHandle` and await it: it carries the task's `Result` and its panic, a sleep carries neither. For work the handler spawns and does not hand back, have it signal a `oneshot`/`Notify` the test owns.

```rust,verify,test
mod common;

use app::entities::user;
use sea_orm::ActiveModelTrait as _;
use sea_orm::ActiveValue::Set;
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn a_row_written_by_a_background_task_is_listed() {
    let app = common::test_app().await;
    let db = app.state.db.clone();
    let now = app.state.clock.now();
    let job = tokio::spawn(async move {
        user::ActiveModel {
            id: Set(Uuid::now_v7()),
            email: Set("job@example.com".to_owned()),
            name: Set("job".to_owned()),
            created_at: Set(now.into()),
        }
        .insert(&db)
        .await
    });

    // Not `sleep(200ms)`: the handle resolves exactly when the insert has.
    job.await.unwrap().unwrap();

    let response = app.server.get("/users").await;
    response.assert_status_ok();
    let page = response.json::<serde_json::Value>();
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"][0]["email"], "job@example.com");
    assert_eq!(page["items"][0]["created_at"], json!(common::frozen_now()));
}
```

## Time

There is no freezegun in Rust. freezegun monkey-patches `datetime.now` process-wide, and Rust offers no interception point: `Utc::now()` is a direct call with no import hook. The alternatives are injecting the clock or having every call site consult a global `AtomicI64`, which is a worse version of injection. So the service holds `Arc<dyn Clock>` and `test_app()` hands it a `FixedClock`.

`Clock` is the stated exception to the ban on one-implementor traits, because the real implementation reads wall-clock time and a test cannot.

`#[tokio::test(start_paused = true)]` is **not** the fix for a test that touches Postgres, httpmock or any socket: paused time auto-advances whenever every task is idle, which includes "waiting on a socket", so every timeout on the way to the database (testcontainers' Docker request, sqlx's connect and acquire — all tokio timers) fires instantly and `test_app()` panics at once with `RequestTimeoutError` or `PoolTimedOut`. Paused time is for pure timer logic, no real I/O in the test:

```rust,verify,test
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// `start_paused = true` freezes tokio's clock and auto-advances whenever every
/// task is idle, so a 24 h sleep finishes instantly and deterministically.
#[tokio::test(start_paused = true)]
async fn a_long_sleep_costs_nothing() {
    let start = tokio::time::Instant::now();

    tokio::time::sleep(Duration::from_hours(24)).await;

    assert!(start.elapsed() >= Duration::from_hours(24));
}

#[tokio::test(start_paused = true)]
async fn an_interval_ticks_once_per_advance() {
    let ticks = Arc::new(Mutex::new(0_u32));
    let counter = Arc::clone(&ticks);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            *counter.lock().unwrap() += 1;
        }
    });

    // The spawned task has not run yet, so a single advance(180s) would create
    // the interval after the jump and it would tick once. Step by the period
    // and yield so the task observes every tick.
    for _ in 0..3 {
        tokio::time::advance(Duration::from_secs(60)).await;
        tokio::task::yield_now().await;
    }

    assert_eq!(*ticks.lock().unwrap(), 3);
}
```

`start_paused` moves `tokio::time` only — `sleep`, `interval`, `timeout`, `Instant`. It does not move `Utc::now()`; the two mechanisms are complementary. Code that must run under paused time and real I/O takes its timers from an injected source instead, but that is rare enough to treat as a design smell. Both need `tokio = { version = "1.53", features = ["test-util"] }` in dev-dependencies, because `full` does not include `test-util`.

## Tracing output in tests

```rust
#[test_log::test(tokio::test)]
async fn emits_a_span_for_the_request() {
    tracing::info!(user_id = %uuid::Uuid::nil(), "created user");
}
```

`#[test_log::test]` installs a subscriber per test driven by `RUST_LOG`, and wraps the inner attribute, so `tokio::test` goes inside the parentheses. Output is still captured unless the run uses `--no-capture`; `RUST_LOG_SPAN_EVENTS=new,close` adds span lifecycle events. Use it to read logs while debugging a test, not to assert on them: asserting on log text couples the test to wording that is free to change.
