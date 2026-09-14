# rust-tooling

### Triggering

**Should load**

1. Set up cargo-deny in this service — `cargo deny check` fails on the config `cargo deny init` wrote.
2. Our CI job recompiles the whole dependency tree in every step. Here is `.github/workflows/ci.yml`, what is wrong?
3. We used pre-commit and ruff on the Python side; what is the equivalent hook setup here, and what goes in the Makefile?
4. `warning[E0602]: unknown lint: clippy::string_to_string` — clippy prints this on every build but CI is green.
5. Where does the coverage threshold live so `make cov`, the hook and CI all enforce the same number?

**Should not load**

1. Clippy says `needless_pass_by_value` on this handler argument — how do I fix it? -> `rust-code-style`
2. Write an rstest fixture that gives every test its own database. -> `sea-orm-postgres`
3. Create a new service from scratch with migrations, tests and CI green on the first commit. -> `rust-scaffolding`
4. My spans never reach the OTLP collector. -> `axum-service`
5. `Migrator::up` fails with `duplicate key value violates unique constraint`. -> `sea-orm-postgres`

### Eval 1 - failing cargo deny check

**Prompt**: `cargo deny check` fails with `error[unlicensed]` for our own `service` and `migration` crates,
and with `wildcards` on `migration = { path = "migration" }`. Fix it.

**Must produce**:

- `publish = false` added to both `[package]` tables, named as the actual fix.
- `[licenses.private] ignore = true` only applies to crates with `publish = false`, and `allow-wildcard-paths`
  does not apply to a publishable crate.
- A `deny.toml` matching `references/configs.md`, with `unmaintained` as a scope
  (`all` / `workspace` / `transitive` / `none`) rather than a lint level.

**Must not produce**:

- `cargo deny init` as the remedy, or an empty `allow = []`.
- `multiple-versions = "deny"`.
- A `reason` field inside `[[licenses.exceptions]]`.
- Advice to skip the licenses check or pass `--no-deps`.

### Eval 2 - CI that rebuilds everything

**Prompt**: Our workflow sets `RUSTFLAGS: -Dwarnings` at the job level and every step recompiles from scratch
even though `Swatinem/rust-cache` is enabled. Fix the workflow.

**Must produce**:

- Remove the job-level `RUSTFLAGS`; pass `-D warnings` to the clippy invocation instead.
- Both causes: cargo's fingerprint includes the effective rustflags, and rust-cache hashes `RUST*` env vars
  into the cache key.
- A separate `shared-key` for the job that needs `RUSTDOCFLAGS`.
- `--workspace` kept on clippy, nextest, doctests and `cargo doc`, and a `cargo machete` step so CI matches the
  Makefile's `check` target.

**Must not produce**:

- Disabling the cache, or adding `cargo clean`.
- Moving `-D warnings` into `.cargo/config.toml` `[build] rustflags`, which reintroduces the same fingerprint
  problem for every command.

### Eval 3 - bumping a dependency

**Prompt**: Bump `sea-orm` to the latest 2.x in this repo.

**Must produce**:

- `cargo update -p sea-orm --dry-run` first, then the real update.
- `make lint` and `make test` after it.
- `Cargo.lock` committed with the change, and a note to read the changelog for the versions crossed.

**Must not produce**:

- A hand-edited version string in `Cargo.toml`.
- `cargo update` with no package argument as the way to bump one crate.
- Deleting `Cargo.lock` and regenerating it.

### Eval 4 - Docker layer caching

**Prompt**: Our Dockerfile rebuilds every dependency whenever we touch a source file. Here it is, using
cargo-chef.

**Must produce**:

- chef, planner, builder and runtime stages in that order, with `cargo chef cook` before `COPY . .`.
- The same toolchain name, patch version included, in the chef image tag and in `rust-toolchain.toml`
  (`latest-rust-1.98.1` and `channel = "1.98.1"`).
- A non-root user and `ca-certificates` in a `debian:trixie-slim` runtime stage, `ENV APP__SERVER__HOST=0.0.0.0`,
  and `--bin app` matching the `[package] name`.

**Must not produce**:

- `panic = "abort"` in `[profile.release]`, or the claim that a panicking handler becomes a 500 (it drops the connection).
- A musl target or an Alpine runtime image.
- `tests/` added to `.dockerignore`.

### Eval 5 - lints that skip the migration crate

**Prompt**: `cargo clippy --workspace -- -D warnings` is green, but `migration/src/main.rs` has an `.unwrap()` and a bare `#[allow(clippy::todo)]` with no reason. Why does nothing fire?

**Must produce**:

- The lint tables belong under `[workspace.lints.rust]` / `[workspace.lints.clippy]` in the root `Cargo.toml`, and every member, the root package included, needs `[lints] workspace = true`; a member without it inherits nothing.
- The check afterwards: `cargo clippy --workspace --all-targets --all-features -- -D warnings` now fails on both findings.

**Must not produce**:

- Copying the lint tables into `migration/Cargo.toml`.
- `#![deny(...)]` or `#![warn(...)]` attributes in `migration/src/lib.rs`.
- A `RUSTFLAGS`-based fix.
