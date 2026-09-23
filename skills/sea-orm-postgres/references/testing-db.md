# Per-test database isolation

- [Why a real Postgres](#why-a-real-postgres)
- [The nextest process model](#the-nextest-process-model)
- [Dev-dependencies](#dev-dependencies)
- [The harness](#the-harness)
- [Using it in a test](#using-it-in-a-test)
- [Template databases](#template-databases)
- [Cleanup and recovery](#cleanup-and-recovery)
- [MockDatabase](#mockdatabase)

## Why a real Postgres

Tests run against a real Postgres, not SQLite and not a mock. Unique indexes, foreign keys, check
constraints, `ON CONFLICT`, `jsonb` operators and `timestamptz` semantics are the behaviour under
test, and none of them exist in a substitute. A suite that passes against a fake and fails against
the real server has tested nothing.

Isolation comes from giving every test its own database, not from wrapping tests in a transaction.
Wrapping each test in an outer transaction does not work: sea-orm has no "join the enclosing transaction" mode,
a service function that opens its own transaction escapes the outer one, and a
`DatabaseTransaction` cannot be handed to code that wants a `DatabaseConnection`. Truncating
between tests needs a shared connection and serialises the suite.

## The nextest process model

cargo-nextest runs each test in its own process. A `static` or `OnceCell` holding a container
therefore initialises once per *test*, not once per binary — that is one Postgres container per
test, minutes of startup, and containers left running afterwards. `tokio::sync::OnceCell` does not
help; the boundary is the process, not the task.

Two arrangements survive that, and the harness below supports both:

1. **An external server — the default.** `TEST_DATABASE_URL` points at the compose `postgres`
   service locally (`docker compose up -d postgres` first; `make test` runs only nextest) and at a
   `services:` container in CI. Nothing to start, nothing to clean up, no race, fastest everywhere.
   Do not rely on container reuse in CI.
2. **A reusable container — a warm-machine convenience.** The fixed name
   `rust-powers-test-postgres` plus `ReuseDirective::Always` means every process attaches to the
   same container. On a cold machine the processes race to *create* it: one wins and Docker answers
   the others with `409 Conflict … name is already in use`, so `start()` is retried on that error
   only; a second `start()` takes the reuse branch and attaches. Reuse never re-runs the wait
   strategy, so a process that attaches while Postgres is still booting retries its first
   connection, and a container whose Postgres has died is attached to anyway — see
   [Cleanup and recovery](#cleanup-and-recovery).

## Dev-dependencies

`testcontainers-modules` 0.15 does not re-export the reuse feature, so `ReuseDirective` and
`with_reuse` come from `testcontainers` pinned directly. The stack pins all three modules in one
line, so adding Redis or NATS tests later changes nothing here:

```toml
[dev-dependencies]
testcontainers-modules = { version = "0.15", features = ["postgres", "redis", "nats"] }
testcontainers = { version = "0.27", features = ["reusable-containers"] }   # modules does not re-export it
```

The module's Postgres image defaults to the `11-alpine` tag, so every container sets `with_tag`
to the version production runs.

## The harness

This is the database half of `tests/common/mod.rs` — the file `rust-testing`'s `test_app()` lives
in, helper for helper as the scaffold ships it (`admin_url`, `start_reusable_postgres`,
`is_name_conflict`, `connect_with_retry`). `test_app()` runs exactly `fresh_database()` below and
then builds `AppState` around the connection; it is split out here so the database part can be
read on its own. `tests/common/mod.rs`, not `tests/common.rs`: a file directly in `tests/` is
compiled as its own test binary with no tests in it.

```rust,verify,test
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "allow-unwrap-in-tests covers test bodies, not helpers"
)]

use std::sync::LazyLock;

use migration::{Migrator, MigratorTrait as _};
use sea_orm::{ConnectionTrait as _, Database, DatabaseConnection, EntityTrait, PaginatorTrait, Statement};
use testcontainers::runners::AsyncRunner as _;
use testcontainers::{ContainerAsync, ImageExt as _, ReuseDirective};
use testcontainers_modules::postgres::Postgres;
use tokio::sync::OnceCell;
use uuid::Uuid;

// Only the URL is kept: dropping a `ContainerAsync` marked for reuse leaves the container running.
static ADMIN_URL: LazyLock<OnceCell<String>> = LazyLock::new(OnceCell::new);

/// A connection string for a server that can `CREATE DATABASE`. Under nextest this
/// `OnceCell` is per test process, not per binary, which is why the fallback container
/// is named and reused rather than started per test.
async fn admin_url() -> &'static str {
    ADMIN_URL
        .get_or_init(|| async {
            if let Ok(url) = std::env::var("TEST_DATABASE_URL") {
                return url;
            }
            let container = start_reusable_postgres().await;
            format!(
                "postgres://postgres:postgres@{}:{}/postgres",
                container.get_host().await.unwrap(),
                container.get_host_port_ipv4(5432).await.unwrap(),
            )
        })
        .await
}

/// On a cold run every test process tries to create the container; Docker lets one
/// win and answers the rest with a 409 name conflict. The next attempt attaches to
/// the container that now exists. Any other error is real and surfaces at once.
async fn start_reusable_postgres() -> ContainerAsync<Postgres> {
    let mut last = None;
    for _ in 0..10 {
        let attempt = Postgres::default()
            // The module default is 11-alpine.
            .with_tag("18-alpine")
            .with_container_name("rust-powers-test-postgres")
            .with_reuse(ReuseDirective::Always)
            .start()
            .await;
        match attempt {
            Ok(container) => return container,
            Err(err) if is_name_conflict(&err) => {
                last = Some(err);
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
            Err(err) => panic!("could not start the Postgres container: {err:?}"),
        }
    }
    panic!("container name still in conflict after 10 attempts: {last:?}");
}

fn is_name_conflict(err: &testcontainers::TestcontainersError) -> bool {
    let text = format!("{err:?}");
    text.contains("409") || text.contains("already in use") || text.contains("Conflict")
}

/// Only the process that created the container waited for Postgres to accept
/// connections; every other process attaches to one that may still be booting.
async fn connect_with_retry(url: &str) -> DatabaseConnection {
    let mut last = None;
    for _ in 0..40 {
        match Database::connect(url).await {
            Ok(db) => return db,
            Err(err) => {
                last = Some(err);
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        }
    }
    panic!("could not connect to {url}: {last:?}");
}

/// A migrated, empty database of its own for this test, plus its name for failure
/// messages. This is the first half of `test_app()`.
pub async fn fresh_database() -> (DatabaseConnection, String) {
    let admin_url = admin_url().await;
    let admin = connect_with_retry(admin_url).await;
    // uuid v7 keeps the generated names sortable, which helps when reading a stuck container.
    let db_name = format!("test_{}", Uuid::now_v7().simple());
    admin
        .execute_raw(Statement::from_string(
            admin.get_database_backend(),
            format!(r#"CREATE DATABASE "{db_name}""#),
        ))
        .await
        .unwrap();
    admin.close().await.unwrap();

    // NOT `admin_url.replace("/postgres", ..)`: that would also rewrite the `postgres://` scheme.
    let (prefix, _) = admin_url.rsplit_once('/').unwrap();
    let db = connect_with_retry(&format!("{prefix}/{db_name}")).await;
    // Running the real migrations is also how the migrations get tested.
    Migrator::up(&db, None).await.unwrap();
    (db, db_name)
}

#[tokio::test]
async fn each_test_gets_an_empty_database() {
    let (db, _name) = fresh_database().await;
    let count = app::entities::user::Entity::find().count(&db).await.unwrap();
    assert_eq!(count, 0);
}
```

The identifier is interpolated into the `CREATE DATABASE` string because Postgres does not accept a
bind parameter there. It is a locally generated uuid, never anything from a test fixture or the
environment, which is what makes that safe.

Do not call `std::env::set_var` to point a test at a different database. It is `unsafe` in edition
2024 and the crate lints forbid unsafe code outright, so the attempt ends in a wall rather than a
warning. Read the environment; construct configuration in memory.

## Using it in a test

Day to day, a test calls `common::test_app()` — `rust-testing`'s shared helper — and reaches the
fresh database as `app.state.db`, next to the frozen clock and the mock upstream the rest of
`AppState` carries.

```rust,verify,test
mod common;

use app::entities::user;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait, PaginatorTrait};
use uuid::Uuid;

#[tokio::test]
async fn rejects_a_duplicate_email() {
    let app = common::test_app().await;
    let db = &app.state.db;

    let row = || user::ActiveModel {
        id: Set(Uuid::now_v7()),
        email: Set("alice@example.com".to_owned()),
        name: Set("Alice".to_owned()),
        // Generated entities type timestamptz as `DateTimeWithTimeZone`
        // (`DateTime<FixedOffset>`), so a `Utc` value converts with `.into()`.
        created_at: Set(chrono::Utc::now().into()),
    };

    row().insert(db).await.expect("first insert");
    let err = row().insert(db).await.expect_err("second insert must fail");

    // The assertion is on the mapped condition, not on the message text, which is
    // a Postgres string and not API.
    assert!(matches!(
        err.sql_err(),
        Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
    ));
    assert_eq!(user::Entity::find().count(db).await.unwrap(), 1);
}
```

Because each test owns its database, tests can insert freely, run in parallel, and never need to
clean up after themselves. Test structure, fixtures and data factories belong to `rust-testing`.

## Template databases

`CREATE DATABASE` plus `Migrator::up` costs about 0.17 s per test against a warm server. Under
nextest a process runs one test, so a template built inside the test process never pays for itself:
it is an extra `CREATE DATABASE` and a migration run on top of the clone. A template only helps when
a single test's migration time hurts, and then it is prepared *before* the suite, once, with a fixed
name on a server that outlives the run — a `make test` step, not test code:

```sh
psql "$TEST_DATABASE_URL" -c 'CREATE DATABASE tmpl_app' 2>/dev/null || true   # exists after the first run
sea-orm-cli migrate up -u "${TEST_DATABASE_URL%/*}/tmpl_app"                  # no-op when nothing is pending
```

Nothing is connected to the template while tests run and no process races another to build it, so
the per-test step shrinks to a file copy of roughly ten milliseconds, in place of the
`CREATE DATABASE` plus `Migrator::up` pair in `test_app()`:

```rust,verify
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbErr};
use uuid::Uuid;

// `CREATE DATABASE .. TEMPLATE` refuses a template that anything is connected to, which is the
// other reason the template is built outside the test processes.
pub async fn clone_from_template(
    admin: &DatabaseConnection,
    prefix: &str,
) -> Result<DatabaseConnection, DbErr> {
    let db_name = format!("test_{}", Uuid::now_v7().simple());
    admin
        .execute_unprepared(&format!(
            r#"CREATE DATABASE "{db_name}" TEMPLATE "tmpl_app""#
        ))
        .await?;
    Database::connect(format!("{prefix}/{db_name}")).await
}
```

## Cleanup and recovery

Dropped databases are not the interesting cost; a reused container accumulates `test_*` databases
until it is removed. `docker rm -f rust-powers-test-postgres` resets everything, and a CI job that starts a
service container per run never accumulates anything. If a local run leaves hundreds behind, drop
them with a single `DROP DATABASE` loop over `pg_database` rather than adding teardown to every test,
which would run before an assertion failure has been read.

If every database test suddenly fails to connect, the reused container is wedged — its Postgres has
exited or its data directory is corrupt — and reuse attaches to it without checking. A reused
container is never health-checked again. `docker rm -f rust-powers-test-postgres` and re-run.

## MockDatabase

`sea_orm::MockDatabase`, behind the `mock` feature, pre-loads query results and asserts generated
statements. It parses no SQL and enforces no constraint, so it accepts every query Postgres would
reject, and it is not used in this stack: a fresh database costs 0.17 s, less than reasoning about
what the mock does not check. Logic that has no query in it is tested as a plain function with no
database at all (`rust-testing`).
