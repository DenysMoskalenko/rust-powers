# Test data factories

Test data comes from two crates, not one: `fake` supplies random-but-valid values, `bon` supplies "valid default, override one field". Between them they cover every factory shape a test needs.

- [Which tool for which job](#which-tool-for-which-job)
- [fake: any valid instance](#fake-any-valid-instance)
- [bon: valid default, override one field](#bon-valid-default-override-one-field)
- [Seeding for determinism](#seeding-for-determinism)
- [Parametrized cases](#parametrized-cases)
- [Gotchas](#gotchas)

## Which tool for which job

| Need | Use |
|---|---|
| A whole DTO nobody asserts on | `#[derive(Dummy)]` on the struct, then `Faker.fake()` |
| A realistic single value | a faker such as `SafeEmail().fake::<String>()` |
| A row with one meaningful field | a `bon::Builder` factory whose defaults come from fake |
| A value an assertion depends on | a literal, or a seeded RNG |

The factory below is shown inline in the test file; move it to `tests/common/mod.rs` beside `test_app()` when a second file needs it. If unit tests need it too, put it behind `#[cfg(test)] pub mod factories` in the library instead.

## fake: any valid instance

```rust,verify,test
use fake::faker::internet::en::SafeEmail;
use fake::faker::name::en::Name;
use fake::{Dummy, Fake, Faker};

/// `#[derive(Dummy)]` is "just give me a valid instance"; `#[dummy(faker = ..)]`
/// is the `Use(lambda: faker...)` equivalent. Locale lives in the module path,
/// so `name::fr_fr::Name` is the French generator.
#[derive(Debug, Clone, Dummy)]
struct SignupPayload {
    #[dummy(faker = "SafeEmail()")]
    email: String,
    #[dummy(faker = "Name()")]
    name: String,
    #[dummy(faker = "18..90")]
    age: u8,
    nickname: Option<String>,
    #[dummy(faker = "(Faker, 1..4)")]
    tags: Vec<String>,
}

#[test]
fn faker_fills_every_field_with_something_valid() {
    let payload: SignupPayload = Faker.fake();

    assert!(payload.email.contains('@'));
    assert!(!payload.name.is_empty());
    assert!((18..90).contains(&payload.age));
    assert!(!payload.tags.is_empty());
    // Option<T> needs no attribute; fake decides whether to fill it.
    assert!(payload.nickname.as_ref().is_none_or(|n| !n.is_empty()));
}
```

`Option<T>` needs no attribute. With the `chrono`, `uuid` and `rust_decimal` features enabled, `DateTime<Utc>`, `Uuid` and `Decimal` fields need none either. Collections take a `(generator, len-range)` tuple.

## bon: valid default, override one field

`#[builder(default = expr)]` is evaluated at `build()` time, so every call gets fresh data rather than one value shared across the suite. The factory holds plain fields and converts at the end, which keeps one factory usable for both an HTTP payload and an `ActiveModel`.

```rust,verify,test
mod common;

use app::entities::user;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use fake::Fake;
use fake::faker::internet::en::SafeEmail;
use fake::faker::name::en::Name;
use sea_orm::{ActiveModelTrait, Set};
use uuid::Uuid;

#[derive(Debug, Clone, bon::Builder)]
#[builder(start_fn = user, finish_fn = build)]
struct UserFactory {
    #[builder(into, default = Uuid::now_v7())]
    id: Uuid,
    #[builder(into, default = SafeEmail().fake::<String>())]
    email: String,
    #[builder(into, default = Name().fake::<String>())]
    name: String,
    #[builder(default = common::frozen_now())]
    created_at: DateTime<Utc>,
}

impl UserFactory {
    fn into_active_model(self) -> user::ActiveModel {
        user::ActiveModel {
            id: Set(self.id),
            email: Set(self.email),
            name: Set(self.name),
            // The generated column is `DateTimeWithTimeZone`; `Utc` converts into it.
            created_at: Set(self.created_at.into()),
        }
    }
}

#[tokio::test]
async fn seeded_row_is_visible_through_the_api() {
    let app = common::test_app().await;
    let row = UserFactory::user()
        .email("ada@example.com")
        .build()
        .into_active_model()
        .insert(&app.state.db)
        .await
        .unwrap();

    let response = app.server.get(&format!("/users/{}", row.id)).await;

    response.assert_status(StatusCode::OK);
    assert_eq!(response.json::<serde_json::Value>()["email"], "ada@example.com");
}
```

Only the field the test cares about is named; `#[builder(into)]` lets a `&str` stand in for a `String`. `Option<T>` fields are optional with no attribute, and bare `#[builder(default)]` means `Default::default()`.

Seed rows through the database or the service, not through the endpoint under test — otherwise a failure in the endpoint fails every test that merely needed data.

## Seeding for determinism

Seed the RNG when a failure has to reproduce byte for byte: the same seed yields the same payload on every machine, so the request that failed can be replayed. The assertion still compares the response with the payload the test sent, never one helper call with another.

```rust,verify,test
mod common;

use axum::http::StatusCode;
use fake::faker::internet::en::SafeEmail;
use fake::faker::name::en::Name;
use fake::{Dummy, Fake, Faker};
use rand::SeedableRng;
use rand::rngs::StdRng;
use serde::Serialize;

#[derive(Debug, Clone, Dummy, Serialize)]
struct SignupPayload {
    #[dummy(faker = "SafeEmail()")]
    email: String,
    #[dummy(faker = "Name()")]
    name: String,
}

fn seeded_signup(seed: u64) -> SignupPayload {
    let mut rng = StdRng::seed_from_u64(seed);
    Faker.fake_with_rng(&mut rng)
}

#[tokio::test]
async fn a_seeded_signup_is_stored_as_sent() {
    let app = common::test_app().await;
    let payload = seeded_signup(42);

    let response = app.server.post("/users").json(&payload).await;

    response.assert_status(StatusCode::CREATED);
    let body = response.json::<serde_json::Value>();
    assert_eq!(body["email"], payload.email);
    assert_eq!(body["name"], payload.name);
}
```

fake 5.1 is built on rand 0.10, so the dev-dependency must be `rand = "0.10"`. rand 0.9 compiles but produces a `StdRng` from a different crate version and `fake_with_rng` will not accept it.

## Parametrized cases

`#[case]` is the tuple form and `#[values]` the cartesian product. Name every case: nextest reports them as `create_user_rejects_bad_email::case_1_missing_at`, while generated `#[values]` names are unreadable.

```rust,verify,test
mod common;

use axum::http::StatusCode;
use rstest::{fixture, rstest};
use serde_json::json;

#[fixture]
async fn app() -> common::TestApp {
    common::test_app().await
}

#[rstest]
#[case::missing_at("not-an-email")]
#[case::empty("")]
#[case::spaces_only("   ")]
#[awt]
#[tokio::test]
async fn create_user_rejects_bad_email(#[future] app: common::TestApp, #[case] email: &str) {
    let response = app
        .server
        .post("/users")
        .json(&json!({ "email": email, "name": "Ada" }))
        .expect_failure()
        .await;

    response.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
}
```

`#[rstest]` comes first, `#[awt]` before `#[tokio::test]`, and `#[future]` marks the parameter that is a fixture returning a future. Without `#[awt]` the body would receive the future itself.

## Gotchas

| Symptom | Cause | Fix |
|---|---|---|
| `fake_with_rng` rejects the RNG | rand 0.9 in dev-dependencies | pin `rand = "0.10"` |
| `Dummy` not found | the `derive` feature is off | `fake = { version = "5", features = ["derive", "chrono", "uuid", "rust_decimal"] }` |
| Every test sees the same timestamp value | a `const` default instead of an expression | `#[builder(default = expr)]` is evaluated per `build()` |
| A test asserts on a faked name | the value was never pinned | override the field, or seed the RNG |
