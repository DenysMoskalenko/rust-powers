use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::{boolean, string, timestamp_with_time_zone, uuid};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[derive(DeriveIden)]
enum Post {
    Table,
    Id,
    UserId,
    Title,
    Published,
    CreatedAt,
}

#[derive(DeriveIden)]
enum User {
    Table,
    Id,
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Post::Table)
                    .if_not_exists()
                    .col(uuid(Post::Id).primary_key())
                    .col(uuid(Post::UserId))
                    .col(string(Post::Title))
                    .col(boolean(Post::Published).default(false))
                    .col(
                        timestamp_with_time_zone(Post::CreatedAt)
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_post_user_id")
                            .from(Post::Table, Post::UserId)
                            .to(User::Table, User::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // Postgres indexes the primary key and the unique constraint for you, but
        // never a foreign key column. Without this, deleting a user sequentially
        // scans `post`.
        manager
            .create_index(
                Index::create()
                    .name("idx_post_user_id")
                    .table(Post::Table)
                    .col(Post::UserId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Post::Table).to_owned())
            .await
    }
}
