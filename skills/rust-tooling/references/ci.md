# GitHub Actions

One workflow, three jobs: `test` (format, lint, tests, coverage), `deny` (supply chain), `docs`. It runs the
same commands as the Makefile, so a green local run of the `check` and `test` targets predicts a green
pipeline.

## Contents

- [The workflow](#the-workflow)
- [The toolchain step](#the-toolchain-step)
- [The RUSTFLAGS trap](#the-rustflags-trap)
- [Postgres: service container, not testcontainers](#postgres-service-container-not-testcontainers)
  - [Optional services: Redis and NATS](#optional-services-redis-and-nats)
- [Action versions](#action-versions)

## The workflow

`.github/workflows/ci.yml`:

```yaml
name: CI
on: [push, pull_request]

env:
  CARGO_TERM_COLOR: always
  # RUSTFLAGS is deliberately not set: it changes cargo's fingerprint and the
  # rust-cache key, so every step would rebuild. `-D warnings` goes to clippy.

jobs:
  test:
    runs-on: ubuntu-latest
    services:
      postgres:
        image: postgres:18-alpine
        env:
          POSTGRES_USER: postgres
          POSTGRES_PASSWORD: postgres
          POSTGRES_DB: postgres
        ports:
          - 5432:5432
        options: >-
          --health-cmd "pg_isready -U postgres"
          --health-interval 10s
          --health-timeout 5s
          --health-retries 5
    env:
      TEST_DATABASE_URL: postgres://postgres:postgres@localhost:5432/postgres
    steps:
      - uses: actions/checkout@v7
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt, llvm-tools-preview

      - uses: Swatinem/rust-cache@v2
      - uses: taiki-e/install-action@v2
        with:
          tool: cargo-nextest,cargo-llvm-cov,cargo-machete

      - run: cargo fmt --all -- --check
      - run: cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
      - run: cargo machete

      - name: Tests and coverage
        run: cargo llvm-cov --no-report nextest --profile ci --workspace --all-features --locked

      - name: Doctests, which nextest cannot run
        run: cargo test --doc --workspace --all-features --locked

      - name: Coverage report and gate
        run: |
          cargo llvm-cov report --lcov --output-path target/lcov.info \
            --ignore-filename-regex '(entities|migration)/|main\.rs|telemetry\.rs'
          cargo llvm-cov report --summary-only --fail-under-lines 80 \
            --ignore-filename-regex '(entities|migration)/|main\.rs|telemetry\.rs'

      - name: Upload JUnit
        if: always()
        uses: actions/upload-artifact@v7
        with:
          name: junit
          path: target/nextest/ci/junit.xml

      - name: Upload coverage
        if: always()
        uses: actions/upload-artifact@v7
        with:
          name: lcov
          path: target/lcov.info

  deny:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
      - uses: EmbarkStudios/cargo-deny-action@v2
        with:
          command: check advisories bans licenses sources

  docs:
    runs-on: ubuntu-latest
    env:
      RUSTDOCFLAGS: -D warnings
    steps:
      - uses: actions/checkout@v7
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
        with:
          # A fixed key shared across runs of this job, which has its own
          # RUSTDOCFLAGS and therefore its own artifacts.
          shared-key: docs
      - run: cargo doc --no-deps --workspace --all-features --locked
```

The lcov file is uploaded as a workflow artifact; to publish it, append a `codecov/codecov-action@v7`
step with `files: target/lcov.info` and a `CODECOV_TOKEN` secret. Either way the `--fail-under-lines` gate is
the part that blocks a merge. Writing the file under `target/` keeps it covered by the existing `/target`
line in `.gitignore`; at the repository root it is one `git add .` away from being committed.

The lcov step runs **before** the gate: `--fail-under-lines` exits non-zero, and a failing step with no report
uploaded is the one case where you most want the report.

`--locked` on every cargo invocation makes a stale `Cargo.lock` fail the build instead of being silently
updated in CI, which is the whole point of committing it. `--workspace` belongs on clippy, nextest, the
doctests and `cargo doc` alike: without it clippy never lints a member crate's `#[cfg(test)]` code and
`cargo doc` documents only the root package, so the `docs` job's `RUSTDOCFLAGS: -D warnings` gate never sees
`migration/` at all.

`cargo machete` runs here for the same reason it runs in `prek`: it is the one step of the `check` target that has no
counterpart elsewhere in the pipeline, so leaving it out means a dropped dependency passes CI and fails the
next person's pre-commit hook.

## The toolchain step

`dtolnay/rust-toolchain@stable` bootstraps rustup and installs a stable toolchain; it does not decide which
compiler the build uses. The action runs `rustup toolchain install` and `rustup default` and sets no
`RUSTUP_TOOLCHAIN`, so `rust-toolchain.toml` still wins for every cargo command executed inside the
repository, and rustup installs that channel on first use together with the components the file declares —
`llvm-tools-preview` included, without anyone asking. The version pin therefore lives in exactly one file, and
the action's ref only decides what bootstraps rustup.

The `components:` input names them for the action's own `stable` toolchain. It is belt and braces: useful if
someone later deletes `rust-toolchain.toml`, redundant while the file exists.

`rustup toolchain install` with no argument does the same thing in one step (valid on rustup 1.28 and later,
which installs "the given toolchains, or by default the active toolchain") and skips the second download.
Either is correct; the action is the version with a maintained cache key output.

`Swatinem/rust-cache` must come after the toolchain step: it keys the cache on `rustc -vV`.

## The RUSTFLAGS trap

Setting `RUSTFLAGS: -Dwarnings` job-wide looks like the obvious way to make warnings fatal. It costs two full
rebuilds:

1. Cargo's fingerprint includes the effective rustflags, so a step with `RUSTFLAGS` set cannot reuse artifacts
   from a step without it.
2. `Swatinem/rust-cache` hashes environment variables by prefix — the default `env-vars` list is
   `CARGO CC CFLAGS CXX CMAKE RUST`, which matches `RUSTFLAGS`. The cache key changes too, so this job also
   fights the other jobs for a cache entry.

Pass `-D warnings` to the clippy invocation instead, as the workflow above does. The same reasoning applies to
`RUSTDOCFLAGS`: the `docs` job needs it, and its artifacts are therefore not interchangeable with the test
job's. `shared-key: docs` gives that job one fixed, stable cache entry of its own rather than a key that moves
with every environment change — rust-cache already separates jobs by default, so this is about keeping the
docs cache hittable, not about isolating it.

## Postgres: service container, not testcontainers

The `services:` block starts one Postgres for the whole job and `TEST_DATABASE_URL` points the tests at it;
each test then creates its own database. testcontainers stays the local fallback for developers who have set
no environment variable.

Leaving `TEST_DATABASE_URL` unset in CI while the tests fall back to testcontainers is the failure mode worth
avoiding: nextest forks one process per test, so a 16-core runner starts one container per test.

Docker is preinstalled on `ubuntu-latest`, so nothing extra is needed for the fallback path to work either.

### Optional services: Redis and NATS

When the service takes on a cache or a broker, give CI the same containers and point the matching
`TEST_*_URL` at them; `rust-redis` and `rust-nats` own the tests that read those variables. Redis is a plain
service container next to `postgres`:

```yaml
      redis:
        image: redis:8-alpine
        ports:
          - 6379:6379
        options: >-
          --health-cmd "redis-cli ping"
          --health-interval 10s
          --health-timeout 5s
          --health-retries 5
    env:
      TEST_DATABASE_URL: postgres://postgres:postgres@localhost:5432/postgres
      TEST_REDIS_URL: redis://localhost:6379
      TEST_NATS_URL: nats://localhost:4222
```

NATS needs JetStream, which is the `-js` flag, and a `services:` entry cannot pass a command to the image. Start
it in the first step instead and wait for its monitoring endpoint:

```yaml
      - name: Start NATS with JetStream
        run: |
          docker run -d --name nats -p 4222:4222 -p 8222:8222 nats:2.12-alpine -js -m 8222
          for i in $(seq 1 30); do
            curl -fsS http://localhost:8222/healthz && break
            sleep 1
          done
```

## Action versions

Verified current: `actions/checkout` v7, `Swatinem/rust-cache` v2, `taiki-e/install-action` v2,
`EmbarkStudios/cargo-deny-action` v2, `actions/upload-artifact` v7, `codecov/codecov-action` v7.

`dtolnay/rust-toolchain` is referenced by branch (`stable`, `nightly`, `master`, version branches) rather than
by the `v1` tag it also publishes, because the branch name is the toolchain selector, not a version number.

Use `taiki-e/install-action` for cargo tools in CI rather than `cargo binstall`: it resolves prebuilt binaries
from its own manifest, needs no bootstrap install and accepts a comma-separated list.
