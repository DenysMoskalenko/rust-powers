# Migrations

- [There is no autogenerate](#there-is-no-autogenerate)
- [The migration crate](#the-migration-crate)
- [CLI commands](#cli-commands)
- [Writing a migration](#writing-a-migration)
- [Schema helpers](#schema-helpers)
- [Altering a live table: expand, migrate, contract](#altering-a-live-table-expand-migrate-contract)
- [Lock-safe DDL on a live table](#lock-safe-ddl-on-a-live-table)
- [Regenerating entities](#regenerating-entities)
- [Running migrations](#running-migrations)
- [Schema Registry sync](#schema-registry-sync)

## There is no autogenerate

sea-orm-cli has no `--autogenerate`, no `diff` and no `--from-entity`. Code generation points one
way only: database to entities. Migrations are hand-written, entities are generated:

1. `sea-orm-cli migrate generate add_org_to_users` is how a migration file is created: it
   writes the timestamped file under `migration/src/` and registers it in `migration/src/lib.rs`
   (the `mod` line and the `Box::new(..)` entry). It is a template — a `schema::*` import, a
   hand-written `MigrationName` impl and `todo!()` in `up` and `down`, which rustc and the
   `[lints]` table warn on and `-D warnings` fails — so replace the whole file in the house style
   of the migrations below: `#[derive(DeriveMigrationName)]`, `DeriveIden`
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
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // The foreign key below locks `user`, a table with traffic, for a moment. Give up
        // after 5 s instead of queueing every write to it behind this migration.
        manager
            .get_connection()
            .execute_unprepared("SET LOCAL lock_timeout = '5s'")
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Post::Table)
                    .if_not_exists()
                    .col(uuid(Post::Id).primary_key().default(Expr::cust("uuidv7()")))
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
        // Dropping a referencing table takes the strongest lock on `user` too.
        manager
            .get_connection()
            .execute_unprepared("SET LOCAL lock_timeout = '5s'")
            .await?;
        manager
            .drop_table(Table::drop().table(Post::Table).to_owned())
            .await
    }
}
```

Write `down` unless the project is forward-only: it makes `migrate down -n 1` useful while a
migration is still being written and `migrate refresh` work at all (`migrate fresh` drops every
table and never calls it). The scaffold's `tests/api/migrations.rs` runs every `down` through
`Migrator::refresh`, so none rots. A forward-only project leaves `down` out, keeping
sea-orm-migration's default that returns an error, and deletes that test.

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

A uuid key takes `.default(Expr::cust("uuidv7()"))`, as above; `pk_uuid` sets no default. `uuidv7()`
is new in Postgres 18, so an older server fails the migration with `function uuidv7() does not
exist`: leave the default off there and assign `Uuid::now_v7()` in Rust. `gen_random_uuid()` is v4,
never for a new table.

Auto-increment columns in 2.0 emit `GENERATED BY DEFAULT AS IDENTITY` rather than `serial`. The
legacy behaviour is available behind sea-orm's `postgres-use-serial-pk` feature (sea-orm-migration
has no passthrough for it), but there is no reason to want it on a new schema. With uuid v7
primary keys the question does not arise.

## Altering a live table: expand, migrate, contract

A deployment runs the migration and the new code at different moments, and during a rolling deploy
both versions of the code are live at once. Any change that removes or narrows something must
therefore be split across releases:

1. **Expand.** One release adds the new nullable column, the new table, or the new index, and
   ships code that writes both the old and the new shape. Old code ignores the addition.
2. **Migrate.** Once that release is the only one running, backfill the existing rows. Not
   earlier: a replica still on the old code keeps inserting rows without the new value, behind
   the backfill.
3. **Contract.** Once every row has the new shape and no running process reads the old one, a
   later release narrows or drops: `NOT NULL`, the old column.

Renaming a column is the same three steps: add, dual-write plus backfill, drop. A single
`ALTER TABLE ... RENAME COLUMN` breaks every process still running the previous release.

The expand migration adds the column and nothing else:

```rust,verify
use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::string_null;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[derive(DeriveIden)]
enum User {
    Table,
    DisplayName,
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    // Expand: the column is nullable, so old code inserting without it still works.
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Before any statement that locks a table with traffic; the next section says why.
        manager
            .get_connection()
            .execute_unprepared("SET LOCAL lock_timeout = '5s'")
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(User::Table)
                    .add_column(string_null(User::DisplayName))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("SET LOCAL lock_timeout = '5s'")
            .await?;
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

The backfill is a migration of its own, in the release after the one that started writing the
column:

```rust,verify
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
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

    // Nothing to undo: the expand migration's `down` drops the column. Without this the
    // default `down` returns an error and `migrate refresh` stops here.
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
```

## Lock-safe DDL on a live table

Postgres queues lock requests. Most `ALTER TABLE` forms take the strongest lock, so the statement
waits for every open transaction that has touched the table, and while it waits, every later
query on that table waits behind it: one forgotten transaction plus one migration is an outage.
Two rules follow.

**Never wait long for a lock.** A migration that locks a table with traffic runs
`SET LOCAL lock_timeout = '5s'` first. That is any `ALTER TABLE`, and also a new table's foreign
key, which locks the table it references, and the drop of a referencing table or column, which
takes the strongest lock on the referenced table too. A statement that cannot get its lock in
time fails with `canceling statement due to lock timeout` (55P03), the migration rolls back, and
the deploy is retried. `SET LOCAL` ends with the transaction, so it never leaks into a pooled
connection; outside a transaction it does nothing but warn.

**Never scan or build under a lock that blocks writes.** Three changes do exactly that when
written the short way, and the schema builder has no lock-safe form for the first two, so they
are raw SQL:

| Change | Short form, and what it blocks | Lock-safe form |
|---|---|---|
| foreign key | `create_foreign_key` checks every existing row with writes to both tables blocked | `ADD CONSTRAINT .. FOREIGN KEY .. NOT VALID` checks new writes and scans nothing; `VALIDATE CONSTRAINT`, in a later transaction, scans with reads and writes going on |
| `NOT NULL` | `ALTER COLUMN .. SET NOT NULL` scans the table with reads and writes blocked | `ADD CONSTRAINT .. NOT NULL "col" NOT VALID`, then `VALIDATE CONSTRAINT` |
| index | `create_index` blocks writes for the whole build | `Index::create().concurrently()`, `CREATE INDEX CONCURRENTLY` |

A new table needs neither `NOT VALID` nor `CONCURRENTLY`: nothing reads or writes it yet, so
`create_table` with its foreign keys and indexes stays as in
[Writing a migration](#writing-a-migration).

Adding an index to a table that is being written to needs `CREATE INDEX CONCURRENTLY`, which
Postgres refuses to run inside a transaction, and a constraint validated in the transaction that
added it holds that statement's lock for the whole scan. Opt the migration out with
`use_transaction`. `None` means the backend default, which on Postgres wraps each migration in a
transaction. With `Some(false)`, `up` runs statement by statement on the pool: nothing rolls
back, a failure leaves the finished steps in place and the migration unrecorded, and the next run
starts `up` from the top. So every step must be safe to run again, and the steps that have to
commit together, or that need `SET LOCAL`, go through `manager.begin()`, which returns a
`SchemaManager` over a transaction of its own.

A foreign key onto an existing table, complete:

```rust,verify
use sea_orm_migration::prelude::*;
use sea_orm_migration::schema::uuid_null;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[derive(DeriveIden)]
enum Post {
    Table,
    OrganisationId,
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    fn use_transaction(&self) -> Option<bool> {
        Some(false)
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 1. Expand: a nullable column, so old code that never writes it keeps inserting.
        //    Column and constraint commit together, so the column's presence answers for
        //    both when this runs a second time.
        if !manager.has_column("post", "organisation_id").await? {
            let txn = manager.begin().await?;
            let db = txn.get_connection();
            db.execute_unprepared("SET LOCAL lock_timeout = '5s'")
                .await?;
            txn.alter_table(
                Table::alter()
                    .table(Post::Table)
                    .add_column(uuid_null(Post::OrganisationId))
                    .to_owned(),
            )
            .await?;
            db.execute_unprepared(
                r#"ALTER TABLE "post" ADD CONSTRAINT "fk_post_organisation_id"
                   FOREIGN KEY ("organisation_id") REFERENCES "organisation" ("id")
                   ON DELETE SET NULL NOT VALID"#,
            )
            .await?;
            txn.commit().await?;
        }

        // 2. The scan, in a transaction of its own. A no-op on a constraint already valid.
        manager
            .get_connection()
            .execute_unprepared(
                r#"ALTER TABLE "post" VALIDATE CONSTRAINT "fk_post_organisation_id""#,
            )
            .await?;

        // 3. The index Postgres does not create for a referencing column. An interrupted
        //    concurrent build leaves an INVALID index under this name, and `if_not_exists`
        //    would keep it: drop, then build.
        manager
            .drop_index(
                Index::drop()
                    .concurrently()
                    .if_exists()
                    .name("idx_post_organisation_id")
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .concurrently()
                    .name("idx_post_organisation_id")
                    .table(Post::Table)
                    .col(Post::OrganisationId)
                    .to_owned(),
            )
            .await
    }

    // Dropping the column drops its constraint and index with it.
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let txn = manager.begin().await?;
        txn.get_connection()
            .execute_unprepared("SET LOCAL lock_timeout = '5s'")
            .await?;
        txn.alter_table(
            Table::alter()
                .table(Post::Table)
                .drop_column(Post::OrganisationId)
                .to_owned(),
        )
        .await?;
        txn.commit().await
    }
}
```

Regenerated, the entity gains `organisation: BelongsTo<Option<organisation::Entity>>`, the
nullable-FK shape: `Loaded(None)` for a post without one. Migrate: new code sets it, a batched
backfill fills old rows.

Validation and a concurrent build take as long as the table is big, so the connection that runs
migrations carries no `statement_timeout`; see [Running migrations](#running-migrations).

### Recovering an INVALID index

A concurrent build that is cancelled or fails (a `statement_timeout`, a deadlock, a duplicate
under a unique index) leaves the index behind, marked INVALID: no query uses it, every
write still maintains it, and `CREATE INDEX CONCURRENTLY IF NOT EXISTS` sees the name and skips
it. `\d post` in `psql` shows the mark, and this lists every one in the database:

```sql
SELECT indexrelid::regclass FROM pg_index WHERE NOT indisvalid;
```

Recover with `DROP INDEX CONCURRENTLY "idx_post_organisation_id"` and build again, or rebuild in
place with `REINDEX INDEX CONCURRENTLY "idx_post_organisation_id"`. The migration above does the
first on every run. A concurrent build also waits for older transactions that wrote to the table,
and for every query or snapshot older than itself, to finish: one long-running report stalls it
for as long as the report runs, and that wait is not a failure.

### Contract: NOT NULL

Once every row has an organisation and no running code inserts without one, a later release
makes the column required. Same shape as above, and both statements are safe to run again:

```rust,verify
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    fn use_transaction(&self) -> Option<bool> {
        Some(false)
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // From here on an insert without the column fails with 23502; nothing is scanned yet.
        let txn = manager.begin().await?;
        let db = txn.get_connection();
        db.execute_unprepared("SET LOCAL lock_timeout = '5s'")
            .await?;
        db.execute_unprepared(
            r#"ALTER TABLE "post" ADD CONSTRAINT "post_organisation_id_not_null"
               NOT NULL "organisation_id" NOT VALID"#,
        )
        .await?;
        txn.commit().await?;

        // Fails with 23502 while a NULL is left; backfill, then run the migration again.
        manager
            .get_connection()
            .execute_unprepared(
                r#"ALTER TABLE "post" VALIDATE CONSTRAINT "post_organisation_id_not_null""#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let txn = manager.begin().await?;
        let db = txn.get_connection();
        db.execute_unprepared("SET LOCAL lock_timeout = '5s'")
            .await?;
        db.execute_unprepared(r#"ALTER TABLE "post" ALTER COLUMN "organisation_id" DROP NOT NULL"#)
            .await?;
        txn.commit().await
    }
}
```

The regenerated field becomes `BelongsTo<organisation::Entity>`. `NOT VALID` on a `NOT NULL`
constraint is new in Postgres 18. On an older server add
`CHECK ("organisation_id" IS NOT NULL) NOT VALID`, validate it, then run `SET NOT NULL`, which
skips its scan when a valid check already proves the column, and drop the check.

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

An entity with no relations fails `-D warnings` under the pedantic lints: `#[sea_orm::model]`
expands to an `async fn` with no `.await`, which `clippy::unused_async_trait_impl` reports. The
file is generated, so suppress it where the module is declared, in `lib.rs`:

```rust
#[allow(
    clippy::unused_async_trait_impl,
    reason = "#[sea_orm::model] on an entity with no relations expands to an async fn with no .await; \
              allow, not expect: it fires only while such an entity exists, as after posts is removed"
)]
pub mod entities;
```

`allow`, not `expect`: once every entity has a relation, an unfulfilled `expect` fails the build.

## Running migrations

`Migrator::up` applies each pending migration in a transaction of its own and takes no lock, so
two replicas that start together both find a migration pending and both run it. On the service
pool it also inherits that pool's `statement_timeout`, which cancels a backfill or an index build
half-way. A startup call therefore gets connections and a lock of its own. This is `migrate()`
from the scaffold's `main.rs`, called before the service pool exists:

```rust,verify
use std::time::Duration;

use anyhow::Context as _;
use migration::{Migrator, MigratorTrait as _};
use sea_orm::{ConnectionTrait as _, TransactionTrait as _};

/// Any fixed number. Advisory locks are scoped to the database, so it only has
/// to differ from the keys the service's own jobs take.
const MIGRATION_LOCK_KEY: i64 = 4_815_162_342;

/// Migrations get connections of their own, with no `statement_timeout`: the
/// service's ten seconds would cancel a backfill or an index build half-way.
/// An advisory lock, held by an open transaction until the migrations finish,
/// makes replicas that start together migrate one at a time; the ones that
/// waited find nothing left to apply. With rolling deploys the old code still
/// runs against the new schema, so sequence breaking changes as expand,
/// migrate, contract, or run migrations as a separate job (`make migrate`).
async fn migrate(database_url: &str) -> anyhow::Result<()> {
    let mut options = sea_orm::ConnectOptions::new(database_url);
    options
        // One for the transaction that holds the lock, one for the migrations.
        .max_connections(2)
        .connect_timeout(Duration::from_secs(5))
        .sqlx_logging(false);
    let db = sea_orm::Database::connect(options)
        .await
        .context("connecting to Postgres for migrations")?;

    let lock = db.begin().await.context("opening the migration lock")?;
    // This session idles in its transaction while the migrations run on the
    // other connection. A role or server timeout for that would end it, and
    // the lock with it. `transaction_timeout` exists from Postgres 17: drop
    // that statement on an older server.
    lock.execute_unprepared(
        "SET LOCAL idle_in_transaction_session_timeout = 0; SET LOCAL transaction_timeout = 0",
    )
    .await
    .context("lifting the idle timeouts from the migration lock")?;
    lock.execute_unprepared(&format!(
        "SELECT pg_advisory_xact_lock({MIGRATION_LOCK_KEY})"
    ))
    .await
    .context("waiting for the migration lock")?;

    let migrated = Migrator::up(&db, None).await.context("running migrations");

    // Committing releases the lock; so does a crash, with the session.
    lock.commit()
        .await
        .context("releasing the migration lock")?;
    db.close()
        .await
        .context("closing the migration connections")?;
    migrated
}
```

- **A pool of its own.** sea-orm sets `statement_timeout` on every pooled connection, as a startup
  option. A migration could lift it with `SET LOCAL statement_timeout = 0`, but every migration
  would have to remember to, and one that runs outside a transaction has no transaction to scope
  it to; a `CREATE INDEX CONCURRENTLY` the timeout cancels leaves an INVALID index behind. The
  migration pool sets none, and no `lock_timeout` either: a replica waiting for the migration
  lock is waiting for a lock.
- **The lock is transaction-level, in a transaction that stays open.** The open transaction pins
  one connection for as long as the lock is needed, and Postgres releases the lock when that
  transaction or its session ends. A session-level `pg_advisory_lock` taken through a pool
  belongs to whichever connection ran it and goes back into the pool still held.
- **The lock session idles while the migrations run.** A role or server that sets
  `idle_in_transaction_session_timeout` or `transaction_timeout` would terminate it, lock
  included, under a migration longer than that, so the lock transaction lifts both for itself
  with `SET LOCAL` before it takes the lock. `transaction_timeout` exists from Postgres 17; on
  an older server drop that statement.

The lock orders the replicas of one release. It does nothing about the previous release, which
keeps serving on the migrated schema until the rollout ends. So a startup migration fits what
old code can live with, an expand step, and a job is right for the rest:

- a change the previous release cannot run against, unless it is sequenced as expand, migrate,
  contract;
- a migration that outlasts the orchestrator's startup deadline (a large backfill, validating
  or indexing a large table): the pod is killed mid-migration and its replacement starts over;
- a service role that may not run DDL.

The job is `sea-orm-cli migrate up` (`make migrate`), run once per deploy. It takes no lock, so
it is a pipeline step or a one-off job, never an init container, which runs once per pod and
races like an unlocked startup call. A project that migrates from a job drops the startup call.

## Schema Registry sync

2.0 can create tables directly from entity definitions, behind two non-default features,
`entity-registry` and `schema-sync`:

```rust,ignore
db.get_schema_registry("app::entities::*").sync(db).await?;
```

`sync` adds missing tables, columns, unique keys and foreign keys, renames a column marked
`#[sea_orm(renamed_from = "old")]`, and tries to drop any unique key the entity does not declare,
one a migration created included. It never alters a column type or drops a column or table, it
leaves no reviewable artifact, and it is explicitly exempt from semver. Use it while prototyping
before the first migration exists, or in a harness that only needs the tables to be present. It is
not a migration system and must not run against a production database.
