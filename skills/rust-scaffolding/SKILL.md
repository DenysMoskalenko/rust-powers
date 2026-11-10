---
name: rust-scaffolding
description: "Use when creating a brand-new Rust axum service from nothing — 'create a service that...', an empty directory with no Cargo.toml, a new API or microservice that needs migrations, tests, lints, Docker and CI green from the first commit, or a repo that still errors with could not find Cargo.toml. Greenfield only. Not for adding a route (axum-service), entity (sea-orm-postgres) or test (rust-testing) to an existing crate, nor changing tooling in an existing service (rust-tooling)."
metadata:
  version: "0.8.0"
---

# Rust Scaffolding (Greenfield Only)

Assumes Rust 1.99.1 edition 2024, axum 0.8, sea-orm 2.0, utoipa 6, cargo-nextest, Docker.

## Important

- Copy `assets/app/` and rename the crate by hand (step 2). Never `cargo new`, never a
  hand-written `Cargo.toml`, `main.rs`, `Dockerfile` or CI workflow: the template's value
  is the version interactions that already pass.
- Only where neither the destination nor any parent directory has a `Cargo.toml`: inside an
  existing workspace the template's own `[workspace]` fails the parent with `multiple workspace
  roots found`, or sits beside it unbuilt. An existing service is `rust-tooling`,
  `axum-service`, `sea-orm-postgres` and `rust-testing` territory.
- Migration first, then `make entity`. Never hand-edit `src/entities/`.
- The gate is `make check && make test`, green before any feature code. A red baseline is a
  missed rename, the environment (no Docker daemon and no `TEST_DATABASE_URL`), or `cargo deny`
  reporting an advisory or a yank published after the template's lock file, which `rust-tooling`
  handles. Fix that cause; do not edit the template's code to get to green.

## References

- `references/layout.md` — open when you need to know which file a change belongs in, or
  how a request travels through the tree.

## 1. Prerequisites

`rustup` (the template's `rust-toolchain.toml` fetches 1.99.1 on the first cargo command),
Docker and `git`; step 4 installs everything else.

## 2. Copy the template and rename the crate

`<name>` is a cargo package name (`orders-svc`, `notifications`: lowercase, digits, `-`,
`_`); `<snake>` is the same with `-` turned into `_`, which is the crate's library path.
The destination `<dest>` is a directory that does not exist yet or an existing empty one, the
current directory included; a lone `.git/` counts as empty. This skill never copies over an
existing tree: if `ls -A <dest>` prints anything else, stop and ask.

```bash
mkdir -p <dest> && cp -R <this skill's directory>/assets/app/. <dest>/  # the /. keeps dotfiles
cd <dest> && rm -rf target
git init   # prek's hook (step 4) needs a repository, and it should guard the first commit
```

The template's crate is `app`. Rename it in exactly these places and nowhere else: `app`
is also a local variable in `main.rs` and the tests, the Dockerfile's user, and a word in the
comments of `.env.example` and `docker-compose.yml`; those stay.

| File | Replace |
| --- | --- |
| `Cargo.toml` | the one `name = "app"` line with `name = "<name>"`; then `cargo update --workspace` moves the `Cargo.lock` entry and touches nothing else |
| `src/main.rs`, `tests/api/common.rs`, `tests/api/panic.rs`, `tests/api/stack.rs` | every `app::` path prefix with `<snake>::` |
| `src/lib.rs` | `info(title = "app"` with `<name>` |
| `Dockerfile` | `--bin app`, `/build/target/release/app /usr/local/bin/app` and `CMD ["app"]` with `<name>` |
| `.env.example` | the trailing `/app` in both database URLs and the `app=` in `RUST_LOG` with `<snake>` |
| `docker-compose.yml` | `name: app` with `<name>`, `POSTGRES_DB: app` with `<snake>` |

Then `grep -rnE '\bapp::' src tests` must print nothing (the `\b` keeps a new name that ends in
`app`, such as `shop_app::`, from matching), and run `cargo fmt --all`: renaming reorders
imports, and step 7 starts with `cargo fmt --all -- --check`.

On Rust 1.99, `cargo update --workspace` also prints `warning: ignoring
registry.global-min-publish-age without -Zmin-publish-age`. That is expected: keep
`.cargo/config.toml`, which holds back crate versions under five days old from cargo 1.100.

## 3. Set the environment

```bash
cp .env.example .env
```

Set `APP__AUTH__JWT_SECRET`, and `APP__SERVER__CORS_ORIGINS` if a browser on another
origin will call this service: comma-separated, each written `scheme://host[:port]` with no
trailing slash or path, or the service refuses to start. Every setting is an environment
variable prefixed `APP__`, with `__` between nesting levels, so `APP__SERVER__PORT` fills
`settings.server.port`.
`DATABASE_URL` repeats the database URL because `sea-orm-cli` reads that name. `.env` is
git-ignored and loaded only in development; in production the missing file is a no-op.

## 4. Install the tools

```bash
make install-tools
```

Installs cargo-binstall, then nextest, llvm-cov, deny, machete, chef, bacon, insta,
sea-orm-cli and prek as prebuilt binaries, and prek's git hook.

## 5. Start Postgres and migrate

```bash
docker compose up -d --wait postgres
make migrate
```

`--wait` returns once the healthcheck passes; without it the first `make migrate` stalls on a
booting server. Set `POSTGRES_PORT` in `.env` first if 5432 is taken, and keep both URLs in step
with it. To use a Postgres that is already running instead, point both URLs at it and create the
`<snake>` database first: `make migrate` fails with `database "<snake>" does not exist`.
`main.rs` also runs `Migrator::up` at startup, before the service pool exists: on connections of
its own with no `statement_timeout`, under a Postgres advisory lock, so replicas that start
together migrate one at a time and a long backfill is not cancelled. A rolling deploy is what
that does not cover, because the old code still serves against the new schema: sequence a
breaking change as expand, migrate, contract, or run `make migrate` as a separate deploy step.

## 6. Run it

```bash
cargo run
```

Then `curl localhost:8000/health/live`, `curl localhost:8000/health/ready` (a JSON body:
`status` plus one entry per dependency under `checks`), `localhost:8000/docs` for Swagger
UI, `/openapi.json` for the document, `/metrics` for Prometheus. If something else already
listens on port 8000, set `APP__SERVER__PORT`.

## 7. Prove the gate is green

```bash
make check && make test
```

`check` is fmt, clippy `-D warnings`, cargo-deny and cargo-machete; `test` is nextest plus
doctests. Plain `cargo test` passes too, but nextest is what the gate and CI run. The tests use
`TEST_DATABASE_URL` when set and otherwise start one reusable container named
`rust-powers-test-postgres`, so a running compose stack and a bare machine both work. `make test` drops a passing run's test databases from that container, and any
older than an hour. A Docker restart needs nothing: the stopped container is started again on
reuse. If a run still hangs at startup, Postgres inside it cannot start:
`docker rm -f rust-powers-test-postgres` and run again.

## 8. First commit

```bash
git add -A && git commit -m "Scaffold service"
```

Step 2 already ran `git init`. `-A` includes `Cargo.lock`, which CI's `--locked` and the Docker build need.

## What to change first for a real domain

The template ships `users` and `posts` as a worked example of every layer. Replace them
rather than adding beside them.

1. **Rename the resource.** `git mv src/api/users.rs src/api/<resource>.rs` and
   `git mv tests/api/users.rs tests/api/<resource>.rs`, update `src/api/mod.rs` and the `mod`
   line in `tests/api/main.rs`, then rename `CreateUser`, `UserResponse`, `ListUsers`, the route paths, the `tag = "users"` attributes
   and the `users` tag in `src/lib.rs`. Keep `Page` as it is, so every list endpoint shares one
   wire shape; once a second resource lists, move it to `src/api/mod.rs`.
2. **Add an entity.** Write the migration first: sea-orm-cli has no autogenerate and can
   only point database to entities. `sea-orm-cli migrate generate <name>` creates the
   timestamped file and registers it in `migration/src/lib.rs`; replace the whole file, imports
   included, in the style of `migration/src/m20260913_000002_create_posts.rs`; the generated
   template fails `-D warnings` (`sea-orm-postgres` says which lines).
   If you copied that file by hand instead, add the `mod` line and the `Box::new(..)` entry
   in `migration/src/lib.rs` yourself. Then `make migrate`, then `make entity`.
3. **Add a route.** Copy the shape of `src/api/users.rs` and register the handler with
   `routes!` in the module's `router()`; the OpenAPI document follows from the attribute.
4. **Delete what you do not use.** `posts` exists to show a foreign key and an index. If
   the domain has no second table, delete `m20260913_000002_create_posts.rs` with its `mod`
   line and `Box::new(..)` entry, run `sea-orm-cli migrate fresh` (it drops every table of the
   development database), `make entity` and `cargo fmt --all`, then delete
   `src/entities/post.rs`: codegen rewrites `user.rs` without the `HasMany` field but never
   deletes a file.

For handlers, extractors, errors, the readiness body, OpenAPI and telemetry see
`axum-service`; for queries, relations and migrations see `sea-orm-postgres`; for test
helpers, factories and fixtures see `rust-testing`; for any configuration file see
`rust-tooling`.
