//! Shared test setup.
//!
//! `tests/common/mod.rs`, NOT `tests/common.rs`: every `.rs` directly in
//! `tests/` is compiled as its own test binary, so `tests/common.rs` would show
//! up as an empty test target. A subdirectory module is only compiled by the
//! binaries that declare `mod common;`.
#![allow(dead_code, reason = "each test binary compiles the whole file")]
// `clippy.toml`'s allow-unwrap-in-tests covers the body of a `#[test]` function
// only. Helpers like these still warn without an explicit allow.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers"
)]

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use app::clock::FixedClock;
use app::config::Settings;
use app::{AppState, build_router};
use axum_test::TestServer;
use chrono::{DateTime, TimeZone, Utc};
use migration::{Migrator, MigratorTrait as _};
use sea_orm::{ConnectionTrait as _, Database, DatabaseConnection, raw_sql};
use testcontainers::runners::AsyncRunner as _;
use testcontainers::{ContainerAsync, ImageExt as _, ReuseDirective};
use testcontainers_modules::postgres::Postgres;
use tokio::sync::OnceCell;
use uuid::Uuid;

/// One fixed instant for every test that renders or stores a timestamp.
pub fn frozen_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 13, 12, 0, 0).unwrap()
}

pub struct TestApp {
    pub server: TestServer,
    pub state: AppState,
    /// The throwaway database this test owns; handy in failure messages.
    pub db_name: String,
    /// Stands in for every upstream HTTP dependency.
    pub mock: httpmock::MockServer,
}

static ADMIN_URL: LazyLock<OnceCell<String>> = LazyLock::new(OnceCell::new);

/// A connection string for a server that can `CREATE DATABASE`.
///
/// nextest forks one process per test, so this `OnceCell` is per test, not per
/// binary: a plain testcontainers call would start one container per test.
/// `TEST_DATABASE_URL` (docker compose locally, `services:` in CI) is the fast
/// path; the fallback container is named and reused so repeated runs share it.
/// A reused container is never cleaned up. If a run hangs at startup after a
/// Docker restart, its data is corrupt: `docker rm -f rust-powers-test-postgres`.
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

/// Ten test processes start at once on a cold run and every one of them tries to
/// create the container. Docker lets exactly one win and answers the rest with a
/// 409 name conflict; on their next attempt testcontainers finds the container
/// that now exists and attaches to it instead. Any other error is real and
/// surfaces at once.
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

/// A reused container is only "started" for the process that created it: every
/// other test process attaches to one that may still be booting Postgres. Retry
/// rather than fail the first test of a cold run.
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

/// Names the run every test process belongs to: `TEST_RUN` under `make test`
/// and `make cov`, nextest's own run id under any other nextest run (bacon, CI,
/// a direct `cargo nextest run`), and one id per test binary under `cargo test`.
fn run_id() -> &'static str {
    static RUN: LazyLock<String> = LazyLock::new(|| {
        let id = ["TEST_RUN", "NEXTEST_RUN_ID"]
            .into_iter()
            .find_map(|var| std::env::var(var).ok().filter(|id| !id.is_empty()))
            .unwrap_or_else(|| Uuid::now_v7().simple().to_string());
        // 20 characters keep every database name inside Postgres's 63 bytes.
        id.chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(20)
            .collect()
    });
    &RUN
}

/// `test_<run>_<uuid>`: the run leads the name, so `make test` can drop the
/// databases of a run that passed.
async fn create_database(admin: &DatabaseConnection, template: Option<&str>) -> String {
    let db_name = format!("test_{}_{}", run_id(), Uuid::now_v7().simple());
    let template = template
        .map(|t| format!(r#" TEMPLATE "{t}""#))
        .unwrap_or_default();
    admin
        .execute_unprepared(&format!(r#"CREATE DATABASE "{db_name}"{template}"#))
        .await
        .unwrap();
    db_name
}

async fn database_exists(admin: &DatabaseConnection, name: &str) -> bool {
    admin
        .query_one_raw(raw_sql!(
            Postgres,
            "SELECT FROM pg_database WHERE datname = {name}"
        ))
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
        .execute_unprepared(&format!(
            r#"ALTER DATABASE "{candidate}" RENAME TO "{template}""#
        ))
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

/// A migrated, empty database of this test's own, and its name.
///
/// Every test clones the run's template, a file copy, instead of running every
/// migration. Running the real migrations, once per run, is also how the
/// migrations get tested.
async fn migrated_database(admin_url: &str) -> (DatabaseConnection, String) {
    let admin = connect_with_retry(admin_url).await;
    // Keyed by package too: a workspace's crates share one run, not one schema.
    let template = format!("test_{}_{:.20}_template", run_id(), env!("CARGO_PKG_NAME"));
    if !database_exists(&admin, &template).await {
        build_template(&admin, admin_url, &template).await;
    }
    let db_name = create_database(&admin, Some(&template)).await;
    admin.close().await.unwrap();
    (
        connect_with_retry(&database_url(admin_url, &db_name)).await,
        db_name,
    )
}

/// NOT `admin_url.replace("/postgres", ..)`: that would also rewrite the
/// `postgres://` scheme.
fn database_url(admin_url: &str, db_name: &str) -> String {
    let (prefix, _) = admin_url.rsplit_once('/').unwrap();
    format!("{prefix}/{db_name}")
}

/// The whole service on a throwaway database, with time frozen.
///
/// One database per test beats rolling back a transaction: the code under test
/// can commit, and nothing leaks between tests.
pub async fn test_app() -> TestApp {
    let admin_url = admin_url().await;
    let (db, db_name) = migrated_database(admin_url).await;

    let mock = httpmock::MockServer::start_async().await;
    let settings = Settings::from_map(HashMap::from([
        (
            "APP__DATABASE__URL".to_owned(),
            database_url(admin_url, &db_name),
        ),
        ("APP__AUTH__JWT_SECRET".to_owned(), "test-secret".to_owned()),
        ("APP__TELEMETRY__JSON_LOGS".to_owned(), "false".to_owned()),
    ]))
    .unwrap();

    // Outbound calls are pointed at `mock` by whatever base URL the code under
    // test is given. `no_proxy` keeps them off any proxy, and on macOS it skips
    // the system proxy lookup, which lists every file beside the test binary.
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let state = AppState {
        db,
        settings: Arc::new(settings),
        clock: Arc::new(FixedClock(frozen_now())),
        http: reqwest_middleware::ClientBuilder::new(client).build(),
    };

    // axum-test 21: `new` returns Self and panics; `try_new` returns a Result.
    let server = TestServer::new(build_router(state.clone()));
    TestApp {
        server,
        state,
        db_name,
        mock,
    }
}
