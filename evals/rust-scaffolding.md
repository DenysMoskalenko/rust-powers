# rust-scaffolding

### Triggering

**Should load**

1. Create a service that stores books and lends them out, with a REST API.
2. I'm porting a FastAPI app to Rust. Start me a new repo with SQLAlchemy-style models, alembic-style migrations and pytest-style tests already wired.
3. Bootstrap this empty directory into an axum service: migrations, tests, lints, Docker and CI green from the first commit.
4. I ran `cargo run` in my new project directory and got `error: could not find 'Cargo.toml' in '/Users/me/notifications' or any parent directory`. I want to start a new API here.
5. We need a new microservice for notifications. Set it up from scratch, including pre-commit hooks and a uv-style dependency workflow.

**Should not load**

1. Add an `/orders` route to my existing axum service. -> `axum-service`
2. `cargo deny check` fails on a CC0-1.0 licence in my repo. -> `rust-tooling`
3. My list endpoint runs one query per row. -> `sea-orm-postgres`
4. This integration test passes alone and fails under nextest. -> `rust-testing`
5. Spans never reach the OTLP collector. -> `axum-service`

### Eval 1 - greenfield service from zero

**Prompt**: Create a new Rust service that stores books and exposes a REST API. I want tests and CI passing before we write any features.

**Must produce**:

- Copying the skill's `assets/app/` (dotfiles included, no `target/` carried over) into a directory that does not exist yet as the first step, then renaming the crate `app` in the places the skill lists (crate name in `Cargo.toml` with `cargo update --workspace` for the lock, `use` paths, OpenAPI title, Dockerfile binary, database name, the `RUST_LOG` filter directive, compose project) and running `cargo fmt --all`.
- The `.env` step with `APP__AUTH__JWT_SECRET` (and `DATABASE_URL` for sea-orm-cli already present), `make install-tools` with `uv` and Docker named as prerequisites, `docker compose up -d postgres`, `make migrate`.
- `make check && make test` green before any feature code, and a first commit that includes `Cargo.lock`.
- Replacing the `users` resource with `books` rather than adding a second resource beside it.

**Must not produce**:

- A hand-written `Cargo.toml`, `main.rs`, `Dockerfile` or CI workflow.
- A `cargo new` or `cargo init` starting point, a hand-written file set instead of the copied template, or running a shell script to do the copy and rename.
- Entity files written by hand instead of `make migrate` followed by `make entity`.
- A claim that `.config/nextest.toml` has a database test group; it has none.

### Eval 2 - adding a table to a freshly scaffolded service

**Prompt**: The scaffold is running. Add an `author` table with a foreign key from `book`.

**Must produce**:

- A new migration file in `migration/src/` created with `sea-orm-cli migrate generate <name>` (or copied from the shipped `create_posts` migration and then registered by hand), its body rewritten in the house style of `create_posts` with no `todo!()` left, registered in `migration/src/lib.rs`, with an index on the foreign key column.
- `make migrate` then `make entity` to regenerate `src/entities/` from the live schema.
- A statement that sea-orm-cli has no autogenerate: the migration is written first, and codegen only points database to entities.
- A pointer to `sea-orm-postgres` for the query and relation work that follows.

**Must not produce**:

- Hand-edited files under `src/entities/`.
- An entity-first workflow, `--from-entity`, or a schema `diff` command.
- Re-running the scaffold to "add" the table.
- Leaving the generated `todo!()` body in place, or a migration file that is not registered in `migration/src/lib.rs`.

### Eval 3 - refusing to scaffold into an existing project

**Prompt**: My axum service is missing a Dockerfile and a CI workflow. Can you scaffold those in?

**Must produce**:

- A refusal to re-scaffold, and a hand-off to `rust-tooling` for adding or changing those files in an existing repository.

**Must not produce**:

- Copying the template over an existing tree.
- Overwriting an existing `Cargo.toml`, `Makefile` or workflow file.
