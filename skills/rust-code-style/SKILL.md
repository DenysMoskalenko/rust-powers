---
name: rust-code-style
description: "Use when writing or reviewing Rust for an axum or tokio backend service — ownership and cloning, thiserror versus anyhow, no-panic rules, newtypes, derive_more and strum, async discipline, module layout, dependency injection. Also for E0502, E0499, a clone added to satisfy the borrow checker, future cannot be sent between threads safely, or a clippy finding in code such as ptr_arg or needless_pass_by_value. Not for lint configuration (rust-tooling), framework patterns (axum-service), or tests (rust-testing)."
metadata:
  version: "0.2.0"
---

# Rust Code Style

Assumes Rust 1.98 edition 2024, tokio 1.53, thiserror 2, anyhow 1, serde 1, bon 3, derive_more 2, strum 0.28, itertools 0.15.

Names such as `AppError`, `test_app()`, `Valid<T>` and the Makefile targets come from the rust-scaffolding template. In a project built differently, use its own types, helpers and tooling, map outcomes onto its nearest existing error variant, and say so when none fits instead of adding one. Apply these rules to new code; when editing existing code, keep its public contract and tuned configuration and report differences instead of rewriting, unless asked. If `Cargo.lock` pins another major or minor version than the line above, follow the project and say which rules may not apply.

## Important

- Never `.clone()` to silence the borrow checker; end the borrow with a smaller scope, `&` instead of `&mut`, or ownership in the signature.
- No `unwrap()` or `expect()` outside tests: return the error with context, or, where failure is impossible, put `#[expect(clippy::expect_used, reason = "why it cannot fail")]` on the item.
- A `thiserror` enum per domain module with matched-on variants or more than one failure mode; `anyhow` for errors only logged; production code never returns `Box<dyn Error>` or `Result<T, String>`, which lose the source chain.
- Never block the runtime: `spawn_blocking` for sync or CPU-bound work past about 100 microseconds.
- No trait with a single implementation; the only injected traits are for side effects a test cannot exercise (time, email, payments, an LLM).

## References

- `references/error-handling.md` — adding or changing an error type: thiserror versus anyhow, domain enums, the `#[from]` / `#[source]` / `#[error(transparent)]` table.
- `references/async-patterns.md` — before a `select!`, background work, or an async trait: cancellation safety, structured concurrency, shutdown.
- `references/diagnostics.md` — a pasted rustc error or clippy lint name: what it means, the fix, why the lint is on.

*house* marks this project's preference, not a Rust-wide rule; *lint* marks one the scaffold's `[lints]` table enforces, so quote the lint rather than restate it.

## Ownership and Borrowing

```rust
let all = items.clone();                            // Bad: clone to dodge the borrow checker
for i in &all { if i.is_stale() { items.push(i.renewed()); } }

let renewed: Vec<Item> = items.iter().filter(|i| i.is_stale()).map(Item::renewed).collect();
items.extend(renewed);                              // Good: the read ends before the write starts
```

Cheap to clone: `Arc`, `Uuid`, `DatabaseConnection`, a `String` at an API boundary. Not: in a loop, on a `Vec` of domain objects, on `AppState`.

Take `&str` unless the function stores the value; then `String`, or `impl Into<String>` in a constructor. `&[T]` never `&Vec<T>`. A lifetime on a service struct is a smell: own the data or an `Arc`.

## Shared State

Take the first rung that works:

1. **No sharing.** `DatabaseConnection` is `Clone` and internally synchronised — hold it by value in `AppState`, never `Arc<Mutex<_>>`.
2. **Read-mostly.** `Arc<Settings>`, no lock.
3. **Mutable, with a natural owner task or an ordering requirement.** `tokio::sync::mpsc` to that one owner: no poisoning, no ordering bugs. A short synchronous critical section has neither — take rung 4 directly.
4. **Only then a lock**, `std::sync` when the critical section is synchronous, `tokio::sync` when it must await.

Lock inside a non-async method so the guard cannot reach an `.await`, and settle check-then-insert races with `entry()`. Keep a critical section short and panic-free. Recover a poisoned lock with `lock().unwrap_or_else(PoisonError::into_inner)` only where its data cannot be half-updated (`references/error-handling.md`).

## Errors

A single-variant wrapper is not worth an enum. Domain errors live in the module that raises them and reach `AppError` through `#[from]`; every variant names the concrete thing that failed, with ids, or is `#[error(transparent)]`. Add `anyhow` context once per meaningful operation, not at every `?`. Log an error only where the response is decided or the error is swallowed. For status mapping see `axum-service`.

## No Panics in Production Paths

`unwrap_used` and `expect_used` (*lint*) fire on every call outside tests, whatever the message says: a constant regex compiled at startup carries `#[expect(clippy::expect_used, reason = "the pattern is a compile-time constant")]` on its item, never an `#[allow]`. `todo!` and `unimplemented!` never reach a commit (*lint*). In the service a panic becomes a logged 500 via `CatchPanicLayer`; still never panic on purpose.

## Types Carry the Invariant

A value from a fixed set is an enum, never a `String`. A named `bool` DTO field is fine; two or more `bool` parameters on a function are not — `list_users(true, false)` is unreadable and trivially swapped. Derive its wire form with `strum` and `serde(rename_all)`.

```rust
fn transfer(from: Uuid, to: Uuid, amount: Decimal);         // Bad: swapping arguments compiles
fn transfer(from: AccountId, to: AccountId, amount: Money);  // Good
```

Ids are bare `Uuid` by default — generated sea-orm entities, `Path<Uuid>` and the DTOs all use it, and a newtype per id costs `ToSchema`, `IntoParams` and `find_by_id` plumbing. Newtype an id when one signature takes two or more of them or the value has a validity rule; `struct AccountId(Uuid)` with `derive_more` is enough, `nutype` only when validation is intrinsic.

Derive order (*house*): `Debug, Clone, Copy, PartialEq, Eq, Hash, Default`, then third-party, with `Debug` on everything. `Default` only where the zero value is meaningful. Private fields plus a constructor keep invariants true. `From`/`TryFrom` for conversions, never `to_domain()`. `bon::Builder` at four or more fields, or two optional ones.

`derive_more` for newtype impls (`Display`, `From`, `Add`), never `Deref`, which fakes inheritance. `itertools` for `chunks`, `unique`, `izip!`, not to replace a two-line loop.

## Async Discipline

`tokio::fs` over `std::fs`; static data such as a template is `include_str!` or `LazyLock` once, never a read per request. `spawn_blocking` for password hashing, image work, any sync client; such a task cannot be aborted, so a loop that never ends gets `std::thread::spawn`.

Native `async fn` in traits, but a `pub` trait writes the method out as `fn send(&self, ...) -> impl Future<Output = T> + Send;`: rustc's `async_fn_in_trait` fires on the sugar there, because the returned future promises no `Send` bound. `#[async_trait]` only when the trait is stored as `dyn`. An `async fn` with no `.await` is sync (*lint* `unused_async`). Drop a `JoinHandle` only for fire-and-forget work that logs its own failure inside the task; otherwise the `JoinError` and the panic go with it. Before a `select!`, read `references/async-patterns.md`: a branch that is not cancel-safe loses data when another wins.

## Control Flow, Iterators and Numbers

`let else` for guard clauses, so the happy path stays unindented. Combinators while the steps only reshape a value, a `match` once the arms carry different logic. Match your own enums exhaustively: `_ =>` lets a new variant compile silently.

```rust
// Bad: three arms that only rewrap; `?` and `ok_or` already say this.
match repo.find(id).await { Ok(Some(u)) => Ok(u), Ok(None) => Err(UserError::NotFound(id)), Err(e) => Err(e.into()) }
repo.find(id).await?.ok_or(UserError::NotFound(id))   // Good
```

Iterators over index loops; past four chained adaptors, a `for` loop (*house*).

`as` between integer types is banned in domain code because it truncates silently (*lint* `cast_possible_truncation`): use `u32::try_from(n)?` or the infallible `i64::from(x)`, and write `#[expect(clippy::cast_possible_truncation, reason = "...")]` where a cast is right. `usize` is for in-memory indices only; a database column is `i32`/`i64` (sea-orm's output), a wire count, limit or offset is `u64` (the pagination envelope's). Money is `rust_decimal`; counters use `checked_add`/`saturating_sub`, because debug panics and release wraps.

## Module Layout and Visibility

`main.rs` is thin: everything testable lives in `lib.rs` and below, because integration tests can only import the lib target. For the service tree and the one-ORM-statement-per-handler rule see `axum-service`.

`foo.rs` beside `foo/` for modules you write by hand, never mixed with `mod.rs` at one level (*house*). `mod.rs` where a generator or a convention fixes it: `tests/common/` (cargo), generated `src/entities/` (sea-orm-cli), `src/api/` (*house*). No `utils.rs`, `helpers.rs` or `types.rs`, and no crate-wide prelude (*house*): a `prelude::*` makes every file's dependencies invisible. Imports in three blocks — std, external crates, then `crate` (*house*; rustfmt does not enforce it).

Three visibility levels only: private, `pub(crate)`, `pub`. No `pub(super)` or `pub(in path)` (*house*): moving the module silently changes the caller set. Never widen visibility for a test. A new workspace crate needs a second consumer.

## Documentation

`//!` for a module's purpose, `///` for non-obvious public items: one sentence, then `# Errors` and `# Panics` where they apply. No doc comments on private helpers, no comment narrating the next line — code says what, comments say why.

## Dependency Injection

Constructors take their collaborators; `AppState` is the composition root; no globals or service singletons.

Beyond the effect seams in Important, injected through `AppState` as `Clock` already is, a trait earns its keep only for a second implementation you can name today. A mock of your own repository only proves the mock behaves, while real Postgres and `httpmock` exercise the thing that breaks.

## Reuse, Suppressions and Project Style

Check std and the crates already locked before hand-writing a retry loop, a builder or a `Display` impl. Read the callers before editing; extend a near-duplicate rather than adding another.

Fix the finding rather than silencing it. A justified suppression is `#[expect(lint, reason = "...")]`, which warns once the lint stops firing (*lint* `allow_attributes_without_reason`). Never `#![deny(warnings)]`. Priority when rules collide: KISS, YAGNI, single responsibility, DRY after the third repetition, fail fast.

## Valid Patterns — Do Not Flag

- `#[expect(clippy::expect_used, reason = "...")]` on an item whose `expect` cannot fail, and `unwrap` or `expect` under `cfg(test)` or in `tests/`.
- `#![allow(lint, reason = "...")]` in a file several targets compile, such as `tests/common/mod.rs`, when the lint fires in only some: `#![expect]` there fails as unfulfilled.
- A `match` with one arm per variant and no `_` arm — exhaustiveness is the point.
- A module named after its type (`user::User`).
- A bare `Uuid` id in an entity, a `Path<Uuid>`, a DTO, or a function taking one id.

## Red Flags — STOP

| About to… | Rule to apply |
|---|---|
| Add `.clone()` because the borrow checker complained, pass `&String` or `&Vec<T>`, or put a lifetime on a service struct | Ownership and Borrowing |
| Reach for `Arc<Mutex<T>>` before checking rungs 1-3 (no sharing, read-mostly, an owner task), or hold a lock across `.await` | Shared State |
| Write `unwrap()` or `expect()` outside a test | No Panics |
| Return `Box<dyn Error>` or `Result<T, String>` from production code, or log an error and also return it | Errors |
| Call `std::fs`, a sync client or a password hash inside `async fn`, put `#[async_trait]` on a trait not stored as `dyn`, or drop the `JoinHandle` of a task that does not log its own failure | Async Discipline |
| Use `String` for a fixed set, add a second `bool` parameter, or write a function taking two or more bare `Uuid` ids | Types Carry the Invariant |
| Write `as` between integer types, chain a fifth iterator adaptor, or add `_ =>` to a match over your own enum | Control Flow |
| Define a trait with exactly one implementation | Dependency Injection |
| Create `utils.rs`, a crate-wide prelude or `pub(super)`, put business logic in `main.rs`, or widen visibility for a test | Module Layout |
| Add `#[allow(...)]` to green the build, hand-write a builder or retry loop, or patch from a quoted line | Reuse and Suppressions |
| Write a comment that narrates the next line | Documentation |

## Edition 2021 to 2024

Compiler, not edition: 1.80 `std::sync::LazyLock` replaces `once_cell::sync::Lazy`, 1.81 `#[expect(lint, reason = "...")]` replaces `#[allow]`. The edition changes:

| 2021 | 2024 |
|---|---|
| An `if let` scrutinee temporary outlived `else`, deadlocking on a lock | It drops before `else`. Still bind guards to a name. |
| Return-position `impl Trait` needed `+ 'a` or a `Captures` helper | Lifetimes are captured automatically; `use<>` opts out. |
| `std::env::set_var` was safe | It is `unsafe`; tests take config as a value, not through the environment. |
| `gen` was an identifier | `gen` is reserved; write `r#gen`. |
