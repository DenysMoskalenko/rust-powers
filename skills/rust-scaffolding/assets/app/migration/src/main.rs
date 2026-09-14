use sea_orm_migration::prelude::*;

#[tokio::main]
async fn main() {
    // Gives `sea-orm-cli migrate up|down|status|fresh`; reads DATABASE_URL from
    // the environment or from .env.
    cli::run_cli(migration::Migrator).await;
}
