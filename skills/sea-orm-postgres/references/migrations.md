# Migrations

- [There is no autogenerate](#there-is-no-autogenerate)
- [The migration crate](#the-migration-crate)
- [CLI commands](#cli-commands)
- [Writing a migration](#writing-a-migration)
- [Schema helpers](#schema-helpers)
- [Altering a live table: expand, migrate, contract](#altering-a-live-table-expand-migrate-contract)
- [Regenerating entities](#regenerating-entities)
- [Running migrations](#running-migrations)
- [Schema Registry sync](#schema-registry-sync)

## There is no autogenerate

sea-orm-cli has no `--autogenerate`, no `diff` and no `--from-entity`. Code generation points one
way only: database to entities. Migrations are hand-written, entities are generated:

1. `sea-orm-cli migrate generate add_org_to_users` is how a migration file is created: it
   writes the timestamped file under `migration/src/` and registers it in `migration/src/lib.rs`
   (the `mod` line and the `Box::new(..)` entry). Its body is a template — `todo!()` in `up` and
   `down`, which `[lints]` turns into a warning and `-D warnings` into a failure — so rewrite it
   in the house style of the migrations below: `#[derive(DeriveMigrationName)]`, `DeriveIden`
   enums, the `schema::` helpers. A file created by hand needs both `lib.rs` lines added by hand.
2. Fill in `up` and `down`.
3. `sea-orm-cli migrate up` — it reads `DATABASE_URL`, which `.env.example` sets alongside
   `APP__DATABASE__URL`.
4. `sea-orm-cli generate entity --entity-format dense -o src/entities --with-serde both`.
5. Commit the migration and the regenerated entities in the same change.

The payoff is that the database is the single source of truth and entities are a build artifact, so
the two cannot drift. The cost is that nobody writes the migration for you. Anyone reaching for an
`--autogenerate` flag is looking for something that does not exist.

## The migration crate

`sea-orm-cli migrate init` writes the `migration/` crate and nothing else — it never touches the
parent `Cargo.toml`. Add `members = ["migration"]` to the workspace by hand, and fill in the
features: the generated `Cargo.toml` ships `sea-orm-migration` with an empty feature list and the
driver and runtime commented out.

```
migration/
  Cargo.toml     # sea-orm-migration = { version = "2", features = ["sqlx-postgres", "runtime-tokio-rustls"] }  (you fill these in)
  src/
    lib.rs       # pub use sea_orm_migration::prelude::*; pub struct Migrator; impl MigratorTrait
    main.rs      # cli::run_cli(Migrator).await — the binary `sea-orm-cli migrate` drives
    m20220101_000001_create_table.rs   # sample; replace it
```

`lib.rs` lists every migration in order inside `MigratorTrait::migrations`. `migrate generate`
adds the new module and the new `Box::new(..)` line; check that it did.

## CLI commands

| Command | Effect |
|---|---|
| `sea-orm-cli migrate generate NAME` | Write an empty timestamped migration |
| `sea-orm-cli migrate up` | Apply pending migrations |
| `sea-orm-cli migrate down -n 1` | Roll back the last migration |
| `sea-orm-cli migrate status` | List applied and pending migrations |
| `sea-orm-cli migrate fresh` | Drop every table, then re-apply all migrations |
| `sea-orm-cli migrate refresh` | Roll everything back, then re-apply |

The CLI reads `DATABASE_URL` through dotenvy, so a local env file is enough; `-u` overrides it and
`-d` points at the migration crate when it is not `./migration`.

## Writing a migration

```rust,verify
use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::{boolean, string, timestamp_with_time_zone, uuid};

#[derive(DeriveMigrationName)]
pub struct Migration;

// Column names are spelled once, as an iden enum, so a typo is a compile error.
#[derive(DeriveIden)]
enum User {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Post {
    Table,
    Id,
    UserId,
    Title,
    Published,
    CreatedAt,
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    // Note `&self`: in 2.0 the migration methods take a receiver.
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

        // Postgres indexes the referenced primary key, never the referencing column.
        // Without this, every delete or update of a parent row seq-scans the child table.
        manager
            .create_index(
                Index::create()
                    .name("idx_post_user_id")
                    .table(Post::Table)
                    .col(Post::UserId)
                    .to_owned(),
            )
            .await?;

        // Escape hatch for anything the builder cannot express: partial and expression indexes,
        // CREATE EXTENSION, check constraints, data backfills.
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX idx_post_user_id_published ON post (user_id) WHERE published",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Post::Table).to_owned())
            .await
    }
}
```

Write `down` even when nobody plans to run it; it is what makes `migrate fresh` and `refresh`
usable in tests, and it forces a moment of thought about whether the change is reversible at all.

## Schema helpers

`sea_orm_migration::schema` carries over a hundred column constructors. The ones that cover almost
every table: `uuid`, `string`, `string_len`, `text`, `integer`, `big_integer`, `boolean`,
`decimal_len`, `json`, `json_binary`, `timestamp_with_time_zone`, `date`, `float`, `double`, `blob`.
Each has a `_null` variant for nullable columns and a `_uniq` variant that adds a unique key, plus
`pk_auto`, `pk_uuid` and `timestamp_default_now`.

Postgres type choices worth stating: `timestamp_with_time_zone` rather than `timestamp`, so the
entity maps to `DateTimeWithTimeZone` (a `DateTime<FixedOffset>`, `.into()` from `Utc`) instead of a naive local time; `json_binary` rather than `json`, so the
column is `jsonb` and indexable; `decimal_len(col, 12, 2)` for money. An `integer` column is `i32` on
the model, so the DTO field over it is `i32` too.

Auto-increment columns in 2.0 emit `GENERATED BY DEFAULT AS IDENTITY` rather than `serial`. The
legacy behaviour is available behind sea-orm's `postgres-use-serial-pk` feature (sea-orm-migration
has no passthrough for it), but there is no reason to want it on a new schema. With app-side uuid v7
primary keys the question does not arise.

## Altering a live table: expand, migrate, contract

A deployment runs the migration and the new code at different moments, and during a rolling deploy
both versions of the code are live at once. Any change that removes or narrows something must
therefore be split across releases:

1. **Expand.** Add the new nullable column, the new table, or the new index. Old code ignores it.
2. **Migrate.** Deploy code that writes both the old and the new shape, then backfill existing rows
   in batches.
3. **Contract.** Once no running process reads the old shape, drop it in a later migration.

Renaming a column is the same three steps: add, dual-write plus backfill, drop. A single
`ALTER TABLE ... RENAME COLUMN` breaks every process still running the previous release.

```rust,verify
use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::string_null;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[derive(DeriveIden)]
enum User {
    Table,
    Name,
    DisplayName,
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    // Expand phase: the column is nullable, so old code inserting without it still works.
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(User::Table)
                    .add_column(string_null(User::DisplayName))
                    .to_owned(),
            )
            .await?;

        // Fine for a small table. A single UPDATE over a large one holds row locks for the
        // whole migration transaction: batch it outside the migration instead, in a loop of
        // `UPDATE .. WHERE ctid IN (SELECT ctid FROM "user" WHERE "display_name" IS NULL LIMIT 10000)`.
        manager
            .get_connection()
            .execute_unprepared(
                r#"UPDATE "user" SET "display_name" = "name" WHERE "display_name" IS NULL"#,
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(User::Table)
                    .drop_column(User::DisplayName)
                    .to_owned(),
            )
            .await
    }
}
```

A foreign key onto an existing table is the same expand step: a nullable column, the
constraint, and the index Postgres does not create for it.

```rust,verify
use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::uuid_null;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[derive(DeriveIden)]
enum Organisation {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Post {
    Table,
    OrganisationId,
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    // Expand: nullable, so old code that never writes the column keeps inserting.
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Post::Table)
                    .add_column(uuid_null(Post::OrganisationId))
                    .to_owned(),
            )
            .await?;
        manager
            .create_foreign_key(
                ForeignKey::create()
                    .name("fk_post_organisation_id")
                    .from(Post::Table, Post::OrganisationId)
                    .to(Organisation::Table, Organisation::Id)
                    .on_delete(ForeignKeyAction::SetNull)
                    .to_owned(),
            )
            .await?;
        // Postgres indexes the referenced key only; the referencing column is ours.
        manager
            .create_index(
                Index::create()
                    .name("idx_post_organisation_id")
                    .table(Post::Table)
                    .col(Post::OrganisationId)
                    .to_owned(),
            )
            .await
    }

    // Dropping the column drops its constraint and index with it.
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Post::Table)
                    .drop_column(Post::OrganisationId)
                    .to_owned(),
            )
            .await
    }
}
```

Regenerated, the entity gains `organisation: BelongsTo<Option<organisation::Entity>>` — the
nullable-FK shape, `Loaded(None)` for a post without one (`relations.md`). Migrate: new code sets
it, a batched backfill fills old rows. Contract, once every row has one and no old code runs:
`ALTER COLUMN .. SET NOT NULL` in a later migration, and the regenerated field becomes
`BelongsTo<organisation::Entity>`.

Adding an index to a table that is being written to needs `CREATE INDEX CONCURRENTLY`, which
Postgres refuses to run inside a transaction. Opt the migration out:

```rust,ignore
fn use_transaction(&self) -> Option<bool> {
    Some(false)
}
```

`None` means the backend default, which on Postgres wraps each migration in a transaction.

## Regenerating entities

```bash
sea-orm-cli generate entity --entity-format dense -o src/entities --with-serde both
```

`--entity-format dense` is not optional. The generator still defaults to `compact`, the 1.x layout
with a hand-rolled `Relation` enum and `impl Related`, and none of the 2.0 relation loading works
against it. A codebase already generated as compact or expanded regenerates in that format until
someone migrates it on purpose: switching formats rewrites every relation and its callers, so it
is a change of its own, never a side effect of adding a column.

Other flags worth knowing: `--tables` and `--ignore-tables` to scope generation,
`--model-extra-derives` and `--model-extra-attributes` to add project derives,
`--serde-skip-deserializing-primary-key`, and `--er-diagram` to write a mermaid diagram of the
schema. Regenerate rather than hand-editing. `--experimental-preserve-user-modifications` keeps only
extra derives and attributes on `Model` and `Relation` plus the `ActiveModelBehavior` impl block;
any other hand edit is lost on the next run.

## Running migrations

`Migrator::up(&db, None).await?` at startup is fine for a single process. It is racy for several
replicas rolling at once, because the applied-migrations table is the only thing serialising them.
Run migrations as a separate job or init container and gate the startup call behind a setting, so
local development keeps the convenience and production does not inherit the race.

## Schema Registry sync

2.0 can create tables directly from entity definitions, behind two non-default features,
`entity-registry` and `schema-sync`:

```rust,ignore
db.get_schema_registry("app::entities::*").sync(db).await?;
```

`sync` only adds: missing tables, columns, unique keys and foreign keys. It never alters or drops,
it leaves no reviewable artifact, and it is explicitly exempt from semver. Use it while prototyping
before the first migration exists, or in a harness that only needs the tables to be present. It is
not a migration system and must not run against a production database.
