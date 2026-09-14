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

The service picks the domain outcome — `Conflict` for a duplicate, `NotFound` for a missing parent
(a body-supplied id that references nothing is closer to "not found" than to "malformed"; a service
that disagrees maps it to `BadRequest`). Which HTTP status each `AppError` variant renders as, and
the default the `Db` arm applies when a service returns a bare `DbErr`, are `axum-service`'s.
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
it (a collision only over-serialises), or derive a stable `i64` in code.

```rust,verify
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

/// One nightly job across N replicas: the others block until the leader commits.
pub async fn rebuild_report<C: TransactionTrait>(db: &C) -> Result<(), DbErr> {
    let txn = db.begin().await?;
    advisory_xact_lock(&txn, "rebuild_report").await?;
    // ... the work, on `txn` ...
    txn.commit().await
}
```

`pg_try_advisory_xact_lock` is the non-blocking form: it returns `false` at once when another
transaction holds the key, for "skip this run if one is already going". It is also how a test
proves the lock without a race: the second transaction cannot take it until the first commits.

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

Prefer it over a Redis lease when the guarded work is itself in this database: the release is
transactional for free, there is no lease TTL to size, and a crashed holder releases on
reconnect. Reach for a Redis lease (`rust-redis`) when the critical section spans services or
protects non-database work — an HTTP call, a file, a queue — or must outlive one transaction.
