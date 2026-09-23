# rust-scaffolding

### Triggering

**Should load**

1. Create a service that stores books and lends them out, with a REST API.
2. Start me a new repo with sea-orm entities, sea-orm-migration migrations and nextest integration tests already wired.
3. This folder is empty. Turn it into an axum API I can deploy, with the tests and the pipeline passing before I write any features.
4. I ran `cargo run` in my new project directory and got `error: could not find 'Cargo.toml' in '/Users/me/notifications' or any parent directory`. I want to start a new API here.
5. We need a new microservice for notifications. Set it up from scratch, including the prek hooks and a pinned toolchain.

**Should not load**

1. Add an `/orders` route to my existing axum service. -> `axum-service`
2. Scaffold an `invoices` endpoint in our service, with the handler and its request and response types. -> `axum-service`
3. Add a GitHub Actions CI workflow and a Dockerfile to my existing axum service. -> `rust-tooling`
4. Add a `shared` library crate to our existing Cargo workspace, with the workspace lints applied. -> `rust-tooling`
5. Add prek hooks and a pinned toolchain to our existing service. -> `rust-tooling`
6. Set up integration tests for the `/orders` endpoints we are adding to our existing service. -> `rust-testing`
7. Add an `authors` table and entity to the service we scaffolded last week. -> `sea-orm-postgres`
8. Set up a migration and an entity for a new `invoices` table in our existing service. -> `sea-orm-postgres`
9. My axum service's Dockerfile and CI workflow are out of date. Can you scaffold fresh ones in from the template? -> `rust-tooling`

### Eval 1 - greenfield service from zero

**Prompt**: Create a new Rust service that stores books and exposes a REST API. I want tests and CI passing before we write any features.

**Must produce**:

- Copying the skill's `assets/app/`, dotfiles included, into a directory that does not exist yet, as the first step.
- No `target/` carried over from the template.
- `git init` in that same step, so prek's hook in `make install-tools` has a repository.
- The crate `app` renamed in the places the skill lists: the `Cargo.toml` name with `cargo update --workspace` for the lock, `use` paths, the OpenAPI title, the Dockerfile binary, the database name, the `RUST_LOG` directive and the compose project.
- `cargo fmt --all` after the rename.
- `.env` copied from `.env.example`, with an instruction to set `APP__AUTH__JWT_SECRET`.
- `make install-tools`.
- `docker compose up -d postgres`, then `make migrate`.
- `make check && make test` run before any feature code, a red result treated as a copy problem.
- A first commit that includes `Cargo.lock`.
- Replacing the `users` resource with `books` rather than adding a second resource beside it.

**Must not produce**:

- A `Cargo.toml`, `main.rs`, `Dockerfile` or CI workflow written from scratch instead of copied from `assets/app/`.
- A `cargo new` or `cargo init` starting point.
- A `.sh` file the user is told to save and run for the copy and rename.
- Entity files written by hand instead of `make migrate` followed by `make entity`.
- A claim that `.config/nextest.toml` has a database test group; it has none.

### Eval 2 - greenfield service with its first table

**Prompt**: Create a new service `library-svc` here with a `books` table (title, author, isbn) and CRUD endpoints.

**Fixture**: empty

**Must produce**:

- The template copied from `assets/app/`.
- `sea-orm-cli migrate generate create_books` to create the migration.
- The generated `todo!()` body rewritten in the style of the template's `create_users` or `create_posts` migration.
- `make migrate` then `make entity`.
- The `users` example replaced by `books`.

**Must not produce**:

- A hand-written file under `src/entities/`.
- `--from-entity`.

### Eval 3 - a second service inside a workspace

**Prompt**: Scaffold a second service, `billing`, at `services/billing/` inside this repository.

**Fixture**: scaffold

**Must produce**:

- A refusal to copy the template into `services/billing/`.
- The reason: the template's own `[workspace]` fails the parent workspace with `multiple workspace roots found`.

**Must not produce**:

- A `cp -R` of `assets/app` into `services/billing`.

### Probe 1 - the starting point

**Prompt**: Create a new axum and sea-orm service called `orders-svc` in this empty directory.

**Wrong answer**: `(?:^|\n)[ \t]*(?:\$\s*)?cargo\s+(?:new|init)\b`

**Right answer**: `^(?=[\s\S]*assets/app)[\s\S]*\bcp\s+-R\b`

### Probe 2 - a migration from a hand-written entity

**Prompt**: Create a new service `library-svc`. I already wrote `src/entities/book.rs` by hand for its `books` table; generate the migration from that entity.

**Wrong answer**: `\.create_table_from_entity\s*\(\s*\w|sea-orm-cli\s+migrate\s+[^\n]*--(?:autogenerate|from-entity)|sea-orm-cli\s+migrate\s+diff\b`

**Right answer**: `make entity|sea-orm-cli generate entity`

### Probe 3 - the runtime image

**Prompt**: Create a new axum service `ledger-svc` and show me its Dockerfile runtime stage.

**Wrong answer**: `x86_64-unknown-linux-musl|FROM\s+\S*alpine\S*|FROM\s+gcr\.io/distroless/static|(?:^|\n)[ \t]*panic\s*=\s*"abort"`

**Right answer**: `FROM\s+debian:trixie-slim`

### Probe 4 - a second service inside this repository

**Prompt**: Scaffold a second service, `billing`, at `services/billing/` inside this repository.

**Fixture**: scaffold

**Wrong answer**: `cp\s+-R\b[^\n]*assets/app[^\n]*\s(?:\./)?services/billing`
