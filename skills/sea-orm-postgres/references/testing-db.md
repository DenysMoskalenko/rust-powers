# Per-test database isolation

- [Why a real Postgres](#why-a-real-postgres)
- [Why not a rolled-back transaction](#why-not-a-rolled-back-transaction)
- [The nextest process model](#the-nextest-process-model)
- [Dev-dependencies](#dev-dependencies)
- [The harness](#the-harness)
- [Using it in a test](#using-it-in-a-test)
- [One template per run](#one-template-per-run)
- [Cleanup and recovery](#cleanup-and-recovery)
- [MockDatabase](#mockdatabase)

## Why a real Postgres

Tests run against a real Postgres, not SQLite and not a mock. Unique indexes, foreign keys, check
constraints, `ON CONFLICT`, `jsonb` operators and `timestamptz` semantics are the behaviour under
test, and none of them exist in a substitute. A suite that passes against a fake and fails against
the real server has tested nothing.

Isolation comes from giving every test its own database, not from wrapping tests in a
transaction, and truncating between tests needs a shared connection and serialises the suite.

## Why not a rolled-back transaction

Running each test inside one transaction and rolling it back at the end does not fit sea-orm 2.0
and nextest:

- sea-orm has no mode that joins an enclosing transaction. `AppState.db` is a pool, every handler
  checks out a connection of its own, and what it commits escapes the test's transaction.
  `DatabaseExecutor` (new in 2.0) borrows or owns one transaction and is not `Clone`, so it cannot
  sit in `AppState`.
- A transaction the code under test opens itself becomes a `SAVEPOINT`, and Postgres then rejects
  `begin_with_config(Some(level), ..)`: `SET TRANSACTION ISOLATION LEVEL must be called before any
  query` (25001), unless the outer transaction already runs at that level.
- One failed statement aborts the whole outer transaction. Code that catches a 23505 and retries
  gets `current transaction is aborted` (25P02) under test and a 500, where production recovers.
- Parallel tests share one database, so an uncommitted insert of a unique key blocks every other
  test inserting that key until the first test ends.
- `now()` stands still for the whole test, a second connection cannot see the test's rows, and a
  test of `FOR UPDATE` or an advisory lock needs sessions that one connection cannot give.

## The nextest process model

cargo-nextest runs each test in its own process. A `static` or `OnceCell` holding a container
therefore initialises once per *test*, not once per binary — that is one Postgres container per
test, minutes of startup, and containers left running afterwards. `tokio::sync::OnceCell` does not
help; the boundary is the process, not the task.

Two arrangements survive that, and the harness below supports both:

1. **An external server — the default.** `TEST_DATABASE_URL` points at the compose `postgres`
   service locally (`docker compose up -d postgres` first; `make test` does not start it) and at a
   `services:` container in CI. Nothing to start, no race, fastest everywhere. Do not rely on
   container reuse in CI.
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

This is the database half of `tests/api/common.rs` — the file `rust-testing`'s `test_app()` lives
in, helper for helper as the scaffold ships it. `test_app()` calls `migrated_database()` and then
builds `AppState` around the connection; it is split out here so the database part can be read on
its own.

```rust,verify,test
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "allow-unwrap-in-tests covers test bodies, not helpers"
)]

use std::sync::LazyLock;

use migration::{Migrator, MigratorTrait as _};
use sea_orm::{ConnectionTrait as _, Database, DatabaseConnection, EntityTrait, PaginatorTrait, raw_sql};
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
            // The Makefile's TEST_PG: `make test` drops test databases by this name.
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

/// `TEST_RUN` under `make test` and `make cov`, nextest's own run id under any
/// other nextest run (bacon, CI, a direct `cargo nextest run`), and one id per
/// test binary under `cargo test`.
fn run_id() -> &'static str {
    static RUN: LazyLock<String> = LazyLock::new(|| {
        let id = ["TEST_RUN", "NEXTEST_RUN_ID"]
            .into_iter()
            .find_map(|var| std::env::var(var).ok().filter(|id| !id.is_empty()))
            .unwrap_or_else(|| Uuid::now_v7().simple().to_string());
        // 20 characters keep every database name inside Postgres's 63 bytes.
        id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(20).collect()
    });
    &RUN
}

/// `test_<run>_<uuid>`: the run leads the name, so `make test` can drop the
/// databases of a run that passed.
async fn create_database(admin: &DatabaseConnection, template: Option<&str>) -> String {
    let db_name = format!("test_{}_{}", run_id(), Uuid::now_v7().simple());
    let template = template.map(|t| format!(r#" TEMPLATE "{t}""#)).unwrap_or_default();
    admin
        .execute_unprepared(&format!(r#"CREATE DATABASE "{db_name}"{template}"#))
        .await
        .unwrap();
    db_name
}

async fn database_exists(admin: &DatabaseConnection, name: &str) -> bool {
    admin
        .query_one_raw(raw_sql!(Postgres, "SELECT FROM pg_database WHERE datname = {name}"))
        .await
        .unwrap()
        .is_some()
}

/// The run's first test processes each migrate a candidate and rename it into
/// place; the first rename wins and the others drop theirs.
async fn build_template(admin: &DatabaseConnection, admin_url: &str, template: &str) {
    let candidate = create_database(admin, None).await;
    let db = connect_with_retry(&database_url(admin_url, &candidate)).await;
    Migrator::up(&db, None).await.unwrap();
    // `CREATE DATABASE .. TEMPLATE` refuses a template anyone is connected to.
    db.close().await.unwrap();
    let renamed = admin
        .execute_unprepared(&format!(r#"ALTER DATABASE "{candidate}" RENAME TO "{template}""#))
        .await;
    // Racing renames can fail with 23505 instead of "already exists": ask, never match.
    if let Err(err) = renamed {
        assert!(
            database_exists(admin, template).await,
            "could not rename {candidate} to {template}: {err}"
        );
        admin
            .execute_unprepared(&format!(r#"DROP DATABASE "{candidate}""#))
            .await
            .unwrap();
    }
}

/// A migrated, empty database of this test's own, and its name. Every test
/// clones the run's template instead of running every migration.
pub async fn migrated_database(admin_url: &str) -> (DatabaseConnection, String) {
    let admin = connect_with_retry(admin_url).await;
    // Keyed by package too: a workspace's crates share one run, not one schema.
    let template = format!("test_{}_{:.20}_template", run_id(), env!("CARGO_PKG_NAME"));
    if !database_exists(&admin, &template).await {
        build_template(&admin, admin_url, &template).await;
    }
    let db_name = create_database(&admin, Some(&template)).await;
    admin.close().await.unwrap();
    (connect_with_retry(&database_url(admin_url, &db_name)).await, db_name)
}

/// NOT `admin_url.replace("/postgres", ..)`: that would also rewrite the `postgres://` scheme.
fn database_url(admin_url: &str, db_name: &str) -> String {
    let (prefix, _) = admin_url.rsplit_once('/').unwrap();
    format!("{prefix}/{db_name}")
}

#[tokio::test]
async fn each_test_gets_an_empty_database() {
    let (db, _name) = migrated_database(admin_url().await).await;
    let count = app::entities::user::Entity::find().count(&db).await.unwrap();
    assert_eq!(count, 0);
}
```

The names are interpolated into `CREATE DATABASE` and `ALTER DATABASE` because Postgres does not
accept a bind parameter there. They are built from a uuid generated here and the run id, which the
Makefile or nextest sets, never from a test fixture, which is what makes that safe.

Do not call `std::env::set_var` to point a test at a different database. It is `unsafe` in edition
2024 and the crate lints forbid unsafe code outright, so the attempt ends in a wall rather than a
warning. Read the environment; construct configuration in memory.

## Using it in a test

Day to day, a test calls `common::test_app()` — `rust-testing`'s shared helper — and reaches the
fresh database as `app.state.db`, next to the frozen clock and the mock upstream the rest of
`AppState` carries.

```rust,verify,test
use app::entities::user;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait, PaginatorTrait};
use uuid::Uuid;

use crate::common;

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

## One template per run

A plain `CREATE DATABASE` copies `template1`, and `Migrator::up` then replays every migration; that
cost grows with every migration. `CREATE DATABASE .. TEMPLATE` from a database already migrated is a
file copy, whatever the schema holds. The harness therefore migrates one template per run and
package, and every test clones it.

- **The run.** `TEST_RUN`, which `make test` and `make cov` set; otherwise `NEXTEST_RUN_ID`, which
  nextest sets in every test process of a run, so bacon's nextest job, CI's `cargo llvm-cov
  nextest` and a direct `cargo nextest run` need no variable of their own. Plain `cargo test` has
  neither, so each test binary takes an id of its own. A template that belongs to one run picks up a
  migration edited before the next run; one kept across runs under a fixed name would not, because
  `migrate up` skips a migration it has applied, edited or not.
- **One per package.** The template is `test_<run>_<package>_template`: one run covers a whole
  workspace, and two crates with migrations of their own must not clone each other's schema.
- **No step before the suite.** A test process that finds no template migrates a candidate of its
  own, closes its pool and renames the candidate into place; the first rename wins.
  A loser's rename fails with `database "…" already exists`, or with `duplicate key value
  violates unique constraint "pg_database_datname_index"` when two renames race, so the harness
  asks whether the template exists now instead of matching the message, and drops its candidate.
  An advisory lock would let one process build while the rest wait, but it needs one pinned
  connection, since a pool may take the lock and release it on different ones, and the rest would
  wait about as long as they now spend building.
- **Nothing connects to the template.** `CREATE DATABASE .. TEMPLATE` fails while any session is
  connected to the source, which is why the builder closes its pool before the rename. Concurrent
  clones of one template are fine.

A clone costs what a plain `CREATE DATABASE` costs, about 30 ms, so the template saves the
migrations, about 4 ms each per test. Measured on the scaffold (macOS arm64, 100 light API tests),
the median test fell from 370 to 341 ms with two migrations and from 450 to 353 ms with 22. A
process that builds the template pays a rename, a clone and a connection more than migrating its own
database would, so a run of one test is about 70 ms slower, and `make test`'s cleanup adds about
0.4 s: for a small suite the gain is disk, not time. Most of what remains is not the database;
`rust-testing` lists it.

## Cleanup and recovery

A test database is about 8 MB, `template1` plus the schema, so a suite of a thousand tests leaves
8 GB behind per run, and a reused container fills the Docker disk within days. `make test` and
`make cov` clean up around the run, in the reused container:

- before the run, they drop every `test_` database older than an hour, by the age of its
  directory (`pg_stat_file('base/<oid>/PG_VERSION')`, superuser only, which the container's
  `postgres` is): leftovers of failed runs, of bacon and of direct nextest runs;
- after a passing run, they drop the run's own databases, its template included, by the
  `test_<TEST_RUN>_` prefix. A failing run keeps them for an hour, so the database a test failed
  on (`TestApp::db_name`) can still be opened.

`TEST_RUN` reaches only those two targets, and each run drops only its own databases, so two
worktrees or two services sharing `rust-powers-test-postgres` can run at once. With
`TEST_DATABASE_URL` the Makefile leaves the server alone: a CI service container is thrown away with
the job, and a long-lived server takes the same query by hand. Teardown inside each test is the
wrong place: it would run before an assertion failure has been read. `docker rm -f
rust-powers-test-postgres` resets everything.

If every database test suddenly fails to connect, the reused container is wedged — its Postgres has
exited or its data directory is corrupt — and reuse attaches to it without checking. A reused
container is never health-checked again. `docker rm -f rust-powers-test-postgres` and re-run.

## MockDatabase

`sea_orm::MockDatabase`, behind the `mock` feature, pre-loads query results and asserts generated
statements. It parses no SQL and enforces no constraint, so it accepts every query Postgres would
reject, and it is not used in this stack: a cloned database costs less than reasoning about what
the mock does not check. Logic that has no query in it is tested as a plain function with no
database at all (`rust-testing`).
