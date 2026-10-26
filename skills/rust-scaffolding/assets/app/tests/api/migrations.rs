//! Every migration's `down`, run for real. `up` already runs on an empty
//! database once per test run, when the run's template is built.

use migration::{Migrator, MigratorTrait as _};

use crate::common;

#[tokio::test]
async fn every_migration_rolls_back_and_applies_again() {
    let app = common::test_app().await;

    Migrator::refresh(&app.state.db).await.unwrap();
}
