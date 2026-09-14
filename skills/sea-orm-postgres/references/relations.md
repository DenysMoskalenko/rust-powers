# Relation loading and N+1

- [Choosing a strategy](#choosing-a-strategy)
- [Entity loader](#entity-loader)
- [LoaderTrait](#loadertrait)
- [find_with_related and find_also_related](#find_with_related-and-find_also_related)
- [has_related for EXISTS filters](#has_related-for-exists-filters)
- [Custom joins](#custom-joins)
- [Diagnosing N plus 1](#diagnosing-n-plus-1)

## Choosing a strategy

| API | Queries | SQL | Use when | Python analog |
|---|---|---|---|---|
| `Entity::load().with(..)` | one per level | join for to-one, `IN (..)` batch for to-many | the default for nested reads | `selectinload` chained |
| `LoaderTrait::load_many` / `load_one` | two | `WHERE fk IN (..)` | the parent list came from somewhere else, or plain `Model`s are wanted | `selectinload` |
| `find_with_related` | one | LEFT JOIN, consolidated by parent | one parent, or a small fixed set | `joinedload` on a collection |
| `find_also_related` | one | LEFT JOIN, flat tuples | a to-one on a single fetch | `joinedload` on a to-one |

Unlike SQLAlchemy, a missed eager load is not an exception. An unloaded relation is a value:
`HasMany<E>`, `HasOne<E>` and `BelongsTo<E>` are all `Loaded(..)` or `Unloaded`, two variants. A
nullable foreign key generates `BelongsTo<Option<E>>`, where a loaded-but-absent parent is
`Loaded(None)`; `is_not_found()` and `is_unloaded_or_not_found()` are the predicates for it. Checking
`is_unloaded()` is the equivalent of `lazy='raise'`, except it is the reader's job to look rather
than the runtime's job to shout — which is exactly why the rule below is worth keeping: every
relation a response touches is loaded in the query that fetched the parent.

## Entity loader

`Entity::load()` is the 2.0 default. It decides per relation whether to join or batch, and it takes
nested paths in one call.

```rust,verify
use crate::entities::{post, user};
use sea_orm::{ConnectionTrait, DbErr, EntityLoaderTrait};
use uuid::Uuid;

// Two statements, whatever the row counts:
//   SELECT ... FROM "user" WHERE "user"."id" = $1 LIMIT $2
//   SELECT ... FROM "post" WHERE ("post"."user_id") IN (($1)) ORDER BY "post"."id" ASC
pub async fn user_with_posts<C: ConnectionTrait>(
    db: &C,
    user_id: Uuid,
) -> Result<Option<user::ModelEx>, DbErr> {
    user::Entity::load()
        .filter_by_id(user_id)
        .with(post::Entity)
        .one(db)
        .await
}

pub async fn all_users_with_posts<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<user::ModelEx>, DbErr> {
    // Still two statements for the whole list — this is the shape to reach for by default.
    user::Entity::load().with(post::Entity).all(db).await
}

pub fn post_count(found: &user::ModelEx) -> usize {
    // `posts` is HasMany<post::Entity>; iterating an Unloaded relation yields nothing,
    // so check explicitly when "no posts" and "never loaded" must be told apart.
    if found.posts.is_unloaded() {
        return 0;
    }
    found.posts.len()
}
```

The loader returns `ModelEx`, a `Model` whose relation fields carry the loaded children, rather than
`Model`. A sibling branch is a second `.with(..)` call; a nested path is a tuple,
`.with((post::Entity, comment::Entity))`, which loads user, then post, then comment in three
statements. `EntityLoaderTrait` has to be in scope or `filter_by_id` is not found.

`EntityLoaderTrait` also gives `order_by_id_*` and `paginate(db, size)`, so the loader covers
paginated nested reads too. `.filter()` and `.order_by_asc()` come from `QueryFilter` and
`QueryOrder`, which have to be imported as well; with only `EntityLoaderTrait` in scope, `.filter()`
resolves to `Iterator::filter` and the error says the loader is not an iterator.

## LoaderTrait

Use `load_many` and `load_one` when the parents came from an ordinary query, or when plain `Model`s
are wanted rather than `ModelEx`. The returned vector is index-aligned with the parents, so zip it;
do not look rows up by id.

```rust,verify
use crate::entities::{post, user};
use sea_orm::{ConnectionTrait, DbErr, EntityTrait, LoaderTrait};

// Two statements: the parents, then one batched IN over the children.
pub async fn users_and_posts<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<(user::Model, Vec<post::Model>)>, DbErr> {
    let users = user::Entity::find().all(db).await?;
    let posts = users.load_many(post::Entity, db).await?;
    Ok(users.into_iter().zip(posts).collect())
}

// The to-one direction, deduplicated: each distinct author is fetched once.
pub async fn posts_and_authors<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<(post::Model, Option<user::Model>)>, DbErr> {
    let posts = post::Entity::find().all(db).await?;
    let authors = posts.load_one(user::Entity, db).await?;
    Ok(posts.into_iter().zip(authors).collect())
}
```

`load_many` already resolves a many-to-many through its junction entity; `load_many_to_many` is kept
as legacy. `load_self` covers a self-reference.

## find_with_related and find_also_related

One LEFT JOIN. `find_with_related` consolidates rows by parent and yields
`Vec<(Model, Vec<Model>)>`; `find_also_related` leaves them flat as `Vec<(Model, Option<Model>)>`.

```rust,verify
use crate::entities::{post, user};
use sea_orm::{ConnectionTrait, DbErr, EntityTrait};
use uuid::Uuid;

// One parent, so the join has nothing to duplicate. Good fit.
pub async fn one_user_with_posts<C: ConnectionTrait>(
    db: &C,
    user_id: Uuid,
) -> Result<Option<(user::Model, Vec<post::Model>)>, DbErr> {
    Ok(user::Entity::find_by_id(user_id)
        .find_with_related(post::Entity)
        .all(db)
        .await?
        .into_iter()
        .next())
}
```

`find_with_related` has no `paginate` — `SelectTwoMany` deliberately omits it. `find_also_related`
does paginate, and over a to-many that is a trap: `LIMIT` applies to joined rows, not to parents, so
page two starts in the middle of a parent's children. Paginate the parents, then `load_many` their
children. The same join also re-transmits the parent's columns once per child row, which is why it
is a poor fit for a list of wide parents.

## has_related for EXISTS filters

Filtering parents by a predicate on their children is not a join problem.

```rust,verify
use crate::entities::{post, user};
use sea_orm::{ConnectionTrait, DbErr, EntityTrait, QueryFilter, sea_query::ExprTrait};

// SELECT ... FROM "user" WHERE EXISTS(SELECT 1 FROM "post" WHERE "post"."published" = $1
//                                     AND "user"."id" = "post"."user_id")
pub async fn users_with_a_published_post<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<user::Model>, DbErr> {
    user::Entity::find()
        .has_related(post::Entity, post::COLUMN.published.eq(true))
        .all(db)
        .await
}
```

A join would return one parent row per matching child and break any `LIMIT` applied to it.
`has_related` returns each parent once, works through a junction table for many-to-many, and
accepts a full `Condition`.

## Custom joins

When the generated ON condition is not enough, join explicitly. The relation variant is named after
the **target entity**, not the field: a field `posts: HasMany<post::Entity>` produces
`user::Relation::Post`, singular. Two fields pointing at the same entity need
`relation_enum = "..."` to disambiguate.

```rust,verify
use crate::entities::{post, user};
use sea_orm::{
    ConnectionTrait, DbErr, EntityTrait, QuerySelect, RelationTrait, query::JoinType,
};

pub async fn names_and_titles<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<(String, Option<String>)>, DbErr> {
    user::Entity::find()
        .join(JoinType::LeftJoin, user::Relation::Post.def())
        .select_only()
        .column(user::Column::Name)
        .column_as(post::Column::Title, "title")
        .into_tuple()
        .all(db)
        .await
}
```

`.def().on_condition(|_left, right| Expr::col((right, post::Column::Published)).eq(true).into())`
adds an extra ON predicate. `.into()` is the short form; `into_condition()` still exists and needs
`use sea_orm::sea_query::IntoCondition;`.

## Diagnosing N plus 1

The symptom is a handler whose latency grows with the number of rows it returns while each
individual statement looks fast. The cause is almost always a loop:

```rust,ignore
// One query for the list, then one more per row.
let users = user::Entity::find().all(db).await?;
for found in &users {
    let posts = found.find_related(post::Entity).all(db).await?;
}
```

The fix is to lift the child query out of the loop: `Entity::load().with(..)` for a nested read,
`load_many` when the parents are already in hand. `find_related` is correct only for a single parent
that was fetched by id.

To confirm it rather than guess, count the statements. sqlx 0.9 emits one `tracing` event per
statement, target `sqlx::query`, whenever `sqlx_logging` is on (sea-orm's default; the scaffold
turns it off, so the test opens a second pool with it on). A `Layer` that collects those events
turns "is it N+1?" into an assertion that survives refactors; a request that logs the same
`SELECT ... WHERE "user_id" = $1` fifty times has the answer in the log.

```rust,verify,test
mod common;

use std::sync::{Arc, Mutex};

use app::entities::{post, user};
use common::{frozen_now, test_app};
use sea_orm::{
    ActiveModelTrait as _, ActiveValue::Set, ConnectOptions, ConnectionTrait, Database, DbErr,
    EntityTrait, LoaderTrait as _,
};
use secrecy::ExposeSecret as _;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};
use uuid::Uuid;

/// The function under test: the parents, then one batched `IN` over the children.
async fn users_and_posts<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<(user::Model, Vec<post::Model>)>, DbErr> {
    let users = user::Entity::find().all(db).await?;
    let posts = users.load_many(post::Entity, db).await?;
    Ok(users.into_iter().zip(posts).collect())
}

/// Collects the `summary` field of every `sqlx::query` event: one per statement.
#[derive(Default, Clone)]
struct SqlLog(Arc<Mutex<Vec<String>>>);

struct Summary(String);

impl Visit for Summary {
    fn record_debug(&mut self, _: &Field, _: &dyn std::fmt::Debug) {}
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "summary" {
            value.clone_into(&mut self.0);
        }
    }
}

impl<S: tracing::Subscriber> Layer<S> for SqlLog {
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        if event.metadata().target() != "sqlx::query" {
            return;
        }
        let mut summary = Summary(String::new());
        event.record(&mut summary);
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(summary.0);
    }
}

#[tokio::test]
async fn load_many_issues_two_statements_whatever_the_user_count() {
    let app = test_app().await;
    for i in 0..5 {
        let owner = user::ActiveModel {
            id: Set(Uuid::now_v7()),
            email: Set(format!("user{i}@example.com")),
            name: Set("User".to_owned()),
            created_at: Set(frozen_now().into()),
        }
        .insert(&app.state.db)
        .await
        .unwrap();
        for j in 0..2 {
            post::ActiveModel {
                id: Set(Uuid::now_v7()),
                user_id: Set(owner.id),
                title: Set(format!("post {j}")),
                published: Set(true),
                created_at: Set(frozen_now().into()),
            }
            .insert(&app.state.db)
            .await
            .unwrap();
        }
    }
    // A second pool on the same database, statement logging on.
    let mut options = ConnectOptions::new(app.state.settings.database.url.expose_secret());
    options.sqlx_logging(true);
    let db = Database::connect(options).await.unwrap();

    let log = SqlLog::default();
    let guard = tracing::subscriber::set_default(tracing_subscriber::registry().with(log.clone()));
    let rows = users_and_posts(&db).await.unwrap();
    drop(guard);

    assert_eq!(rows.len(), 5);
    assert!(rows.iter().all(|(_, posts)| posts.len() == 2));
    let statements = log.0.lock().unwrap().clone();
    assert_eq!(statements.len(), 2, "statements: {statements:#?}");
}
```

The same layer over the naive loop counts six. The alternative is the `tracing-spans` feature
with `record_stmt_in_spans(true)`, which attaches each SQL to the surrounding span.

Checking a plan does not need a first-class API: build the statement with `.into_query()`, render it
with `db.get_database_backend().build(&query)`, and run `EXPLAIN` over the result through
`query_all_raw`.
