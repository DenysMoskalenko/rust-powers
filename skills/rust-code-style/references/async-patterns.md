# Async Patterns

Rules for tokio code in a service: what blocks the runtime, what `select!` throws away, how background work shuts down, and when a trait needs a macro.

## Contents

- [Never block the runtime](#never-block-the-runtime)
- [Cancellation safety](#cancellation-safety)
- [Structured concurrency](#structured-concurrency)
- [Shutting background work down](#shutting-background-work-down)
- [Async functions in traits](#async-functions-in-traits)

## Never block the runtime

A blocking call inside `async fn` occupies a worker thread and starves every other task scheduled on it. No lint catches most of these, so the rule has to be held by hand.

Triggers for `spawn_blocking`: password hashing, image or PDF work, `std::fs`, any synchronous database or HTTP client, and anything CPU-bound past roughly 100 microseconds.

```rust,verify
use argon2_stub::hash_password;

mod argon2_stub {
    pub fn hash_password(password: &str) -> String {
        password.to_owned()
    }
}

/// `spawn_blocking` returns `Result<T, JoinError>`, so a fallible closure gives
/// two layers to unwrap — here the closure is infallible and there is only one.
pub async fn register(password: String) -> Result<String, tokio::task::JoinError> {
    tokio::task::spawn_blocking(move || hash_password(&password)).await
}
```

A `spawn_blocking` task cannot be aborted once it starts, and runtime shutdown waits for it, so never put an unbounded loop inside one.

## Cancellation safety

A future is cancel-safe if dropping it part-way and recreating it loses nothing. This only matters inside `select!`, because `select!` drops every losing branch. `AsyncReadExt::read` is cancel-safe; a hand-written `read_exact_message` that has already consumed half a frame is not, and the half is gone.

Check the "Cancel safety" note in the tokio docs for every method you put in a `select!` branch. If a branch is not cancel-safe, spawn it and select on the handle instead, so the work survives the race.

```rust,verify
use std::time::Duration;
use tokio::time::timeout;

async fn read_message() -> Vec<u8> {
    tokio::task::yield_now().await;
    Vec::new()
}

/// `read_message` is not cancel-safe, so it runs in its own task; only the
/// join handle is raced, and the task is aborted explicitly on timeout.
pub async fn read_with_deadline() -> Option<Vec<u8>> {
    let task = tokio::spawn(read_message());
    let aborter = task.abort_handle();
    let Ok(joined) = timeout(Duration::from_secs(5), task).await else {
        aborter.abort();
        return None;
    };
    joined.ok()
}
```

Never mutate shared state half-way through a `select!` branch: the branch can be dropped between the two halves of the mutation.

## Structured concurrency

| Need | Use |
|---|---|
| A fixed set of operations, all must finish | `tokio::try_join!` |
| A dynamic number of tasks, results as they finish | `JoinSet` — dropping it aborts the rest |
| A deadline on one operation | `tokio::time::timeout`, before reaching for `select!` |
| Broadcast "stop" to many tasks | a `tokio::sync::watch` channel |
| Wait for tasks to finish shutting down | `JoinSet::join_all` |

Never `tokio::spawn` and drop the `JoinHandle` unless fire-and-forget is genuinely intended: the `JoinError` is discarded, so nothing can learn the task failed, and the panic lands on stderr outside the tracing subscriber. Keep the handle, or own it in a `JoinSet`.

## Shutting background work down

`axum::serve(..).with_graceful_shutdown(..)` drains the HTTP listener only. Anything spawned outside a request needs its own signal, or the process exits mid-write.

```rust,verify
use std::time::Duration;
use tokio::{sync::watch, task::JoinSet};

async fn run_sweep() {
    tokio::task::yield_now().await;
}

/// Workers hold a shutdown receiver and are awaited before the process exits.
pub struct Workers {
    shutdown: watch::Sender<bool>,
    tasks: JoinSet<()>,
}

impl Workers {
    pub fn start() -> Self {
        let (shutdown, mut stopping) = watch::channel(false);
        let mut tasks = JoinSet::new();
        tasks.spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(60));
            loop {
                tokio::select! {
                    _ = stopping.changed() => break,
                    _ = ticker.tick() => run_sweep().await,
                }
            }
        });
        Self { shutdown, tasks }
    }

    /// Signals every worker, then waits for them. Call it after the server has
    /// drained and before the telemetry exporter is flushed.
    pub async fn shutdown(mut self) {
        self.shutdown.send_replace(true);
        self.tasks.join_all().await;
    }
}
```

If the project already depends on `tokio-util`, its `CancellationToken` plus `TaskTracker` is the same pattern with child-token propagation. Do not add the dependency for a single worker.

## Async functions in traits

Native `async fn` in a trait is the default: static dispatch, no macro, no boxing. A trait defined this way is not dyn-compatible, which is the entire trade.

```rust,verify
use std::future::Future;

/// Injected because sending mail is a side effect a test cannot exercise.
pub trait Mailer: Send + Sync + 'static {
    /// Written as `impl Future + Send` rather than `async fn` because callers
    /// spawn it, and a bare `async fn` in a trait promises nothing about `Send`.
    fn send(&self, to: &str, body: &str) -> impl Future<Output = anyhow::Result<()>> + Send;
}

pub struct SmtpMailer;

impl Mailer for SmtpMailer {
    async fn send(&self, _to: &str, _body: &str) -> anyhow::Result<()> {
        tokio::task::yield_now().await;
        Ok(())
    }
}
```

Injecting one of these through the application state means `Arc<dyn Mailer>`, which this definition forbids: a trait returning `impl Future` is not dyn-compatible. That is the case the `#[async_trait]` exception is for.

Reach for `#[async_trait]` only when the trait really must be stored as `dyn Trait` — for example when one state field holds one of several implementations chosen at startup. Before that, check whether making the caller generic over the trait removes the need entirely; it usually does, and it keeps the allocation and the `Box::pin` out of the hot path.

Two more rules worth stating: an `async fn` with no `.await` is a lie and should be sync, and a large future moved into `tokio::spawn` is memcpy'd, so `Box::pin` anything clippy flags as a large future.
