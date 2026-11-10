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

The blocking pool is sized for IO: it grows to 512 threads by default and only then queues. CPU-heavy work (password hashing, image or PDF work) therefore takes a permit from one process-wide `Semaphore`, sized to the cores, before it is spawned; without the bound a burst of logins hashes on hundreds of threads at once and starves every other request of CPU and memory. A blocking IO call needs no permit.

```rust,verify
use std::sync::Arc;

use argon2_stub::hash_password;
use tokio::sync::Semaphore;

mod argon2_stub {
    pub fn hash_password(password: &str) -> String {
        password.to_owned()
    }
}

/// One semaphore for the whole process, built at startup and kept in the
/// application state.
pub fn cpu_permits() -> Arc<Semaphore> {
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    Arc::new(Semaphore::new(cores))
}

/// `spawn_blocking` returns `Result<T, JoinError>`, so a fallible closure gives
/// two layers to unwrap — here the closure is infallible and there is only one.
pub async fn hash(cpu: Arc<Semaphore>, password: String) -> anyhow::Result<String> {
    let permit = cpu.acquire_owned().await?;
    let hash = tokio::task::spawn_blocking(move || {
        let hash = hash_password(&password);
        // Released by the thread, not by the caller: the hash keeps running
        // after a cancelled request, and the bound has to count it until it ends.
        drop(permit);
        hash
    })
    .await?;
    Ok(hash)
}
```

A `spawn_blocking` task cannot be aborted once it starts, and runtime shutdown waits for it, so never put an unbounded loop inside one.

## Cancellation safety

A future is cancel-safe if dropping it part-way and recreating it loses nothing. Three things drop a future part-way: `select!` drops every losing branch, `timeout` drops its future at the deadline, and axum drops a handler's future when the client disconnects or the request timeout fires. `AsyncReadExt::read` is cancel-safe; a hand-written `read_exact_message` that has already consumed half a frame is not, and the half is gone.

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

A branch has two parts and only one of them can be cut short. The future before `=>` is dropped at whatever `.await` it had reached when another branch completes first; the handler after `=>` runs only for the winning branch, once the others are gone, so `select!` never cuts it short. A mutation in two steps therefore belongs in the handler, or in a spawned task, never in the raced future, where the second step can be lost. Inside a request handler use the spawned task or one database transaction: the handler's own future can be dropped at any `.await`.

## Structured concurrency

| Need | Use |
|---|---|
| A fixed set of operations, all must finish | `tokio::try_join!` |
| A dynamic number of tasks, results as they finish | `JoinSet` — dropping it aborts the rest |
| A deadline on one operation | `tokio::time::timeout`, before reaching for `select!` |
| Broadcast "stop" to many tasks | a `tokio::sync::watch` channel |
| Wait for tasks to finish shutting down | `JoinSet::join_next` in a loop, logging each `JoinError`; `join_all` panics when any task panicked or was aborted |

Never `tokio::spawn` and drop the `JoinHandle` unless fire-and-forget is genuinely intended. The runtime catches a panic inside a task: the process carries on, the panic hook prints to stderr, outside the tracing subscriber, whether the handle is kept or not, and the `JoinError` is the only thing code can act on. Dropping the handle discards it, so nothing logs the failure through tracing and nothing restarts the task. Keep the handle, or own it in a `JoinSet`, and log the `JoinError` where it is joined.

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
        // Not `join_all`: it panics again here if a worker panicked, and the
        // flush that follows would never run.
        while let Some(joined) = self.tasks.join_next().await {
            if let Err(error) = joined {
                tracing::error!(%error, "background worker failed");
            }
        }
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
