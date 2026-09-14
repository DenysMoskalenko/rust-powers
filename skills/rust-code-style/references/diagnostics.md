# Diagnostics

What a pasted rustc error or clippy lint name means, and the fix. Configuration of the lint table belongs to `rust-tooling`; this file is about the code in front of you.

## Contents

- [Borrow errors](#borrow-errors) — E0502, E0499, E0505
- [Future cannot be sent between threads safely](#future-cannot-be-sent-between-threads-safely)
- [Clippy lints by name](#clippy-lints-by-name)
- [Why these lints are on](#why-these-lints-are-on)
- [Gotchas](#gotchas)

## Borrow errors

All three have the same shape: two accesses to one value overlap in time. The fix is to make one of them end sooner, never to add `.clone()` — a clone that only exists to end a borrow is a permanent allocation paying for a temporary scope problem.

**E0502, cannot borrow as mutable because it is also borrowed as immutable.** A read borrow is still alive where the write starts. End the read first: bind what you need into a local, or close the block.

```rust,ignore
// Bad: the iterator borrows `items` for the whole loop, so the push cannot compile.
for item in items.iter() {
    if item.is_stale() {
        items.push(item.renewed());
    }
}
```

```rust,verify
#[derive(Clone)]
struct Item {
    sku: String,
    stale: bool,
}

impl Item {
    fn is_stale(&self) -> bool {
        self.stale
    }
    fn renewed(&self) -> Self {
        Self { sku: self.sku.clone(), stale: false }
    }
}

// Good: the read finishes, then the write starts.
fn renew_stale(items: &mut Vec<Item>) {
    let renewed: Vec<Item> = items.iter().filter(|i| i.is_stale()).map(Item::renewed).collect();
    items.extend(renewed);
}
```

**E0499, cannot borrow as mutable more than once.** Two `&mut` to one value overlap. Use `split_at_mut` for disjoint slice halves, look up a key and finish with it before the next lookup, or restructure so a single owner mutates. For a map, `entry()` is one lookup that both reads and writes.

**E0505, cannot move out of a borrowed value.** A borrow outlives a move. Drop the borrowing binding, or close its block, before the move — often the borrow only needs to be a field read copied into a local.

## Future cannot be sent between threads safely

A `!Send` value is alive across an `.await` inside something that must be `Send` — nearly always a `tokio::spawn` or an axum handler. The culprit is an `Rc`, a `RefCell` borrow, a `std::sync::MutexGuard`, or a trait object without a `Send` bound. The compiler's note names the value and the await point; read it, do not guess.

Fixes, in order of preference: compute the value and let the guard drop before the await (an explicit block is the clearest way); swap `Rc` for `Arc` and `RefCell` for `tokio::sync::Mutex`; add `+ Send` to a returned `impl Future` or trait object. `spawn_local` is a last resort: it needs a `LocalSet` around it, and it pins the task to one thread.

```rust,ignore
// Bad: the guard is still alive at the await point, so the future is !Send.
let mut cache = state.cache.lock().unwrap_or_else(PoisonError::into_inner);
let value = fetch(&key).await?;
cache.insert(key, value);
```

```rust,verify
use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

struct State {
    cache: Mutex<HashMap<String, String>>,
}

async fn fetch(key: &str) -> std::io::Result<String> {
    Ok(key.to_uppercase())
}

// Good: fetch first, then take the lock for a synchronous critical section.
// A poisoned lock is recovered with `into_inner`, which passes `expect_used`;
// `expect("cache poisoned")` does not.
async fn refresh(state: &State, key: String) -> std::io::Result<()> {
    let value = fetch(&key).await?;
    state
        .cache
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key, value);
    Ok(())
}
```

## Clippy lints by name

| Lint | Cause and fix |
|---|---|
| `needless_pass_by_value` | The parameter is `String` or `Vec<T>` but only read. Take `&str` or `&[T]`. |
| `ptr_arg` | `&String` or `&Vec<T>` in a signature. Take `&str` or `&[T]` — callers deref-coerce for free. |
| `await_holding_lock` | A `std::sync` guard is live across `.await`. Take the lock inside a non-async helper so the guard cannot escape. |
| `redundant_clone` | The cloned value is never used again. Drop the clone. |
| `unwrap_used`, `expect_used` | Outside a test. Return an error, or give `expect` a message stating why it cannot fail. |
| `manual_let_else` | A `match` or `if let` that only guards. Rewrite as `let ... else { return ... };`. |
| `cast_possible_truncation`, `cast_sign_loss` | An `as` cast between integer types. Use `TryFrom`, or `#[expect(..., reason = "...")]` if the cast is right. |
| `too_many_arguments` | Past five parameters. Pass a struct; derive `bon::Builder` at four or more fields. |
| `type_complexity` | Nested generics in a signature. Name it with a type alias or a struct. |
| `unused_async` | An `async fn` with no `.await`. Make it sync; callers lose nothing. |
| `large_futures` | A big future is memcpy'd into `tokio::spawn`. `Box::pin` it. |
| `wildcard_imports` | A glob import hides where names come from. Import the items. |
| `cognitive_complexity` | Branching has outgrown the function. Extract the sub-steps. |

## Why these lints are on

| Lint | Why |
|---|---|
| `unwrap_used` | In the service a panic becomes a logged 500 via `CatchPanicLayer`; still never panic on purpose in a request path — the layer is a net for bugs, not an error path. |
| `expect_used` | Every survivor must state why it cannot fail; tests are exempt. |
| `allow_attributes_without_reason` | Forces `#[expect(..., reason)]`, so a suppression justifies itself and goes stale loudly. |
| `redundant_clone` | Catches the mechanical half of cloning past the borrow checker. |
| `cognitive_complexity` | An alarm that a function wants splitting, not a measurement. |
| `cast_possible_truncation` | `as` truncates silently; ids and money stop matching. |
| `panic`, `todo`, `unimplemented` | A placeholder that reaches a commit ships as a crash. |
| `print_stdout` | Anything outside the tracing subscriber is invisible in production. |

## Gotchas

- `await_holding_lock` only catches bound guards, so a clean clippy run is not proof the rule held.
- `allow-unwrap-in-tests` covers only the body of a `#[test]` or `#[tokio::test]` function. A helper in the same file, `tests/common/mod.rs` included, still warns. Such a file needs `#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test helpers")]`; a bare `#![allow(..)]` with no reason trips `allow_attributes_without_reason`.
- An unknown lint name is only a warning, so a typo in the lint table silently does nothing unless the build denies warnings.
- Fixing a lint by deleting the code it complains about is usually right. Fixing it with an attribute is only right when the reason string would convince a reviewer.
