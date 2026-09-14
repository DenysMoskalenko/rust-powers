# Connecting to Postgres

## Cargo features

```toml
sea-orm = { version = "2", features = [
  "sqlx-postgres",          # driver; also turns on postgres-array
  "runtime-tokio-rustls",   # sea-orm's own feature name, kept in 2.0
  "macros",                 # derives, the model attribute macro, and raw_sql!
  "with-chrono", "with-uuid", "with-rust_decimal", "with-json",
] }
```

Every feature above except the driver is already on by default; list them anyway so a later
`default-features = false` does not silently remove something. Two defaults are easy to lose that
way: `stream`, which backs `.stream()`, and `with-time`, which pulls the `time` crate and is the one
default worth dropping deliberately. `with-bigdecimal` is no longer a default. The `mock` feature
gates `MockDatabase`.

## ConnectOptions

Build the pool once at startup and hold it in application state.

```rust,verify
use sea_orm::{ConnectOptions, Database, DatabaseConnection, DbErr};
use std::time::Duration;

use crate::config::DB_STATEMENT_TIMEOUT;

// `max_connections` is per process. Budget
// replicas * max_connections + migrations + admin <= the server's max_connections.
pub async fn connect(url: &str, max_connections: u32) -> Result<DatabaseConnection, DbErr> {
    let mut opt = ConnectOptions::new(url);
    opt.max_connections(max_connections)
        // Keeps a warm pair, so the first request after a scale-up skips TCP, TLS and auth.
        .min_connections(2)
        .connect_timeout(Duration::from_secs(5))
        .acquire_timeout(Duration::from_secs(5))
        .idle_timeout(Duration::from_mins(10))
        .max_lifetime(Duration::from_mins(30))
        // Server-side guard: Postgres cancels a runaway query even if the client never gives up.
        // The const is 10 s, strictly below axum-service's 30 s request timeout.
        .statement_timeout(DB_STATEMENT_TIMEOUT)
        // Off, as in the scaffold: on, every statement is an INFO line, seven per request.
        .sqlx_logging(false)
        .set_schema_search_path("public")
        .set_application_name("app");

    Database::connect(opt).await
}
```

`statement_timeout` is new in 2.0 and belongs in every baseline connect: without it a single bad
plan holds a pooled connection until the client times out, and the query keeps burning server CPU
afterwards.

Sizing: ten connections per process with four replicas is a reasonable start against the Postgres
default of 100. Raise the replica count before the per-pod pool, and put PgBouncer in front before
raising either. `statement_timeout` is a connection-level `SET`, which PgBouncer in transaction
pooling does not carry reliably; there, set it on the server or the role instead. `connect_lazy(true)`
lets a process start before the database is reachable, which matters when the orchestrator starts
the app and the database in the same batch.

sea-orm's own default is `sqlx_logging(true)`; the scaffold turns it off because with it on every
statement is an INFO event, `echo=True` for every request in production. To see SQL locally, flip
it in one place — `.sqlx_logging(cfg!(debug_assertions))`, or a `settings.database.log_statements`
boolean — and keep `RUST_LOG=info`. sqlx 0.9 emits native `tracing` events with target
`sqlx::query` (no `log` bridge), so a `tracing_subscriber::Layer` on that target can collect and
count them; `relations.md` does exactly that to prove a query count. Slow-query logging is
`sqlx_slow_statements_logging_settings(level, threshold)`, which takes a `log::LevelFilter` and
therefore needs the `log` crate as a direct dependency. The `tracing-spans` feature plus
`record_stmt_in_spans(true)` attaches the SQL to the surrounding span instead.

## The connection in application state

`DatabaseConnection` is a struct wrapping an `Arc`'d sqlx pool. Cloning it is a refcount bump, so
store it by value and clone it into each handler:

```rust,ignore
// Wrong: a second layer of reference counting that buys nothing.
pub struct AppState {
    pub db: Arc<DatabaseConnection>,
}

// Right.
#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
}
```

There is no session-per-request object and nothing to open or close per request. A web framework
extractor that produces a "session" is porting a SQLAlchemy idea that has no counterpart here:
checkout happens inside each `await`ed statement and returns to the pool immediately. Where a
request genuinely needs several statements to commit together, open a transaction explicitly.
For wiring state into the router see `axum-service`.

Keep the URL in a secret-carrying type and call its exposing method exactly once, at
`ConnectOptions::new`. The one in 2.0 that reads it back, `ConnectOptions::get_url`, returns the
password in clear text, so never log it.
