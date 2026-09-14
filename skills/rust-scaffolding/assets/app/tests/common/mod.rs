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
use sea_orm::{ConnectionTrait as _, Database, DatabaseConnection, Statement};
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

/// The whole service on a throwaway database, with time frozen.
///
/// One database per test beats rolling back a transaction: the code under test
/// can commit, and nothing leaks between tests.
pub async fn test_app() -> TestApp {
    let admin_url = admin_url().await;
    let admin = connect_with_retry(admin_url).await;
    let db_name = format!("test_{}", Uuid::now_v7().simple());
    admin
        .execute_raw(Statement::from_string(
            admin.get_database_backend(),
            format!(r#"CREATE DATABASE "{db_name}""#),
        ))
        .await
        .unwrap();
    admin.close().await.unwrap();

    // NOT `admin_url.replace("/postgres", ..)`: that would also rewrite the
    // `postgres://` scheme.
    let (prefix, _) = admin_url.rsplit_once('/').unwrap();
    let database_url = format!("{prefix}/{db_name}");

    let db = connect_with_retry(&database_url).await;
    Migrator::up(&db, None).await.unwrap();

    let mock = httpmock::MockServer::start_async().await;
    let settings = Settings::from_map(HashMap::from([
        ("APP__DATABASE__URL".to_owned(), database_url),
        ("APP__AUTH__JWT_SECRET".to_owned(), "test-secret".to_owned()),
        ("APP__TELEMETRY__JSON_LOGS".to_owned(), "false".to_owned()),
    ]))
    .unwrap();

    let state = AppState {
        db,
        settings: Arc::new(settings),
        clock: Arc::new(FixedClock(frozen_now())),
        // Outbound calls are pointed at `mock` by whatever base URL the code
        // under test is given; the client itself is the production one.
        http: reqwest_middleware::ClientBuilder::new(reqwest::Client::new()).build(),
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
