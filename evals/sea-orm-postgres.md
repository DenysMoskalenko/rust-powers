# sea-orm-postgres

### Triggering

**Should load**

1. "Add an endpoint that returns a user with all their posts — I want the selectinload equivalent, not a query per row."
2. "My integration tests are starting a Postgres container per test and taking four minutes. How do I set up per-test database isolation?"
3. "I need an alembic-style migration to add a nullable `display_name` column to `users` and backfill it."
4. "Insert fails with `DbErr::RecordNotUpdated` and the row is definitely new. The primary key is a uuid I generate in Rust."
5. "error[E0599]: no method named `like` found for enum `Expr` in the current scope"

**Should not load**

1. "My handler does not compile: the trait bound `fn(State, Json) -> ...: Handler` is not satisfied." -> `axum-service`
2. "Write the OpenAPI annotations and the response DTO for the users list endpoint." -> `axum-service`
3. "The `test_app` helper should swap in a fake clock and an httpmock base URL for the outbound client, and I want a polyfactory-style user factory." -> `rust-testing`
4. "Create a new invoicing service from scratch with migrations, tests and CI wired up." -> `rust-scaffolding`
5. "Spans from this service never show up in the collector even though the OTLP endpoint is set." -> `axum-service`

### Eval 1 - N plus 1 in a list endpoint

**Prompt**: "`list_users_with_posts` calls `find_related` inside a loop over the users it just fetched, and the endpoint gets slower the more users there are. Fix it."

**Must produce**:
- The loop removed, replaced by `user::Entity::load().with(post::Entity)` or by `users.load_many(post::Entity, db)` zipped with the parents.
- A statement of the query count after the fix (two statements, independent of row count).
- `EntityLoaderTrait` or `LoaderTrait` imported as appropriate.

**Must not produce**:
- `find_also_related` paginated over the to-many (LIMIT on joined rows).
- A join plus manual deduplication of parent rows.
- A suggestion to cache results or add an index as the primary fix.

### Eval 2 - adding a column

**Prompt**: "Add an optional `display_name` to the users table and expose it on the user response."

**Must produce**:
- `sea-orm-cli migrate generate ...` followed by a hand-written migration in the house style (`DeriveMigrationName`, `DeriveIden`, `Table::alter()` with a `string_null` column), the generated `todo!()` body replaced.
- `sea-orm-cli migrate up` (with `DATABASE_URL` set), then regeneration with `sea-orm-cli generate entity --entity-format dense -o src/entities --with-serde both`.
- The entity file treated as generated output, not hand-edited.
- A handoff to `axum-service` for the response DTO.

**Must not produce**:
- Any `--autogenerate`, `diff` or `--from-entity` flag.
- `sync` or the Schema Registry offered as the migration mechanism.
- An edit to `src/entities/user.rs` as the first step, with the migration inferred afterwards.

### Eval 3 - constraint violations on create

**Prompt**: "`POST /users` panics on a duplicate email, and `POST /posts` returns a 500 when the `user_id` does not exist. Make both return sensible errors."

**Must produce**:
- `DbErr::sql_err()` matched against `SqlErr::UniqueConstraintViolation` (23505) and `SqlErr::ForeignKeyConstraintViolation` (23503), in classifier functions like `is_duplicate` / `is_missing_reference`.
- The constraints left as the enforcement point: insert first, classify the error after.
- 23505 mapped to `AppError::Conflict` with a constant, PII-free message; 23503 mapped to `AppError::NotFound(format!("user {user_id} not found"))` (or `BadRequest`, with the choice stated).
- A handoff to `axum-service` for the HTTP status each variant renders as.

**Must not produce**:
- A pre-flight `SELECT` to check the email is free or the user exists, as the sole mechanism.
- A match on `DbErr::Query(RuntimeErr::SqlxError(..))` or on the Postgres error message text.
- The email address interpolated into the `Conflict` string (logged at ERROR).
- `.unwrap()` or `.expect()` in the service function.

### Eval 4 - test database setup

**Prompt**: "Set up database tests. We run cargo-nextest and Docker is available."

**Must produce**:
- One database per test: `CREATE DATABASE test_<uuid>` followed by `Migrator::up`.
- A server resolved from `TEST_DATABASE_URL` when set, otherwise a testcontainers Postgres pinned with `with_tag("18-alpine")`.
- `with_container_name("rust-powers-test-postgres")` plus `ReuseDirective::Always`, with the nextest process-per-test model given as the reason, and `testcontainers = { version = "0.27", features = ["reusable-containers"] }` as a dev-dependency next to `testcontainers-modules = { version = "0.15", features = ["postgres", "redis", "nats"] }`.
- A retry around `start()` for the cold-start 409 race (that error only), and a retry around the first connection.
- The helper placed in `tests/common/mod.rs` as the database half of `test_app()`, with `clippy::unwrap_used` and `clippy::expect_used` allowed with a reason.
- `docker rm -f rust-powers-test-postgres` named as the recovery for a wedged reused container.

**Must not produce**:
- `MockDatabase` or SQLite as a substitute for Postgres.
- A transaction-rollback fixture presented as the isolation mechanism.
- A bare `static OnceCell` container with no reuse directive.
- A `make test` step that starts compose, or a `test_db()` helper presented as something `test_app()` wraps.

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
