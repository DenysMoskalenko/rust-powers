# rust-tooling

### Triggering

**Should load**

1. Set up cargo-deny in this service — `cargo deny check` fails on the config `cargo deny init` wrote.
2. Our CI job recompiles the whole dependency tree in every step. Here is `.github/workflows/ci.yml`, what is wrong?
3. Set up the pre-commit hooks for this repo — what runs in them, and what goes in the Makefile?
4. `warning[E0602]: unknown lint: clippy::string_to_string` — clippy prints this on every build but CI is green.
5. I want to raise our coverage gate from 80 to 85 percent. Where does that number live so local runs and CI agree?
6. How do I upgrade one dependency without `cargo update` moving everything else in the lock file?

**Should not load**

1. Clippy says `needless_pass_by_value` on this handler argument — how do I fix it? -> `rust-code-style`
2. Write an rstest fixture that gives every test its own database. -> `sea-orm-postgres`
3. Create a new service from scratch with migrations, tests and CI green on the first commit. -> `rust-scaffolding`
4. Which files should the coverage number leave out, and why? -> `rust-testing`
5. How do I run just the `creates_user` test with nextest? -> `rust-testing`
6. CI rejects `#[allow(clippy::cast_possible_truncation)]` on this `as u32`. What should the code do instead? -> `rust-code-style`
7. Regenerate our entities in the dense format the new sea-orm uses. -> `sea-orm-postgres`
8. A panicking handler should answer 500 instead of dropping the connection. Where is that wired? -> `axum-service`

### Eval 1 - failing cargo deny check

**Prompt**: `cargo deny check` fails with `error[unlicensed]` for our own `service` and `migration` crates, and with `wildcards` on `migration = { path = "migration" }`. Fix it.

**Fixture**: empty

**Must produce**:

- `publish = false` added to both `[package]` tables, named as the actual fix.
- The explanation that `[licenses.private] ignore = true` only applies to crates with `publish = false`.
- The explanation that `allow-wildcard-paths` does not apply to a publishable crate.
- `wildcards = "deny"` kept.
- `allow-wildcard-paths = true`, covering the `migration` path dependency.

**Must not produce**:

- `cargo deny init` as the remedy.
- An empty `allow = []`.
- `multiple-versions = "deny"`.
- A `reason` field inside `[[licenses.exceptions]]`.
- Advice to skip the licenses check.
- Advice to pass `--no-deps`.

### Eval 2 - CI that rebuilds everything

**Prompt**: Our workflow sets `RUSTFLAGS: -Dwarnings` at the job level and every step recompiles from scratch even though `Swatinem/rust-cache` is enabled. Fix the workflow.

**Fixture**: empty

**Must produce**:

- The job-level `RUSTFLAGS` removed.
- `-D warnings` passed to the clippy invocation instead.
- The first cause: cargo's fingerprint includes the effective rustflags.
- The second cause: rust-cache hashes `RUST*` environment variables into the cache key.
- A separate `shared-key` for the job that needs `RUSTDOCFLAGS`.
- `--workspace` kept on clippy, nextest, doctests and `cargo doc`.
- A `cargo machete` step, so CI matches the Makefile's `check` target.

**Must not produce**:

- Disabling the cache.
- Adding `cargo clean`.
- Moving `-D warnings` into `.cargo/config.toml` `[build] rustflags`, which reintroduces the same fingerprint problem for every command.

### Eval 3 - bumping a dependency

**Prompt**: Bump `sea-orm` to its newest release within the current major version in this repo.

**Must produce**:

- `cargo update -p sea-orm` (more `-p` allowed) with `--dry-run` first, then without it.
- `make lint` and `make test` after it.
- `Cargo.lock` committed, alone when the existing requirement already allows the new version.
- A note to read the changelog for every version crossed.

**Must not produce**:

- A version typed into `Cargo.toml` by hand (`version = "2.0.N"`) instead of `cargo add sea-orm@…`.
- `cargo update` with no package argument as the way to bump one crate.
- Deleting `Cargo.lock` and regenerating it.

### Eval 4 - Docker layer caching

**Prompt**: Our Dockerfile rebuilds every dependency whenever we touch a source file. It uses `FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef`, runs `COPY . .` before `cargo chef cook --release` in the builder, and ends in `debian:bookworm-slim`. Fix it.

**Fixture**: empty

**Must produce**:

- chef, planner, builder and runtime stages in that order.
- `cargo chef cook` before `COPY . .` in the builder stage.
- The same toolchain name, patch version included, in the chef image tag and in `rust-toolchain.toml` (`latest-rust-1.98.1` and `channel = "1.98.1"`).
- A `debian:trixie-slim` runtime stage with `ca-certificates` installed.
- A non-root user in the runtime stage.
- `ENV APP__SERVER__HOST=0.0.0.0` in the runtime stage.
- `--bin` naming the binary after the `[package] name`.

**Must not produce**:

- `panic = "abort"` in `[profile.release]`: `CatchPanicLayer` then has nothing to catch, and one panicking request takes the whole process down.
- A musl target.
- An Alpine runtime image.
- `tests/` added to `.dockerignore`.

### Eval 5 - lints that skip the migration crate

**Prompt**: `cargo clippy --workspace -- -D warnings` is green, but `migration/src/main.rs` has an `.unwrap()` and a bare `#[allow(clippy::todo)]` with no reason. Why does nothing fire?

**Fixture**: empty

**Must produce**:

- The lint levels located in the root `Cargo.toml`'s `[workspace.lints.*]` tables, not in `migration/Cargo.toml`.
- `[lints] workspace = true` in every member, the root package included, because a member without it inherits nothing.
- The check afterwards: `cargo clippy --workspace --all-targets --all-features -- -D warnings` now fails on both findings.

**Must not produce**:

- Copying the lint tables into `migration/Cargo.toml`.
- `#![deny(...)]` or `#![warn(...)]` attributes in `migration/src/lib.rs`.
- A `RUSTFLAGS`-based fix.

### Eval 6 - target/ eats the disk

**Prompt**: `target/` in this service is 22 GB. Cargo has a max-size option, doesn't it? Configure it.

**Must produce**:

- A statement that no Cargo setting caps `target/`; Cargo's own cleanup (the nightly `cargo clean gc` size limits, stable's age-based auto-clean) covers only `~/.cargo`.
- `[profile.dev.package."*"] debug = false` in the root `Cargo.toml`.
- The note that the service's own crates keep full debuginfo.
- `cargo clean` as the way to reclaim what is already there.

**Must not produce**:

- `-Zgc` or `cargo clean gc` flags on the pinned stable toolchain.
- A `[profile.*]` table in `migration/Cargo.toml`.
- Disabling `Swatinem/rust-cache`.
- Adding `cargo clean` to CI.

### Probe 1 - grouping imports

**Prompt**: Make rustfmt group our imports into std, external crates, then our own crate.

**Wrong answer**: `^(?![\s\S]*\bnightly\b)[\s\S]*(?:^|\n)[ \t]*(?:group_imports|imports_granularity)\s*=`

### Probe 2 - fatal warnings in CI

**Prompt**: Make warnings fatal in our GitHub Actions workflow without making every step rebuild from scratch.

**Fixture**: empty

**Wrong answer**: `(?:^|\n)[ \t]*RUSTFLAGS\s*:\s*["']?-D\s?warnings`

**Right answer**: `clippy[^\n]*--\s+-D\s?warnings`

### Probe 3 - the pedantic group

**Prompt**: Our workspace has no lint table yet. Turn on clippy's pedantic group for every member, but allow `module_name_repetitions`.

**Fixture**: empty

**Wrong answer**: `(?:^|\n)[ \t]*pedantic\s*=\s*"warn"`

**Right answer**: `pedantic\s*=\s*\{\s*level\s*=\s*"warn"\s*,\s*priority\s*=\s*-1`

### Probe 4 - pinning the toolchain

**Prompt**: Our Dockerfile builds from `lukemathwalker/cargo-chef:latest-rust-1.98.1`. Write the `rust-toolchain.toml` so local builds, CI and Docker use the same compiler.

**Fixture**: empty

**Wrong answer**: `(?:^|\n)[ \t]*channel\s*=\s*"(?:1\.98|stable)"`

**Right answer**: `channel\s*=\s*"1\.98\.1"`

### Probe 5 - our own crates flagged unlicensed

**Prompt**: `cargo deny check` fails with `unlicensed` on our own two crates. Fix `deny.toml`.

**Fixture**: empty

**Wrong answer**: `(?:^|\n)[ \t]*(?:unlicensed|copyleft|allow-osi-fsf-free|default)\s*=|(?:^|\n)[ \t]*(?:\$\s*)?cargo deny init\b`

**Right answer**: `publish\s*=\s*false`
