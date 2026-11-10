# Relation loading and N+1

- [Choosing a strategy](#choosing-a-strategy)
- [Entity loader](#entity-loader)
- [LoaderTrait](#loadertrait)
- [find_with_related and find_also_related](#find_with_related-and-find_also_related)
- [has_related for EXISTS filters](#has_related-for-exists-filters)
- [Custom joins](#custom-joins)
- [Diagnosing N plus 1](#diagnosing-n-plus-1)

## Choosing a strategy

| API | Queries | SQL | Use when |
|---|---|---|---|
| `Entity::load().with(..)` | one per level | join for to-one, `IN (..)` batch for to-many | the default for nested reads |
| `LoaderTrait::load_many` / `load_one` | two | `WHERE fk IN (..)` | the parent list came from somewhere else, or plain `Model`s are wanted |
| `find_with_related` | one | LEFT JOIN, consolidated by parent | one parent, or a small fixed set |
| `find_also_related` | one | LEFT JOIN, flat tuples | a to-one on a single fetch |

A missed eager load is not an error. An unloaded relation is a value:
`HasMany<E>`, `HasOne<E>` and `BelongsTo<E>` are all `Loaded(..)` or `Unloaded`, two variants. A
nullable foreign key generates `BelongsTo<Option<E>>`, where a loaded-but-absent parent is
`Loaded(None)`; `is_not_found()` and `is_unloaded_or_not_found()` are the predicates for it. Checking
`is_unloaded()` is the reader's job — nothing at runtime shouts, which is exactly why the rule
below is worth keeping: every relation a response touches is loaded in the query that fetched
the parent.

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

/// `None`: the posts were never loaded, which is not the same as having none.
pub fn post_count(found: &user::ModelEx) -> Option<usize> {
    // `posts` is HasMany<post::Entity>. An Unloaded relation has `len()` 0 and iterates
    // as empty, so without this check "never loaded" reads as "no posts".
    found.posts.is_loaded().then(|| found.posts.len())
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
use sea_orm::{ConnectionTrait, DbErr, EntityTrait};

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

To confirm it rather than guess, count the statements. `set_metric_callback` on a
`DatabaseConnection` is called once per statement that connection runs, with the SQL. Set it on a
clone of the test's connection: the clone shares the pool and the callback belongs to the clone
alone, so the count is this call's and nothing global is installed. That turns "is it N+1?" into
an assertion that survives refactors.

```rust,verify,test
use std::sync::{Arc, Mutex, PoisonError};

use app::entities::{post, user};
use sea_orm::{
    ActiveModelTrait as _, ActiveValue::{NotSet, Set}, ConnectionTrait, DbErr, EntityTrait,
    LoaderTrait as _,
};

use crate::common::{frozen_now, test_app};

/// The function under test: the parents, then one batched `IN` over the children.
async fn users_and_posts<C: ConnectionTrait>(
    db: &C,
) -> Result<Vec<(user::Model, Vec<post::Model>)>, DbErr> {
    let users = user::Entity::find().all(db).await?;
    let posts = users.load_many(post::Entity, db).await?;
    Ok(users.into_iter().zip(posts).collect())
}

#[tokio::test]
async fn load_many_issues_two_statements_whatever_the_user_count() {
    let app = test_app().await;
    for i in 0..5 {
        let owner = user::ActiveModel {
            id: NotSet,
            email: Set(format!("user{i}@example.com")),
            name: Set("User".to_owned()),
            created_at: Set(frozen_now().into()),
        }
        .insert(&app.state.db)
        .await
        .unwrap();
        for j in 0..2 {
            post::ActiveModel {
                id: NotSet,
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
    let statements = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&statements);
    // The same pool; the callback is set on this clone only.
    let mut db = app.state.db.clone();
    db.set_metric_callback(move |info| {
        seen.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(info.statement.sql.clone());
    });

    let rows = users_and_posts(&db).await.unwrap();

    assert_eq!(rows.len(), 5);
    assert!(rows.iter().all(|(_, posts)| posts.len() == 2));
    let statements = statements.lock().unwrap().clone();
    assert_eq!(statements.len(), 2, "statements: {statements:#?}");
}
```

The same callback over the naive loop counts six. In a running service the answer is in the
log instead: with `sqlx_logging` on, a request that logs the same
`SELECT ... WHERE "user_id" = $1` fifty times is N+1, and the `tracing-spans` feature with
`record_stmt_in_spans(true)` attaches each SQL to the surrounding span. A `tracing` subscriber
is the wrong counter in a test: `set_default` is per thread and `tracing` caches whether a call
site is enabled, so under `cargo test`, where tests share a process, a thread with no subscriber
can get the call site cached as disabled and the scoped subscriber then misses its events.

Checking a plan does not need a first-class API: build the statement with `.into_query()`, render it
with `db.get_database_backend().build(&query)`, and run `EXPLAIN` over the result through
`query_all_raw`.
