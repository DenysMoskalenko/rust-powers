# rust-code-style

### Triggering

**Should load**

1. "Review this service module for style before I open the MR — it's Rust, axum, sea-orm."
2. "Our order `status` field is a `String` holding "pending", "paid" or "shipped". What type should it be, and how do I keep those exact strings in the JSON?"
3. "error[E0502]: cannot borrow `items` as mutable because it is also borrowed as immutable — how do I fix this without cloning?"
4. "clippy says `ptr_arg` on my `fn total(items: &Vec<LineItem>) -> Decimal`. What should the signature be?"
5. "I'm about to add `trait UserRepository` with a single `PgUserRepository` impl so the service layer doesn't depend on sea-orm directly. Good idea?"
6. "CI fails with `used expect() on a Result value` on `Regex::new(SKU_PATTERN).expect("pattern is valid")`, even though the message explains it. What's the fix?"

**Should not load**

1. "Add a `[lints.clippy]` table and a `clippy.toml` to this repo." -> `rust-tooling`
2. "My handler fails with `the trait bound ... Handler<_, _> is not satisfied`." -> `axum-service`
3. "Write the `test_app()` helper and an rstest fixture for these API tests." -> `rust-testing`
4. "`DbErr::RecordNotUpdated` on insert — what's wrong with my `ActiveModel`?" -> `sea-orm-postgres`
5. "Every build prints `warning: unknown lint: clippy::string_to_string` from the lints table in our Cargo.toml." -> `rust-tooling`
6. "Make CI fail on clippy warnings without setting `RUSTFLAGS` for the whole job." -> `rust-tooling`
7. "Should a request body that fails validation get a 400 or a 422, and where is that mapped?" -> `axum-service`
8. "My test sleeps 200 ms so the spawned task can finish before I assert, and it flakes on CI. Fix it." -> `rust-testing`

### Eval 1 - clone to silence the borrow checker

**Prompt**: "This doesn't compile: `for item in items.iter() { if item.is_stale() { items.push(item.renewed()); } }`. I fixed it with `let snapshot = items.clone();` — is that fine?"

**Must produce**:
- A no: the clone exists only to end a borrow, which is the pattern the rules ban.
- The reason: the clone is a permanent allocation paying for a temporary scope problem.
- A rewrite that ends the read before the write — collect the new items into a local, then `extend`.
- Why the borrow fails: `iter()` holds `items` while `push` needs it mutably.

**Must not produce**:
- Approval of the clone.
- `Rc`, `RefCell` or `Arc<Mutex<_>>` as the fix.
- A lifetime parameter added to a surrounding struct.

### Eval 2 - a trait with one implementation

**Prompt**: "I'm adding billing. I want `trait PaymentGateway` with `StripeGateway`, `trait UserRepository` with `PgUserRepository`, and `trait HttpClient` with `ReqwestClient`, so everything is mockable. Sketch the traits."

**Fixture**: empty

**Must produce**:
- Exactly one trait: the payment gateway, because taking a payment is a side effect a test cannot exercise.
- A refusal of the repository trait, because tests run against a real Postgres.
- A refusal of the HTTP client trait, because `httpmock` stands in for the upstream.
- Injection through `AppState` or the constructor rather than a global.
- The gateway's async method written as `fn ... -> impl Future<Output = ...> + Send` when callers are generic, or under `#[async_trait]` when it is stored as `dyn` in `AppState`; never a bare `async fn` in a `pub` trait.

**Must not produce**:
- All three traits.
- A generic "traits improve testability" justification.
- `Box<dyn Error>` or `Result<T, String>` in the sketched signatures.

### Eval 3 - reviewing a generated handler module

**Prompt**: "Review this module: it has `pub async fn create_order(status: String, is_priority: bool, is_gift: bool, user_id: Uuid)`, calls `std::fs::read_to_string` for a template, `.unwrap()`s the database result, holds `state.cache.lock().unwrap()` across an `.await`, and returns `Result<Order, Box<dyn Error>>`."

**Must produce**:
- `status` as an enum rather than a `String`.
- The two `bool` parameters replaced by enums or a parameter struct.
- The single `user_id: Uuid` left as a bare `Uuid`, since the newtype rule fires on two or more ids in one signature.
- The template loaded once through `include_str!` or a `LazyLock` instead of a file read on every call.
- The database result's `.unwrap()` replaced by the `?` operator.
- The cache lock recovered with `unwrap_or_else(PoisonError::into_inner)`, since a single cache insert cannot have left the map half-updated.
- `Box<dyn Error>` replaced by a typed error: a `thiserror` `OrderError` reaching `AppError` through `#[from]` when the module keeps more than one failure mode, otherwise `AppError` directly.
- The lock either taken after the await or moved into a non-async helper, citing the guard-across-await rule.

**Must not produce**:
- `#[allow(clippy::unwrap_used)]` or any other suppression as the fix.
- `.expect("...")` with an explanatory message as the replacement for an `unwrap()`.
- `tokio::fs::read_to_string` of the template on every call.
- A `UserId` newtype for the lone id, with the `ToSchema` / `IntoParams` / `From` plumbing it drags in.
- A suggestion to add `Arc<Mutex<_>>` around more state.
- A split of the module into `handlers/`, `services/` and `repositories/` directories.

### Eval 4 - an expect that explains itself

**Prompt**: "clippy fails with `used expect() on a Result value` on `static SKU: LazyLock<Regex> = LazyLock::new(|| Regex::new(SKU_PATTERN).expect("the pattern is a valid constant"));`. The message already says why it cannot fail. What is the right fix?"

**Must produce**:
- `#[expect(clippy::expect_used, reason = "...")]` on the `static` item, with a reason that says why the pattern cannot fail.
- The explanation that `expect_used` fires on every `.expect()` outside tests, whatever its message says.
- `std::sync::LazyLock` kept for the static.

**Must not produce**:
- `#[allow(clippy::expect_used)]`, or any crate-wide `#![allow]`.
- Turning `expect_used` off in the `[lints]` table or `clippy.toml`.
- A claim that a clearer `expect` message satisfies the lint.
- `once_cell` or `lazy_static` for the static.

### Probe 1 - async method in a public trait

**Prompt**: "Add a public `Mailer` trait with an async `send(&self, to: &str, body: &str)` method and an SMTP implementation. The signup service is generic over `M: Mailer` and calls `send` inside `tokio::spawn`."

**Wrong answer**: `(?:^|\n)[ \t]*pub trait Mailer\b[^{]*\{[^}]*\n[ \t]*async fn|#\[(?:async_trait::)?async_trait\]\s*(?:pub\s+)?(?:trait|impl)\b`

**Right answer**: `fn send\s*\([^)]*\)\s*->\s*impl\s+(?:[\w:]+::)?Future<[\s\S]{0,120}?>\s*\+\s*Send`

### Probe 2 - a global compiled regex

**Prompt**: "I need one compiled `Regex` for validating SKUs, built once and shared by every handler. Write the static."

**Wrong answer**: `(?:^|\n)[ \t]*use\s+once_cell|once_cell::sync::Lazy<|lazy_static!\s*\{|(?:^|\n)[ \t]*(?:once_cell|lazy_static)\s*=`

**Right answer**: `LazyLock<`

### Probe 3 - a cast that cannot truncate

**Prompt**: "clippy flags `cast_possible_truncation` on `let count = items.len() as u32;`. The list is capped at 1000 items. Make the warning go away properly."

**Wrong answer**: `#!?\[allow\(\s*clippy::cast_possible_truncation[^\]]*\]\s*\n\s*(?:let|fn|pub|const|static)\b`

**Right answer**: `u32::try_from\(|#\[expect\(\s*clippy::cast_possible_truncation`

### Probe 4 - expect at startup

**Prompt**: "Write the static that compiles our constant `SKU_PATTERN` into a `Regex` once at startup. It has to pass `make lint`."

**Wrong answer**: `^(?![\s\S]*expect\(\s*clippy::expect_used)[\s\S]*LazyLock::new\(\s*(?:move\s*)?\|\|\s*\{?\s*(?:regex::)?Regex::new\([^)]*\)\s*\.expect\(`

**Right answer**: `#\[expect\(\s*clippy::expect_used\s*,\s*reason\s*=`

### Probe 5 - converting test helpers to expect

**Prompt**: "Our style guide says `#[expect]` replaces `#[allow]`. Convert the `#![allow(...)]` attributes under `tests/` to `#![expect(...)]`."

**Wrong answer**: `(?:^|\n)[ \t]*#!\[expect\(\s*dead_code`

### Probe 6 - a poisoned lock

**Prompt**: "Our order handler reads a `std::sync::Mutex<HashMap<OrderId, Order>>` from state with `let cache = state.cache.lock().unwrap();`. Replace that `.unwrap()`."

**Wrong answer**: `(?:^|\n)[^\n`]*\.lock\(\)\s*\.expect\("`
