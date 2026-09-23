# sea-orm-postgres

### Triggering

**Should load**

1. "Add an endpoint that returns a user with all their posts — one batched query for the posts, not a query per row."
2. "Our integration tests start a Postgres container per test and the suite takes four minutes. How do I give every test its own clean database?"
3. "I need a migration to add a nullable `display_name` column to `users` and backfill it."
4. "`user.save(&db).await` fails with `RecordNotUpdated` on a brand-new user whose id I generate with `Uuid::now_v7()`."
5. "After upgrading sea-orm from 1.x, `Expr::col(Alias::new(\"name\")).like(\"%a%\")` fails with `no method named like found for enum Expr`."
6. "Creating a post for a user id that does not exist blows up with a foreign key violation. How should the service handle it?"
7. "Only one of our three replicas should rebuild the nightly report. Can Postgres do the locking instead of Redis?"

**Should not load**

1. "The `test_app` helper should swap in a fake clock and an httpmock base URL for the outbound client, and I want a `fake` and `bon` user factory." -> `rust-testing`
2. "Create a new invoicing service from scratch with migrations, tests and CI wired up." -> `rust-scaffolding`
3. "Two replicas both run the nightly job that pushes our price list to a supplier's HTTP API. Add a Redis lease so only one of them sends it." -> `rust-redis`
4. "My tests fail with `PoolTimedOut` since I added `#[tokio::test(start_paused = true)]`." -> `rust-testing`
5. "Document in the OpenAPI spec the 422 that `POST /posts` returns when the `user_id` in the body does not exist." -> `axum-service`
6. "Add `limit` and `offset` query parameters and a `Page<T>` envelope to `GET /users`." -> `axum-service`
7. "Cache the result of the user lookup query in Redis for five minutes." -> `rust-redis`
8. "Add a `postgres` service to docker compose and to the CI workflow so the integration tests have a database." -> `rust-tooling`

### Eval 1 - N plus 1 in a list endpoint

**Prompt**: "Our `list_users_with_posts` handler fetches a page of users, then calls `user.find_related(post::Entity).all(db)` for each one, and the endpoint gets slower the more users there are. Fix it."

**Must produce**:
- The loop replaced by one batched load: `user::Entity::load().with(post::Entity)`, or `load_many(post::Entity, db)` on the fetched page.
- A statement of the query count after the fix: a fixed number of statements, independent of row count.
- The loader's trait imported (`EntityLoaderTrait` or `LoaderTrait`).

**Must not produce**:
- `find_also_related` paginated over the to-many (LIMIT on joined rows).
- A join plus manual deduplication of parent rows.
- A cache or an index offered as the primary fix.

### Eval 2 - adding a column

**Prompt**: "Add an optional `display_name` to the users table and expose it on the user response."

**Must produce**:
- `sea-orm-cli migrate generate ...` as the way the migration file is created.
- A hand-written migration in the house style: `DeriveMigrationName`, `DeriveIden`, `Table::alter()` with a `string_null` column.
- `sea-orm-cli migrate up` (or `make migrate`) given as the step that applies it.
- `sea-orm-cli generate entity --entity-format dense …` (or `make entity`) given as how the entity changes.
- `UserResponse` gains `display_name: Option<String>`.

**Must not produce**:
- Any `--autogenerate`, `diff` or `--from-entity` flag.
- `sync` or the Schema Registry offered as the migration mechanism.
- An edit to `src/entities/user.rs` as the first step, with the migration inferred afterwards.
- A hand edit of `src/entities/user.rs`, or one offered as an alternative to regenerating.

### Eval 3 - a missing parent on create

**Prompt**: "Add two ways to create a post (sea-orm, Postgres): `POST /users/{user_id}/posts`, where a user that does not exist must be a 404, and `POST /posts`, which takes `user_id` in the JSON body. Handle a user that does not exist on both."

**Must produce**:
- The post inserted first with the foreign key as the enforcement point: 23503 handled after the insert, not predicted before it.
- `SqlErr::ForeignKeyConstraintViolation` (23503) recognised through `DbErr::sql_err()`, not through the error text.
- On `POST /users/{user_id}/posts`, 23503 classified by the service to `AppError::NotFound`, with a message such as `format!("user {user_id} not found")`.
- On `POST /posts`, 23503 left as the bare `DbErr` for the existing 23503 arm in `AppError` (422, constant `"invalid reference"`), not classified in the service.
- The HTTP status of each variant attributed to `axum-service`.

**Must not produce**:
- A pre-flight `SELECT` to check the user exists, as the sole mechanism.
- A match on `DbErr::Query(RuntimeErr::SqlxError(..))` or on the Postgres error message text.
- The body-supplied `user_id` classified to `AppError::NotFound`.
- A second 23503 arm added to `AppError`, or the existing one changed.
- A new `AppError` variant for the violation.
- `.unwrap()` or `.expect()` in the service function.
- A new service function taking `&DatabaseConnection`.

### Eval 4 - test database setup

**Prompt**: "Set up database tests for our sea-orm service (it has a `migration` crate and no tests yet). We run cargo-nextest and Docker is available."

**Fixture**: empty

**Must produce**:
- One database per test: `CREATE DATABASE test_<uuid>` followed by `Migrator::up`.
- A server resolved from `TEST_DATABASE_URL` when set.
- Otherwise a testcontainers Postgres pinned with `with_tag("18-alpine")`.
- `with_container_name("rust-powers-test-postgres")` plus `ReuseDirective::Always` on the fallback container.
- The nextest process-per-test model given as the reason a `static` container is not enough.
- `testcontainers` with `reusable-containers` as a dev-dependency next to `testcontainers-modules` with `postgres`.
- A retry around `start()` for the cold-start 409 race, and for that error only.
- A retry around the first connection.
- The helper placed in `tests/common/mod.rs` as the database half of `test_app()`.
- `clippy::unwrap_used` and `clippy::expect_used` allowed in that helper file with a reason.
- `docker rm -f rust-powers-test-postgres` named as the recovery for a wedged reused container.

**Must not produce**:
- `MockDatabase` or SQLite as a substitute for Postgres.
- A transaction-rollback fixture presented as the isolation mechanism.
- A bare `static OnceCell` container with no reuse directive.
- A `make test` step that starts compose.
- A `test_db()` helper presented as something `test_app()` wraps.

### Eval 5 - writes and the save trap

**Prompt**: "Write `create_user` and `rename_user` service functions."

**Must produce**:
- `create_user` building an `ActiveModel` struct literal with `id: Set(Uuid::now_v7())` and calling `.insert(db)`.
- `rename_user` converting the fetched `Model` into an `ActiveModel`, assigning only `name`, and calling `.update(db)`.
- Both signatures generic over `&C where C: ConnectionTrait`.

**Must not produce**:
- `.save(db)` on a new row.
- `&DatabaseConnection` as the parameter type.
- `..Default::default()` in the `ActiveModel` literal.

### Eval 6 - a nightly job on several replicas

**Prompt**: "The nightly `rebuild_report` job runs on all three replicas. Use Postgres so it runs once per night, and only once."

**Must produce**:
- `pg_try_advisory_xact_lock` taken inside a transaction, with the run skipped when it returns `false`.
- A row unique on the job and the run date, inserted in the same transaction, so a replica that starts after the leader committed skips too.
- The lock released by the commit or rollback, with no manual unlock.

**Must not produce**:
- The blocking `pg_advisory_xact_lock` presented as making the job run once.
- `pg_advisory_lock` with a manual `pg_advisory_unlock`.
- The lock taken on the pool instead of a `DatabaseTransaction`.

### Probe 1 - save on a new row

**Prompt**: "Write `create_post(db, user_id, title)` with sea-orm; the id is `Uuid::now_v7()` generated in Rust."

**Wrong answer**: `\.save\(\s*&?\w+\s*\)\s*\.await`

**Right answer**: `\.insert\(\s*&?\w+\s*\)`

### Probe 2 - Expr methods without ExprTrait

**Prompt**: "`Expr::col(Alias::new(\"name\")).like(\"%a%\")` stopped compiling after the sea-orm upgrade. Fix it."

**Wrong answer**: `^(?![\s\S]*ExprTrait)[\s\S]*\.like\(`

**Right answer**: `ExprTrait`

### Probe 3 - autogenerated migrations

**Prompt**: "I added a `display_name` field to my sea-orm user entity. Generate the migration for it."

**Wrong answer**: `sea-orm-cli\s+migrate\s+(?:generate\s+\S+\s+--autogenerate|diff)|get_schema_registry\([^)]*\)\s*\.sync\(`

**Right answer**: `sea-orm-cli\s+migrate\s+generate`

### Probe 4 - paginating a joined to-many

**Prompt**: "With sea-orm, list users together with their posts, 20 users per page."

**Wrong answer**: `find_(?:also|with)_related\([^)]*\)\s*(?:\.\w+\([^()]*(?:\([^()]*\)[^()]*)*\)\s*)*\.paginate\(`

### Probe 5 - run once across replicas

**Prompt**: "Our nightly report job runs on three replicas. Use Postgres so it runs once per night."

**Wrong answer**: `^(?![\s\S]*pg_try_advisory)[\s\S]*"\s*SELECT\s+pg_advisory(?:_xact)?_lock\(`

**Right answer**: `pg_try_advisory_xact_lock`
