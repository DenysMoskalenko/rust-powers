---
name: rust-scaffolding
description: "Use when creating a brand-new Rust axum service from nothing — 'create a service that...', an empty directory with no Cargo.toml, a new API or microservice that needs migrations, tests, lints, Docker and CI green from the first commit, or a repo that still errors with could not find Cargo.toml. Greenfield only. Not for adding a route (axum-service), entity (sea-orm-postgres) or test (rust-testing) to an existing crate, nor changing tooling in an existing service (rust-tooling)."
metadata:
  version: "0.2.0"
---

# Rust Scaffolding (Greenfield Only)

Assumes Rust 1.98.1 edition 2024, axum 0.8, sea-orm 2.0, utoipa 5, cargo-nextest, Docker.

## Important

- Copy `assets/app/` and rename the crate by hand (step 2). Never `cargo new`, never a
  hand-written `Cargo.toml`, `main.rs`, `Dockerfile` or CI workflow: the template's value
  is the version interactions that already pass.
- Only where neither the destination nor any parent directory has a `Cargo.toml`: inside an
  existing workspace the template's own `[workspace]` fails the parent with `multiple workspace
  roots found`, or sits beside it unbuilt. An existing service is `rust-tooling`,
  `axum-service`, `sea-orm-postgres` and `rust-testing` territory.
- Migration first, then `make entity`. Never hand-edit `src/entities/`.
- The gate is `make check && make test`, green before any feature code. A red baseline is
  a copy problem, never a code problem.

## References

- `references/layout.md` — open when you need to know which file a change belongs in, or
  how a request travels through the tree.

## 1. Prerequisites

`rustup` (the template's `rust-toolchain.toml` fetches 1.98.1 on the first cargo command),
Docker and `git`; step 4 installs everything else.

## 2. Copy the template and rename the crate

`<name>` is a cargo package name (`orders-svc`, `notifications`: lowercase, digits, `-`,
`_`); `<snake>` is the same with `-` turned into `_`, which is the crate's library path.
The destination `<dest>` must not exist yet: this skill never copies over an existing tree.

```bash
mkdir <dest> && cp -R <this skill's directory>/assets/app/. <dest>/  # the /. keeps dotfiles
cd <dest> && rm -rf target
git init   # prek's hook (step 4) needs a repository, and it should guard the first commit
```

The template's crate is `app`. Rename it in exactly these places and nowhere else: `app`
is also a local variable in `main.rs` and the tests, the Dockerfile's user, and a word in the
comments of `.env.example` and `docker-compose.yml`; those stay.

| File | Replace |
| --- | --- |
| `Cargo.toml` | the one `name = "app"` line with `name = "<name>"`; then `cargo update --workspace` moves the `Cargo.lock` entry and touches nothing else |
| `src/main.rs`, `tests/common/mod.rs`, `tests/panic.rs` | every `app::` path prefix with `<snake>::` |
| `src/lib.rs` | `info(title = "app"` with `<name>` |
| `Dockerfile` | `--bin app`, `/build/target/release/app /usr/local/bin/app` and `CMD ["app"]` with `<name>` |
| `.env.example` | the trailing `/app` in both database URLs and the `app=` in `RUST_LOG` with `<snake>` |
| `docker-compose.yml` | `name: app` with `<name>`, `POSTGRES_DB: app` with `<snake>` |

Then `grep -rn 'app::' src tests` must print nothing, and run `cargo fmt --all`: renaming
reorders imports, and step 7 starts with `cargo fmt --all -- --check`.

## 3. Set the environment

```bash
cp .env.example .env
```

Set `APP__AUTH__JWT_SECRET`, and `APP__SERVER__CORS_ORIGINS` if a browser on another
origin will call this service. Every setting is an environment variable prefixed `APP__`,
with `__` between nesting levels, so `APP__SERVER__PORT` fills `settings.server.port`.
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
docker compose up -d postgres
make migrate
```

Set `POSTGRES_PORT` in `.env` first if 5432 is taken, and keep both URLs in step with it.
`main.rs` also runs `Migrator::up` at startup, which is right for one replica; `make migrate`
is what a deploy pipeline calls when several replicas would otherwise race.

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
doctests. The tests use `TEST_DATABASE_URL` when set and otherwise start one reusable
container named `rust-powers-test-postgres`, so a running compose stack and a bare machine
both work. If a run hangs at startup after a Docker restart, that container's data is
corrupt: `docker rm -f rust-powers-test-postgres` and run again.

## 8. First commit

```bash
git add -A && git commit -m "Scaffold service"
```

Step 2 already ran `git init`. `-A` includes `Cargo.lock`, which CI's `--locked` and the Docker build need.

## What to change first for a real domain

The template ships `users` and `posts` as a worked example of every layer. Replace them
rather than adding beside them.

1. **Rename the resource.** `git mv src/api/users.rs src/api/<resource>.rs`, update
   `src/api/mod.rs`, then rename `CreateUser`, `UserResponse` and the route paths. Keep
   `Page` as it is, so every list endpoint shares one wire shape.
2. **Add an entity.** Write the migration first: sea-orm-cli has no autogenerate and can
   only point database to entities. `sea-orm-cli migrate generate <name>` creates the
   timestamped file and registers it in `migration/src/lib.rs`; rewrite its body (`todo!()`
   fails `-D warnings`) in the style of `migration/src/m20260913_000002_create_posts.rs`.
   If you copied that file by hand instead, add the `mod` line and the `Box::new(..)` entry
   in `migration/src/lib.rs` yourself. Then `make migrate`, then `make entity`.
3. **Add a route.** Copy the shape of `src/api/users.rs` and register the handler with
   `routes!` in the module's `router()`; the OpenAPI document follows from the attribute.
4. **Delete what you do not use.** `posts` exists to show a foreign key and an index. If
   the domain has no second table, drop the entity, the migration and the `HasMany` field.

For handlers, extractors, errors, the readiness body, OpenAPI and telemetry see
`axum-service`; for queries, relations and migrations see `sea-orm-postgres`; for test
helpers, factories and fixtures see `rust-testing`; for any configuration file see
`rust-tooling`.
