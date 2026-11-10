# GitHub Actions

One workflow, three jobs: `test` (format, lint, tests, coverage), `deny` (supply chain), `docs`. Its commands
are the Makefile's, plus three things that neither the hooks nor the `check` and `test` targets do: `--locked`
on every cargo call that resolves, the coverage gate (`make cov` locally), and the `docs` job, which has no
Makefile target (`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --all-features` reproduces it).
A green `make check && make test` predicts the pipeline up to those three.

## Contents

- [The workflow](#the-workflow)
- [No toolchain step](#no-toolchain-step)
- [The RUSTFLAGS trap](#the-rustflags-trap)
- [Postgres: service container, not testcontainers](#postgres-service-container-not-testcontainers)
  - [Optional services: Redis and NATS](#optional-services-redis-and-nats)
- [Action versions](#action-versions)

## The workflow

`.github/workflows/ci.yml`:

```yaml
name: CI
# A branch with an open pull request would otherwise run twice per push.
on:
  push:
    branches: [main]
  pull_request:

# Nothing here writes to the repository.
permissions:
  contents: read

# A newer push to a pull request cancels that pull request's older run. A push
# to main has no pull request, so each commit there gets a group of its own
# and always finishes.
concurrency:
  group: ${{ github.workflow }}-${{ github.event.pull_request.number || github.run_id }}
  cancel-in-progress: true

env:
  CARGO_TERM_COLOR: always
  # RUSTFLAGS is deliberately not set: the variable replaces the rustflags in
  # .cargo/config.toml, and each value is built as its own set of artifacts.
  # `-D warnings` goes to clippy.

jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 30
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
      # No toolchain step: rustup reads rust-toolchain.toml on the first cargo
      # call and installs that channel with its components. A step naming
      # `stable` would download a second toolchain beside it.

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
    timeout-minutes: 10
    steps:
      - uses: actions/checkout@v7
      - uses: EmbarkStudios/cargo-deny-action@v2
        with:
          command: check advisories bans licenses sources

  docs:
    runs-on: ubuntu-latest
    timeout-minutes: 20
    env:
      RUSTDOCFLAGS: -D warnings
    steps:
      - uses: actions/checkout@v7
      - uses: Swatinem/rust-cache@v2
      - run: cargo doc --no-deps --workspace --all-features --locked
```

The triggers are a push to `main` and any pull request. A bare `on: [push, pull_request]` runs the workflow
twice for every push to a branch that has a pull request open. `permissions: contents: read` narrows the
`GITHUB_TOKEN` to what the three jobs use, since none of them writes to the repository; a job added later that
comments on a pull request or publishes a package widens it for that job alone. `concurrency` cancels the
older run when a newer commit arrives on the same pull request: the group is the pull request's number.
A push to `main` has no pull request, so its group falls back to the run id, which is unique, and every
commit on `main` finishes its own run; a group keyed on `github.ref` would cancel the run of the previous
merge instead, and one keyed on `github.head_ref`, the bare branch name, is shared by two pull requests
from forks whose branches have the same name.
`timeout-minutes` ends a hung job in minutes, where GitHub's default lets it hold a runner for six hours.

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

## No toolchain step

The workflow installs no toolchain, on purpose. GitHub's hosted runners ship rustup, and
`rust-toolchain.toml` decides the compiler: the first `rustc` or `cargo` call inside the checkout makes
rustup install that channel together with the components the file lists, `llvm-tools-preview` included,
without anyone asking. The version pin therefore lives in exactly one file.

`dtolnay/rust-toolchain@stable` in front of that adds a second toolchain nothing uses. The action runs
`rustup toolchain install` and `rustup default` for `stable` and sets no `RUSTUP_TOOLCHAIN`, so
`rust-toolchain.toml` still wins for every cargo command executed inside the repository, and its
`components:` input lands on the toolchain that is never run.

Add a step in two cases only. Where rustup's install on demand is switched off (`RUSTUP_AUTO_INSTALL=0`,
or `rustup set auto-install disable` on a self-hosted runner), run `rustup toolchain install` with no
argument right after the checkout: on rustup 1.28 and later it installs "the given toolchains, or by default
the active toolchain", which is the one the file names. In a project that has no `rust-toolchain.toml`, or
on a runner without rustup, use `dtolnay/rust-toolchain@<channel>`, which bootstraps rustup; name the
file's own channel where there is a file, and keep the step before `Swatinem/rust-cache`, which hashes
`rustc -vV` into its key.

`Swatinem/rust-cache` goes after `actions/checkout`: its key also hashes `Cargo.lock`, the manifests and
`rust-toolchain.toml`, which do not exist before it.

## The RUSTFLAGS trap

Setting `RUSTFLAGS: -Dwarnings` looks like the obvious way to make warnings fatal. The variable reaches
every rustc call, dependencies included, and that has three costs:

1. It replaces the `rustflags` in `.cargo/config.toml` instead of adding to them: cargo takes the flags
   from the first source that has any, and the environment comes before the file. A linker or
   `target-cpu` setting kept there stops applying in CI, with no warning.
2. Cargo's fingerprint includes the effective rustflags, and each value gets an artifact set of its own.
   A cargo command that runs under a different value than the others (a step-level `env`, for example)
   shares nothing with them: every crate is compiled once more, and the cache has to hold both sets.
3. `Swatinem/rust-cache` hashes environment variables by prefix into the cache key — the default `env-vars`
   list is `CARGO CC CFLAGS CXX CMAKE RUST`, which matches `RUSTFLAGS`. Adding, editing or removing the
   variable changes the key, and the next run starts from an empty cache.

Pass `-D warnings` to the clippy invocation instead, as the workflow above does: clippy reports rustc's
warnings too, and the same command then runs unchanged in the Makefile and the hook, where no job
environment exists.

`RUSTDOCFLAGS` is not the same trap. It reaches rustdoc only: cargo documents the crates again when it
changes and recompiles nothing. The `docs` job needs it, and setting it there, on that job alone, costs
nothing. The job needs no `shared-key` either: rust-cache already keeps one cache entry per job id.

## Postgres: service container, not testcontainers

The `services:` block starts one Postgres for the whole job and `TEST_DATABASE_URL` points the tests at it;
each test then clones its own database from a template the run migrates once. The run id is nextest's own
`NEXTEST_RUN_ID`, so the job sets none, and the container goes away with the job, so nothing is cleaned up.
testcontainers stays the local fallback for developers who have set no environment variable.

Leaving `TEST_DATABASE_URL` unset in CI while the tests fall back to testcontainers is the failure mode worth
avoiding: every job then pulls the image, and nextest's first processes race to create the reused container.
A service container is up and healthy before the first test.

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
          docker run -d --name nats -p 4222:4222 -p 8222:8222 nats:2.14-alpine -js -m 8222
          for i in $(seq 1 30); do
            curl -fsS http://localhost:8222/healthz && break
            sleep 1
          done
```

## Action versions

Verified current: `actions/checkout` v7, `Swatinem/rust-cache` v2, `taiki-e/install-action` v2,
`EmbarkStudios/cargo-deny-action` v2, `actions/upload-artifact` v7, `codecov/codecov-action` v7.

`dtolnay/rust-toolchain`, where a project needs it, is referenced by branch (`stable`, `nightly`, `master`,
version branches) rather than by the `v1` tag it also publishes, because the branch name is the toolchain
selector, not a version number.

Use `taiki-e/install-action` for cargo tools in CI rather than `cargo binstall`: it resolves prebuilt binaries
from its own manifest, needs no bootstrap install and accepts a comma-separated list.
