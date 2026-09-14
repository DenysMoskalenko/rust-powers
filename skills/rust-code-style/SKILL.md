---
name: rust-code-style
description: "Use when writing or reviewing Rust for an axum or tokio backend service in this stack — ownership and cloning, thiserror versus anyhow, no-panic rules, newtypes, derive_more and strum, async discipline, module layout, dependency injection. Also for E0502, E0499, future cannot be sent between threads safely, or a clippy lint such as needless_pass_by_value. Not for embedded, CLI-only tools, unsafe or FFI code, language tutorials, lint configuration, framework patterns, or tests."
metadata:
  version: "0.1.0"
---

# Rust Code Style

Assumes Rust 1.98 edition 2024, tokio 1.53, thiserror 2, anyhow 1, serde 1, bon 3, derive_more 2, strum 0.28, itertools 0.15.

## Important

- Never `.clone()` to silence the borrow checker; end the borrow with a smaller scope or ownership in the signature.
- `unwrap()` never appears outside tests; `expect("why it cannot fail")` only at startup or under a locally proven invariant.
- A `thiserror` enum per domain module with matched-on variants or more than one failure mode; `anyhow` for errors only logged; `Box<dyn Error>` and `Result<T, String>` are banned.
- Never hold a `std::sync` guard across `.await`, and never block the runtime: `spawn_blocking` for anything sync or CPU-bound.
- No trait with a single implementation; the only injected traits are for side effects a test cannot exercise (time, email, payments, an LLM).

*house* marks this project's preference, not a Rust-wide rule; *lint* marks one the `[lints]` table already enforces, so quote the lint rather than restate it.

## Ownership and Borrowing

Clone when you want a second owned value, never to silence a borrow error. The fix is nearly always a smaller scope, `&` instead of `&mut`, or taking ownership in the signature.

```rust
let all = items.clone();                            // Bad: clone to dodge the borrow checker
for i in &all { if i.is_stale() { items.push(i.renewed()); } }

let renewed: Vec<Item> = items.iter().filter(|i| i.is_stale()).map(Item::renewed).collect();
items.extend(renewed);                              // Good: the read ends before the write starts
```

Cheap: `Arc`, `Uuid`, `DatabaseConnection`, a `String` at an API boundary. Not: in a loop, on a `Vec` of domain objects, on `AppState`.

Take `&str` unless the function stores the value; then `String`, or `impl Into<String>` in a constructor. `&[T]` never `&Vec<T>`. A lifetime on a service struct is a smell: own the data or an `Arc`.

## Shared State

Take the first rung that works:

1. **No sharing.** `DatabaseConnection` is `Clone` and internally synchronised — hold it by value in `AppState`, never `Arc<Mutex<_>>`.
2. **Read-mostly.** `Arc<Settings>`, no lock.
3. **Mutable and shared.** Message passing first — `tokio::sync::mpsc` to one owner task. No poisoning, no ordering bugs.
4. **Only then a lock**, `std::sync` when the critical section is synchronous, `tokio::sync` when it must await.

Never hold a `std::sync` guard across `.await`: lock inside a non-async method, and settle check-then-insert races with `entry()`. A poisoned lock is recovered, not a crash: `lock().unwrap_or_else(PoisonError::into_inner)`, never `expect("poisoned")`.

## Errors

One `thiserror` enum per domain module when callers match on variants or the module has more than one failure mode; a single-variant wrapper is not worth an enum. `anyhow` for errors only logged. `Box<dyn Error>` and `Result<T, String>` are banned — both lose the source chain. Domain errors live in the module that raises them and reach `AppError` through `#[from]`; every variant names the concrete thing that failed, with ids, or is `#[error(transparent)]`. Add `anyhow` context once per meaningful operation, not at every `?`. Never log and return the same error. For status mapping see `axum-service`.

## No Panics in Production Paths

`unwrap()` never appears outside tests (*lint* `unwrap_used`). `expect("reason")` only at startup or under a locally proven invariant; its message says why it cannot fail — `expect("regex is a compile-time constant")`, not what happened. `todo!` and `unimplemented!` never reach a commit (*lint*). In the service a panic becomes a logged 500 via `CatchPanicLayer`; still never panic on purpose.

## Types Carry the Invariant

Encode rules in types so violating them fails to compile. A value from a fixed set is an enum, never a `String`. A named `bool` DTO field is fine; two or more `bool` parameters on a function are not — `list_users(true, false)` is unreadable and trivially swapped. Derive its wire form with `strum` and `serde(rename_all)`.

```rust
fn transfer(from: Uuid, to: Uuid, amount: Decimal);         // Bad: swapping arguments compiles
fn transfer(from: AccountId, to: AccountId, amount: Money);  // Good
```

Ids are bare `Uuid` by default — generated sea-orm entities, `Path<Uuid>` and the DTOs all use it, and a newtype per id costs `ToSchema`, `IntoParams` and `find_by_id` plumbing. Newtype an id when one signature takes two or more of them or the value has a validity rule; `struct AccountId(Uuid)` with `derive_more` is enough, `nutype` only when validation is intrinsic.

Derive order (*house*): `Debug, Clone, Copy, PartialEq, Eq, Hash, Default`, then third-party, with `Debug` on everything. `Default` only where the zero value is meaningful. Private fields plus a constructor keep invariants true. `From`/`TryFrom` for conversions, never `to_domain()`. `bon::Builder` at four or more fields, or two optional ones.

`derive_more` for newtype impls (`Display`, `From`, `Add`), never `Deref`, which fakes inheritance. `itertools` for `chunks`, `unique`, `izip!`, not to replace a two-line loop.

## Async Discipline

Never block the runtime: `tokio::fs` over `std::fs` (static data such as a template: `include_str!` or `LazyLock` once, never a read per request), `spawn_blocking` for password hashing, image work, any sync client, anything CPU-bound past 100 microseconds; such a task cannot be aborted, so never loop unboundedly in one.

Native `async fn` in traits, but a `pub` trait writes the method out as `fn send(&self, ...) -> impl Future<Output = T> + Send;`: rustc's `async_fn_in_trait` fires on the sugar there, because the returned future promises no `Send` bound. `#[async_trait]` only when the trait is stored as `dyn`. An `async fn` with no `.await` is sync (*lint* `unused_async`). Never drop a `JoinHandle` unless fire-and-forget is intended: the `JoinError` and the panic go with it. Before a `select!`, read `references/async-patterns.md`: a branch that is not cancel-safe loses data when another wins.

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

`foo.rs` beside `foo/` for modules you write by hand, never mixed with `mod.rs` at one level. `mod.rs` where a generator or a convention fixes it: `tests/common/` (cargo), generated `src/entities/` (sea-orm-cli), `src/api/` (*house*). No `utils.rs`, `helpers.rs` or `types.rs`, and no crate-wide prelude: a `prelude::*` makes every file's dependencies invisible. Imports in three blocks — std, external crates, then `crate` (*house*; rustfmt does not enforce it).

Three visibility levels only: private, `pub(crate)`, `pub`. No `pub(super)` or `pub(in path)` (*house*): moving the module silently changes the caller set. Never widen visibility for a test. A new workspace crate needs a second consumer.

## Documentation

`//!` for a module's purpose, `///` for non-obvious public items: one sentence, then `# Errors` and `# Panics` where they apply. No doc comments on private helpers, no comment narrating the next line — code says what, comments say why.

## Dependency Injection

Constructors take their collaborators; `AppState` is the composition root; no globals or service singletons.

Do not define a trait with one implementation. A mock of your own repository only proves the mock behaves; real Postgres and `httpmock` exercise the thing that breaks. One exception: a side effect tests cannot exercise — time, email, payments, an LLM — gets a small trait injected through `AppState`, as `Clock` already is. Never the database, never HTTP.

## Reuse, Suppressions and Project Style

Check std, then the crates already in the lock file, before hand-writing a retry loop, a builder or a `Display` impl; add a dependency at its latest version, never a guessed one. Read the enclosing module and its callers before editing; extend a near-duplicate rather than adding another.

Fix the finding rather than silencing it. A justified suppression is `#[expect(lint, reason = "...")]`, which warns once the lint stops firing (*lint* `allow_attributes_without_reason`). Never `#![deny(warnings)]`. Priority when rules collide: KISS, YAGNI, single responsibility, DRY after the third repetition, fail fast.

## Valid Patterns — Do Not Flag

- `expect` at startup whose message states why failure is impossible, and `unwrap` under `cfg(test)` or in `tests/`.
- A `match` with one arm per variant and no `_` arm — exhaustiveness is the point.
- A module named after its type (`user::User`).
- A bare `Uuid` id in an entity, a `Path<Uuid>`, a DTO, or a function taking one id.

## Red Flags — STOP

| About to… | Rule to apply |
|---|---|
| Add `.clone()` because the borrow checker complained, pass `&String` or `&Vec<T>`, or put a lifetime on a service struct | Ownership and Borrowing |
| Write `Arc<Mutex<T>>` for new state, or hold a lock across `.await` | Shared State |
| Write `unwrap()` outside a test, or `expect()` with no why-it-cannot-fail message | No Panics |
| Return `Box<dyn Error>` or `Result<T, String>`, or log an error and also return it | Errors |
| Call `std::fs`, a sync client or a password hash inside `async fn`, reach for `#[async_trait]`, or drop a `JoinHandle` | Async Discipline |
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

## References

- `references/error-handling.md` — adding or changing an error type: thiserror versus anyhow, domain enums, the `#[from]` / `#[source]` / `#[error(transparent)]` table.
- `references/async-patterns.md` — before a `select!`, background work, or an async trait: cancellation safety, structured concurrency, shutdown.
- `references/diagnostics.md` — a pasted rustc error or clippy lint name: what it means, the fix, why the lint is on.
