//! Migrations: every schema change is a reviewable Rust file, applied in order.
//! `pub use` of the prelude lets migration files write `use migration::prelude::*`.
pub use sea_orm_migration::prelude;
pub use sea_orm_migration::prelude::*;

mod m20260913_000001_create_users;
mod m20260913_000002_create_posts;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260913_000001_create_users::Migration),
            Box::new(m20260913_000002_create_posts::Migration),
        ]
    }
}
