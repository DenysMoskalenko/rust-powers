# Queries, writes and transactions

- [Typed columns](#typed-columns)
- [Dynamic filters](#dynamic-filters)
- [Pagination](#pagination)
- [Projections and aggregates](#projections-and-aggregates)
- [Writes](#writes)
- [Upserts and bulk writes](#upserts-and-bulk-writes)
- [Error mapping](#error-mapping)
- [Raw SQL](#raw-sql)
- [Transactions](#transactions)
- [Advisory locks](#advisory-locks)
- [Idempotency and the outbox](#idempotency-and-the-outbox)

## Typed columns

`user::COLUMN` is a generated struct whose fields are typed wrappers around the plain `Column`
enum: `StringColumn`, `UuidColumn`, `BoolColumn`, `NumericColumn`, `DateTimeLikeColumn` and so on.
Comparing against the wrong type is a compile error rather than a runtime mismatch, which is the
whole point of preferring it.

The wrappers implement `Iden` and `IntoSimpleExpr` but **not** `ColumnTrait`, so three APIs still
need the enum:

| Use | Takes |
|---|---|
| `.filter(..)`, `.having(..)` | `user::COLUMN.email` — typed, preferred |
| `.order_by_asc(..)`, `.order_by_desc(..)`, `.group_by(..)` | either |
| `OnConflict::column(..)`, `.update_columns([..])` | either (`IntoIden`) |
| `.column(..)`, `.column_as(..)` | `user::Column::Email` only |
| `.cursor_by(..)` | `user::Column::Id` only |
| `.on_conflict_do_nothing_on([..])` | `user::Column::Email` only |

`user::COLUMN.email.0` unwraps a typed column to the enum when a helper needs both.

## Dynamic filters

Build a `Condition` and add only the parts that are present. `add_option(None)` is a no-op, which
makes optional filters a straight `map` with no branching.

```rust,verify
use crate::entities::user;
use sea_orm::{
    ConnectionTrait, DbErr, EntityTrait, QueryFilter,
    query::Condition,
    sea_query::ExprTrait,
};
use uuid::Uuid;

#[derive(Debug, Default)]
pub struct UserFilter {
    pub name_contains: Option<String>,
    pub emails: Vec<String>,
    pub ids: Vec<Uuid>,
}

fn user_condition(f: &UserFilter) -> Condition {
    Condition::all()
        // `contains` is LIKE %..%; `like` and `ilike` take an explicit pattern.
        .add_option(
            f.name_contains
                .as_ref()
                .map(|n| user::COLUMN.name.contains(n)),
        )
        // `eq_any` builds `= ANY($1)`, one bind parameter. `is_in` builds `IN ($1, $2, ...)`,
        // one parameter per value — past a few dozen values that alone is worth avoiding.
        .add_option((!f.emails.is_empty()).then(|| user::COLUMN.email.eq_any(f.emails.clone())))
        .add_option((!f.ids.is_empty()).then(|| user::COLUMN.id.eq_any(f.ids.clone())))
}

pub async fn find_users<C: ConnectionTrait>(
    db: &C,
    filter: &UserFilter,
) -> Result<Vec<user::Model>, DbErr> {
    user::Entity::find().filter(user_condition(filter)).all(db).await
}
```

`Condition::all()` is AND, `Condition::any()` is OR, and they nest. `Condition: From<bool>` was
removed in 2.0, so passing a bare boolean to `add_option` no longer compiles.

Anything the builder cannot express goes through sea-query directly:
`Expr::cust_with_values("lower($1) = $2", [..])`, or `.into_query()` to get the raw
`SelectStatement` for a CTE or window function. Both need `use sea_orm::sea_query::ExprTrait;` in
scope, which is the cause of most "no method named like found for enum Expr" errors.

## Pagination

```rust,verify
use crate::entities::user;
use sea_orm::{
    ConnectionTrait, Cursor, DbErr, EntityTrait, PaginatorTrait, QueryOrder, SelectModel,
};
use uuid::Uuid;

pub struct PageOfUsers {
    pub items: Vec<user::Model>,
    pub total: u64,
    pub pages: u64,
}

pub async fn list_users<C: ConnectionTrait>(
    db: &C,
    page: u64,
    page_size: u64,
) -> Result<PageOfUsers, DbErr> {
    let paginator = user::Entity::find()
        .order_by_asc(user::COLUMN.created_at)
        // Tiebreaker: OFFSET over a non-total order returns rows twice or not at all.
        .order_by_asc(user::COLUMN.id)
        .paginate(db, page_size);

    // A second round trip: SELECT COUNT(*) over the same filter. Skip it for infinite scroll.
    let counts = paginator.num_items_and_pages().await?;
    Ok(PageOfUsers {
        items: paginator.fetch_page(page).await?,
        total: counts.number_of_items,
        pages: counts.number_of_pages,
    })
}

// Keyset pagination: stable under concurrent inserts and no OFFSET scan, at the cost of
// losing "jump to page 7". `cursor_by` needs the plain Column enum.
pub fn users_after(after: Option<Uuid>) -> Cursor<SelectModel<user::Model>> {
    let mut cursor = user::Entity::find().cursor_by(user::Column::Id);
    if let Some(id) = after {
        cursor.after(id);
    }
    cursor.first(50);
    cursor
}
```

`PaginatorTrait` also puts `.count(db)` and `.exists(db)` directly on a `Select`. The HTTP shape of
a page — the query parameters and the response envelope — belongs to `axum-service`.

## Projections and aggregates

Select only the columns the caller needs. `DerivePartialModel` already implies `FromQueryResult` in
2.0, so deriving both is a conflict.

```rust,verify
use crate::entities::{post, user};
use sea_orm::{
    ConnectionTrait, DbErr, DerivePartialModel, EntityTrait, FromQueryResult, QuerySelect,
    sea_query::ExprTrait,
};
use uuid::Uuid;

#[derive(Debug, DerivePartialModel)]
#[sea_orm(entity = "user::Entity")]
pub struct UserCard {
    pub id: Uuid,
    pub name: String,
}

pub async fn user_cards<C: ConnectionTrait>(db: &C) -> Result<Vec<UserCard>, DbErr> {
    user::Entity::find().into_partial_model().all(db).await
}

// Two columns and nothing else: `into_tuple` skips defining a struct.
pub async fn id_and_email<C: ConnectionTrait>(db: &C) -> Result<Vec<(Uuid, String)>, DbErr> {
    user::Entity::find()
        .select_only()
        .column(user::Column::Id)
        .column(user::Column::Email)
        .into_tuple()
        .all(db)
        .await
}

#[derive(Debug, FromQueryResult)]
pub struct AuthorStats {
    pub user_id: Uuid,
    pub post_count: i64,
}

pub async fn author_stats<C: ConnectionTrait>(db: &C) -> Result<Vec<AuthorStats>, DbErr> {
    post::Entity::find()
        .select_only()
        .column_as(post::Column::UserId, "user_id")
        .column_as(post::COLUMN.id.count(), "post_count")
        .group_by(post::COLUMN.user_id)
        // Chaining a comparison onto an aggregate needs ExprTrait in scope.
        .having(post::COLUMN.id.count().gt(0))
        .into_model::<AuthorStats>()
        .all(db)
        .await
}
```

Postgres returns `COUNT(*)` as `i64` and `SUM(numeric)` as `Option<Decimal>`, null over an empty
group. A partial model can nest a whole related `Model` with `#[sea_orm(nested)]` when the query
joins it; `.into_json()` produces untyped rows.

## Writes

The house style is a struct literal with every column `Set`, then `.insert()`. It is exhaustive, so
the compiler names a forgotten column, and a reviewer sees the whole row at once.

```rust,verify
use crate::entities::{post, user};
use sea_orm::{ActiveModelTrait, ActiveValue::Set, ConnectionTrait, DbErr, EntityTrait};
use uuid::Uuid;

pub async fn create_user<C: ConnectionTrait>(
    db: &C,
    email: &str,
    name: &str,
) -> Result<user::Model, DbErr> {
    user::ActiveModel {
        // uuid v7 in application code: the id exists before the INSERT, so it can be logged,
        // published on a bus, and used to build child rows without a round trip.
        id: Set(Uuid::now_v7()),
        email: Set(email.to_owned()),
        name: Set(name.to_owned()),
        created_at: Set(chrono::Utc::now().into()),
    }
    .insert(db)
    .await
}

// Partial update: convert the Model, assign only what changed. Untouched columns stay
// `Unchanged` and are left out of the SET clause, so no concurrent writer loses a column
// this request never read.
pub async fn rename_user<C: ConnectionTrait>(
    db: &C,
    found: user::Model,
    new_name: &str,
) -> Result<user::Model, DbErr> {
    let mut active: user::ActiveModel = found.into();
    active.name = Set(new_name.to_owned());
    active.update(db).await
}

// Set-based update: one statement, no read.
pub async fn publish_all<C: ConnectionTrait>(db: &C, user_id: Uuid) -> Result<u64, DbErr> {
    use sea_orm::{QueryFilter, sea_query::Expr};
    let res = post::Entity::update_many()
        .col_expr(post::COLUMN.published, Expr::value(true))
        .filter(post::COLUMN.user_id.eq(user_id))
        .exec(db)
        .await?;
    Ok(res.rows_affected)
}
```

Never call `.save()` on a row whose primary key was assigned in application code. `save()` dispatches
on `is_update()`, which is literally "is every primary-key column `Set`" — with `Uuid::now_v7()` that
is always true, so a brand-new row is issued as an `UPDATE`, matches nothing, and fails with
`DbErr::RecordNotUpdated`. `save()` is only an insert-or-update when the *database* assigns the key.

The nested builder, `ActiveModel::builder()`, is the exception worth knowing. It generates
`set_<column>` plus `set_<entity>` for `HasOne`/`BelongsTo` and `add_<entity>` for `HasMany`, opens
its own transaction, and inserts a whole object graph in topological order — which is why it needs a
`TransactionTrait` bound. Reach for it when a single call must create parent, children and junction
rows together; use the struct literal for everything else. It is `.insert()` there too: `.save()`
hits exactly the same trap.

## Upserts and bulk writes

```rust,verify
use crate::entities::user;
use sea_orm::{ConnectionTrait, DbErr, EntityTrait, TryInsertResult, sea_query::OnConflict};

// ON CONFLICT ("email") DO UPDATE SET "name" = "excluded"."name"
pub async fn upsert_users<C: ConnectionTrait>(
    db: &C,
    rows: Vec<user::ActiveModel>,
) -> Result<Vec<user::Model>, DbErr> {
    user::Entity::insert_many(rows)
        .on_conflict(
            OnConflict::column(user::COLUMN.email)
                .update_columns([user::COLUMN.name])
                .to_owned(),
        )
        .exec_with_returning(db)
        .await
}

// Idempotent insert that reports whether anything happened. `on_conflict_do_nothing()` targets
// the primary key only; a conflict on any other unique index needs `_on` with that column.
pub async fn insert_if_new<C: ConnectionTrait>(db: &C, row: user::ActiveModel) -> Result<bool, DbErr> {
    let result = user::Entity::insert(row)
        .on_conflict_do_nothing_on([user::Column::Email])
        .exec(db)
        .await?;
    Ok(matches!(result, TryInsertResult::Inserted(_)))
}

// Postgres caps a statement at 65535 bind parameters, so the real ceiling is
// 65535 / columns_per_row. Chunking at a thousand rows stays well clear.
pub async fn bulk_insert<C: ConnectionTrait>(
    db: &C,
    rows: &[user::ActiveModel],
) -> Result<usize, DbErr> {
    for chunk in rows.chunks(1000) {
        user::Entity::insert_many(chunk.to_vec()).exec(db).await?;
    }
    Ok(rows.len())
}
```

An empty iterator is safe in 2.0: `insert_many([])` returns without issuing a statement, and the 1.x
`on_empty_do_nothing()` is gone. `exec()` yields `last_insert_id`; `exec_with_returning()` yields the
rows. `on_conflict_do_nothing_on([..])` returns a `TryInsertResult` with `Inserted`, `Conflicted` and
`Empty` variants; the bare `on_conflict_do_nothing()` builds its conflict target from the primary key,
so on a duplicate email it raises 23505 instead of reporting `Conflicted`.

## Error mapping

`DbErr::sql_err()` is the supported way to identify a constraint violation. Matching on the inner
sqlx error is not: in 2.0 it sits behind an `Arc` so that `DbErr` can be `Clone`, and its shape is
not stable API. Two violations are worth naming — 23505, a duplicate, and 23503, a reference to a
row that is not there — and both are handled *after* the statement, because a pre-flight `SELECT`
loses every race and the constraint does not:

```rust,verify
use crate::entities::{post, user};
use crate::error::AppError;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, ConnectionTrait, DbErr, SqlErr};
use uuid::Uuid;

/// Postgres 23505: the unique index fired. Raced inserts land here too.
pub fn is_duplicate(err: &DbErr) -> bool {
    matches!(err.sql_err(), Some(SqlErr::UniqueConstraintViolation(_)))
}

/// Postgres 23503: a foreign key points at a row that does not exist (or was
/// deleted between the caller's check and this statement).
pub fn is_missing_reference(err: &DbErr) -> bool {
    matches!(err.sql_err(), Some(SqlErr::ForeignKeyConstraintViolation(_)))
}

pub async fn create_user<C: ConnectionTrait>(
    db: &C,
    email: &str,
    name: &str,
) -> Result<user::Model, AppError> {
    user::ActiveModel {
        id: Set(Uuid::now_v7()),
        email: Set(email.to_owned()),
        name: Set(name.to_owned()),
        created_at: Set(chrono::Utc::now().into()),
    }
    .insert(db)
    .await
    .map_err(|e| {
        if is_duplicate(&e) {
            // A constant, not the email: a 409 is logged at ERROR and an address is PII.
            AppError::Conflict("email already registered".to_owned())
        } else {
            e.into()
        }
    })
}

/// For `POST /users/{user_id}/posts`: the parent id comes from the path, so a
/// missing parent means the URL names nothing, and 23503 becomes `NotFound`.
pub async fn create_post<C: ConnectionTrait>(
    db: &C,
    user_id: Uuid,
    title: &str,
) -> Result<post::Model, AppError> {
    post::ActiveModel {
        id: Set(Uuid::now_v7()),
        user_id: Set(user_id),
        title: Set(title.to_owned()),
        published: Set(false),
        created_at: Set(chrono::Utc::now().into()),
    }
    .insert(db)
    .await
    .map_err(|e| {
        if is_missing_reference(&e) {
            AppError::NotFound(format!("user {user_id} not found"))
        } else {
            e.into()
        }
    })
}
```

The service picks the domain outcome. A duplicate is `Conflict`. For 23503, where the parent id
came from decides:

- **From the path** (`POST /users/{user_id}/posts`): the URL names a parent that is not there, so
  the service classifies the violation to `NotFound`, as `create_post` does.
- **From the body** (`POST /posts` with a `user_id` field): the service does not classify it. The
  bare `DbErr` goes up and reaches `axum-service`'s 23503 arm, a 422 with the constant body
  `"invalid reference"`.

Which HTTP status each `AppError` variant renders as, the 23503 arm, and the default the `Db` arm
applies to any other bare `DbErr` are `axum-service`'s.

Other variants worth handling by name: `DbErr::RecordNotFound`, `RecordNotInserted`,
`RecordNotUpdated`, and `ConnectionAcquire(ConnAcquireErr::Timeout)`, which means the pool is
exhausted rather than that the database is down.

## Raw SQL

```rust,verify
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult, raw_sql};
use uuid::Uuid;

#[derive(Debug, FromQueryResult)]
pub struct AuthorRow {
    pub name: String,
    pub posts: i64,
}

pub async fn top_authors<C: ConnectionTrait>(
    db: &C,
    ids: &[Uuid],
    min_posts: i64,
) -> Result<Vec<AuthorRow>, DbErr> {
    AuthorRow::find_by_statement(raw_sql!(
        Postgres,
        r#"SELECT u."name" AS "name", COUNT(p."id") AS "posts"
           FROM "user" u
           LEFT JOIN "post" p ON p."user_id" = u."id"
           WHERE u."id" IN ({..ids})
           GROUP BY u."id", u."name"
           HAVING COUNT(p."id") >= {min_posts}
           ORDER BY "posts" DESC"#
    ))
    .all(db)
    .await
}
```

`raw_sql!` is not string formatting. `{expr}` becomes a bind parameter, `{..slice}` expands to a
parameter list, and field access works inside the braces, so there is no injection surface. The
first argument picks the backend, which decides placeholder syntax and quoting.

`Entity::find().from_raw_sql(raw_sql!(..))` keeps the `Model` type. A hand-built `Statement` goes to
`execute_raw` or `query_all_raw` — in 2.0 the plain `execute` and `query_all` take a sea-query
statement instead, which is the cause of most type errors when porting 1.x code.

## Transactions

Every service function takes `&C where C: ConnectionTrait`. Both `DatabaseConnection` and
`DatabaseTransaction` implement it, so one function works against the pool and inside a transaction
with no wrapper type and no `dyn`. Taking `&DatabaseConnection` instead locks every caller out of
composing the function into a larger unit of work.

```rust,verify
use crate::entities::user;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ConnectionTrait, DbErr, EntityTrait, QuerySelect,
    TransactionError, TransactionTrait, sea_query::LockType,
};
use uuid::Uuid;

// Add a bound only where the function needs the capability. This one opens its own
// transaction, so it needs TransactionTrait as well.
pub async fn rename_user<C: ConnectionTrait + TransactionTrait>(
    db: &C,
    user_id: Uuid,
    new_name: String,
) -> Result<user::Model, DbErr> {
    db.transaction::<_, user::Model, DbErr>(|txn| {
        Box::pin(async move {
            let found = user::Entity::find_by_id(user_id)
                // SELECT ... FOR UPDATE: hold the row until commit.
                .lock(LockType::Update)
                .one(txn)
                .await?
                .ok_or_else(|| DbErr::RecordNotFound(format!("user {user_id}")))?;

            let mut active: user::ActiveModel = found.into();
            active.name = Set(new_name);
            active.update(txn).await
        })
    })
    .await
    // The closure form wraps the error; both variants collapse back to the caller's type.
    .map_err(|e| match e {
        TransactionError::Connection(e) | TransactionError::Transaction(e) => e,
    })
}
```

The closure commits on `Ok` and rolls back on `Err`. `Box::pin` is mandatory: the callback returns a
pinned boxed future. The error type must be `Display + Debug + Send`.

When the work does not fit a closure, use the explicit form: `db.begin()`, then `commit()` or
`rollback()` — both live on `TransactionSession`, which has to be in scope. `begin_with_config`
takes an isolation level and an access mode. A nested `begin()` opens a savepoint, not a second
transaction.

Most "no method named X found for struct Select" errors are a missing trait import, not a missing
API: `.filter` needs `QueryFilter`, `.lock` and `.column_as` need `QuerySelect`, `.order_by_asc`
needs `QueryOrder`, `.paginate` and `.count` need `PaginatorTrait`.

## Advisory locks

`pg_advisory_xact_lock(key)` serialises work across every replica that names the same key, and
Postgres releases it with the transaction — commit, rollback or a dropped connection — so there is
no unlock path to get wrong. The key is one `bigint`; `hashtext(..)::bigint` folds a string into
it (a collision over-serialises the blocking form and makes an unrelated try-lock skip), or derive
a stable `i64` in code.

The blocking form is a queue, not a gate: each waiter runs its own work once the holder commits.
That is right for "never two at once" and wrong for "once": a nightly job guarded by it runs once
per replica, one after another. Run-once work takes `pg_try_advisory_xact_lock`, which returns
`false` at once while another transaction holds the key, and records the run in the same
transaction, because a replica whose timer fires after the leader committed finds the lock free.

```rust,verify
use chrono::NaiveDate;
use sea_orm::{ConnectionTrait, DbErr, TransactionSession, TransactionTrait, raw_sql};

/// Blocks until no other transaction holds `key`, then holds it until `txn` ends.
/// The `TransactionSession` bound rejects the pool: there the lock would bind to
/// whichever connection ran the statement and vanish with its implicit commit.
pub async fn advisory_xact_lock<T>(txn: &T, key: &str) -> Result<(), DbErr>
where
    T: ConnectionTrait + TransactionSession,
{
    txn.execute_raw(raw_sql!(
        Postgres,
        r#"SELECT pg_advisory_xact_lock(hashtext({key})::bigint)"#
    ))
    .await?;
    Ok(())
}

/// `false` at once while another transaction holds `key`; `true` holds it until `txn` ends.
pub async fn try_advisory_xact_lock<T>(txn: &T, key: &str) -> Result<bool, DbErr>
where
    T: ConnectionTrait + TransactionSession,
{
    txn.query_one_raw(raw_sql!(
        Postgres,
        r#"SELECT pg_try_advisory_xact_lock(hashtext({key})::bigint)"#
    ))
    .await?
    .ok_or_else(|| DbErr::RecordNotFound("lock row".to_owned()))?
    .try_get_by_index(0)
}

/// One nightly run across N replicas; `false` means this replica skipped it. The
/// try-lock makes the others skip at once instead of queueing behind the leader,
/// and the `job_run` row (`PRIMARY KEY (job, run_date)`) stops a replica that
/// starts after the leader committed.
pub async fn rebuild_report<C: TransactionTrait>(
    db: &C,
    run_date: NaiveDate,
) -> Result<bool, DbErr> {
    let job = "rebuild_report";
    let txn = db.begin().await?;
    if !try_advisory_xact_lock(&txn, job).await? {
        txn.rollback().await?;
        return Ok(false);
    }
    let first_run = txn
        .execute_raw(raw_sql!(
            Postgres,
            r#"INSERT INTO "job_run" ("job", "run_date") VALUES ({job}, {run_date})
               ON CONFLICT DO NOTHING"#
        ))
        .await?
        .rows_affected()
        == 1;
    if !first_run {
        txn.rollback().await?;
        return Ok(false);
    }
    // ... the work, on `txn` ...
    txn.commit().await?;
    Ok(true)
}
```

`job_run` is an ordinary two-column table from a migration. Fire the timer more than once per
`run_date` (hourly for a nightly job): the replicas that got `false` do not retry, so a leader whose
work failed and rolled back leaves the date unrun until the next tick, and `job_run` makes the ticks
after a success no-ops. The same try-lock is how a test proves
the lock without a race: the second transaction cannot take it until the first commits.

```rust,verify,test
mod common;

use sea_orm::{
    ConnectionTrait, DatabaseTransaction, DbErr, TransactionSession, TransactionTrait, raw_sql,
};

async fn try_lock(txn: &DatabaseTransaction, key: &str) -> Result<bool, DbErr> {
    txn.query_one_raw(raw_sql!(
        Postgres,
        r#"SELECT pg_try_advisory_xact_lock(hashtext({key})::bigint)"#
    ))
    .await?
    .ok_or_else(|| DbErr::RecordNotFound("lock row".to_owned()))?
    .try_get_by_index(0)
}

#[tokio::test]
async fn second_transaction_waits_for_the_first() {
    let db = common::test_app().await.state.db;
    // Two `begin`s are two pooled connections, so two lock holders.
    let first = db.begin().await.unwrap();
    let second = db.begin().await.unwrap();

    assert!(try_lock(&first, "rebuild_report").await.unwrap());
    assert!(
        !try_lock(&second, "rebuild_report").await.unwrap(),
        "held by `first`"
    );

    first.commit().await.unwrap();
    assert!(
        try_lock(&second, "rebuild_report").await.unwrap(),
        "released with the commit"
    );
    second.rollback().await.unwrap();
}
```

An advisory lock fits work whose effect is a write to this Postgres database; when the effect is
anything else, `rust-redis` covers the lease that replaces it. In the database case the lock also
wins on mechanics: the release
is transactional for free, there is no lease TTL to size, and a crashed holder releases on
reconnect.

## Idempotency and the outbox

A redelivered message or a retried request must not do its work twice, and an event must be
neither published for work that rolled back nor lost for work that committed. Both come from
writing the record in the same transaction as the work.

**Idempotency.** The key is a row: the event's `event_id` for a consumer, the caller-scoped
`Idempotency-Key` for a request, in a table whose primary key or unique index is that key. Insert it
first, inside the transaction that does the work:

- inserted: this attempt owns the work; do it and commit;
- conflicted: an earlier attempt already committed the work, so this one is done — a consumer
  acks, a request answers as the idempotency contract says (`axum-service`);
- concurrent: a second attempt blocks on the unique index until the first commits, then sees the
  conflict, or inserts and proceeds if the first rolled back.

`on_conflict_do_nothing_on([..])` reports the conflict as `TryInsertResult::Conflicted` and leaves
the transaction usable. A plain insert reports it as 23505 (`is_duplicate` above), after which
Postgres has aborted the transaction: roll it back and treat the attempt as done. For a create, the
new row's own key, a uuid the producer minted, is already the idempotency key and needs no marker.

```rust,verify
use crate::entities::post;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ConnectionTrait, DbErr, TransactionSession,
    TransactionTrait, raw_sql,
};
use uuid::Uuid;

/// `processed_event` has `PRIMARY KEY (consumer, event_id)`, so two services can each
/// handle the same event once. `false`: an earlier delivery already committed. The
/// `TransactionSession` bound rejects the pool, where the marker would commit alone.
async fn first_delivery<T>(txn: &T, consumer: &str, event_id: Uuid) -> Result<bool, DbErr>
where
    T: ConnectionTrait + TransactionSession,
{
    let recorded = txn
        .execute_raw(raw_sql!(
            Postgres,
            r#"INSERT INTO "processed_event" ("consumer", "event_id")
               VALUES ({consumer}, {event_id}) ON CONFLICT DO NOTHING"#
        ))
        .await?;
    Ok(recorded.rows_affected() == 1)
}

/// Each `DraftRequested` event creates one post, however often it is delivered.
/// `Ok` on both branches, so the consumer acks once this returns.
pub async fn handle_draft_requested<C: TransactionTrait>(
    db: &C,
    event_id: Uuid,
    user_id: Uuid,
    title: &str,
) -> Result<(), DbErr> {
    let txn = db.begin().await?;
    if !first_delivery(&txn, "drafts", event_id).await? {
        return txn.rollback().await;
    }
    post::ActiveModel {
        id: Set(Uuid::now_v7()),
        user_id: Set(user_id),
        title: Set(title.to_owned()),
        published: Set(false),
        created_at: Set(chrono::Utc::now().into()),
    }
    .insert(&txn)
    .await?;
    txn.commit().await
}
```

With a generated `processed_event` entity the marker is a typed insert,
`.on_conflict_do_nothing_on([processed_event::Column::Consumer, processed_event::Column::EventId])`,
and `TryInsertResult::Conflicted` is the `false` branch.

**The outbox.** Publishing inside the transaction can announce work that then rolls back;
publishing after the commit loses the event when the process dies in between. Insert the event
into an `outbox` table in the same transaction as the state change instead. A relay task, in a
transaction of its own, selects unsent rows oldest first with `FOR UPDATE SKIP LOCKED`, so
replicas share the rows without sending one twice, publishes each with the row id as the broker's
deduplication id, marks it sent and commits. A crash between the publish and the commit sends the
row again: within the stream's `duplicate_window` the deduplication id absorbs it, and past that
the consumer's `event_id` row does. The publish itself is `rust-nats`'s.
