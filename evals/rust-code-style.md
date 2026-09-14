# rust-code-style

### Triggering

**Should load**

1. "Review this service module for style before I open the MR — it's Rust, axum, sea-orm."
2. "I need a request struct with a fixed set of status values. Which type for the statuses, and where does validation go?"
3. "error[E0502]: cannot borrow `items` as mutable because it is also borrowed as immutable — how do I fix this without cloning?"
4. "clippy says `needless_pass_by_value` on my `fn send_invoice(body: String)`. What should the signature be?"
5. "Should I define `trait UserRepository` with a `PgUserRepository` impl so I can swap in a fake in tests?"

**Should not load**

1. "Add a `[lints.clippy]` table and a `clippy.toml` to this repo." -> `rust-tooling`
2. "My handler fails with `the trait bound ... Handler<_, _> is not satisfied`." -> `axum-service`
3. "Write the `test_app()` helper and an rstest fixture for these API tests." -> `rust-testing`
4. "`DbErr::RecordNotUpdated` on insert — what's wrong with my `ActiveModel`?" -> `sea-orm-postgres`
5. "Write embedded firmware for an STM32 in Rust with no_std." -> `none` (outside the stack; no skill loads)
6. "Explain lifetimes and trait objects to me from scratch." -> `none` (a language tutorial; no skill loads)

### Eval 1 - clone to silence the borrow checker

**Prompt**: "This doesn't compile: `for item in items.iter() { if item.is_stale() { items.push(item.renewed()); } }`. I fixed it with `let snapshot = items.clone();` — is that fine?"

**Must produce**:
- A no: cloning to end a borrow is the banned pattern, and it allocates permanently to pay for a temporary scope problem.
- A rewrite that ends the read before the write — collect the new items into a local, then `extend`.
- Naming of the underlying diagnostic (E0502) or the rule "Ownership and Borrowing".

**Must not produce**:
- Approval of the clone, or `Rc`/`RefCell`/`Arc<Mutex<_>>` as the fix.
- A lifetime parameter added to a surrounding struct.

### Eval 2 - a trait with one implementation

**Prompt**: "I'm adding billing. I want `trait PaymentGateway` with `StripeGateway`, `trait UserRepository` with `PgUserRepository`, and `trait HttpClient` with `ReqwestClient`, so everything is mockable. Sketch the traits."

**Must produce**:
- Exactly one trait: the payment gateway, because taking a payment is a side effect a test cannot exercise.
- A refusal of the repository trait (real Postgres) and the HTTP client trait (httpmock), with those reasons.
- Injection through `AppState` / the constructor rather than a global.

**Must not produce**:
- All three traits, or a generic "traits improve testability" justification.
- `Box<dyn Error>` or `Result<T, String>` in the sketched signatures.

### Eval 3 - reviewing a generated handler module

**Prompt**: "Review this module: it has `pub async fn create_order(status: String, is_priority: bool, is_gift: bool, user_id: Uuid)`, calls `std::fs::read_to_string` for a template, `.unwrap()`s the database result, holds `state.cache.lock().unwrap()` across an `.await`, and returns `Result<Order, Box<dyn Error>>`."

**Must produce**:
- `status` as an enum and the two bools replaced by enums or a parameter struct; the single `user_id: Uuid` left as a bare `Uuid`, since the newtype rule fires on two or more ids in one signature.
- `tokio::fs` or `spawn_blocking` instead of `std::fs` in an `async fn`.
- Removal of `unwrap` (the poisoned cache lock recovered with `unwrap_or_else(PoisonError::into_inner)`, since a single cache insert cannot have left the map half-updated, not `expect("poisoned")`), and `Box<dyn Error>` replaced by a `thiserror` `OrderError` enum reaching `AppError` through `#[from]` — justified by the module rule: it has more than one failure mode (template, database), so it earns an enum.
- The lock either taken after the await or moved into a non-async helper, citing the guard-across-await rule.

**Must not produce**:
- `#[allow(clippy::unwrap_used)]` or any suppression as the fix.
- A `UserId` newtype for the lone id, with the `ToSchema` / `IntoParams` / `From` plumbing it drags in.
- A suggestion to add `Arc<Mutex<_>>` around more state, or to split the module into `handlers/`, `services/` and `repositories/` directories.
