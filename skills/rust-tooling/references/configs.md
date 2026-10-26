# Configuration files, verbatim

Every file below lives at the repository root unless the heading says otherwise. In a new repository copy them
as they are; in an existing one merge only the entries the change needs into its own files, keeping its tuned
values. Each comment explains a choice that is not obvious from the key name.

## Contents

- [rust-toolchain.toml](#rust-toolchaintoml)
- [Cargo.toml — the lints table](#cargotoml--the-lints-table)
- [Cargo.toml — cargo-machete exemptions](#cargotoml--cargo-machete-exemptions)
- [Cargo.toml — the dev profile](#cargotoml--the-dev-profile)
- [target/ size](#target-size)
- [clippy.toml](#clippytoml)
- [rustfmt.toml](#rustfmttoml)
- [.config/nextest.toml](#confignextesttoml)
- [deny.toml](#denytoml)
- [bacon.toml](#bacontoml)
- [.pre-commit-config.yaml](#pre-commit-configyaml)
- [Makefile](#makefile)
- [Optional: the typos hook](#optional-the-typos-hook)

## rust-toolchain.toml

```toml
[toolchain]
channel = "1.99.0"
components = ["rustfmt", "clippy", "llvm-tools-preview"]
```

`llvm-tools-preview` is what cargo-llvm-cov instruments with; without it coverage fails at the first run.

The channel is patch-exact, not `"1.99"`, because the cargo-chef base image is tagged with a full version
(`latest-rust-1.99.0`) and rustup matches toolchain names literally: a `"1.99"` channel is a different name
from the `1.99.0` toolchain baked into the image, so both Docker stages download a second toolchain before
they build anything. Keep the minor version equal to `rust-version` in `Cargo.toml`, which stays `"1.99"`
because it is the MSRV, not a pin.

## Cargo.toml — the lints table

Levels live here, thresholds live in `clippy.toml`. The tables sit under `[workspace.lints.*]` in the
root manifest and every member, the root package included, opts in with `[lints] workspace = true`; a
member without that line inherits nothing. Groups carry `priority = -1` so the individual lines below
them win.

```toml
[lints]
workspace = true            # in the root package AND in migration/Cargo.toml

[workspace.lints.rust]
unsafe_code = "forbid"
unused_must_use = "deny"

[workspace.lints.clippy]
all = { level = "warn", priority = -1 }
pedantic = { level = "warn", priority = -1 }
unwrap_used = "warn"
expect_used = "warn"
panic = "warn"
todo = "warn"
unimplemented = "warn"
dbg_macro = "warn"
print_stdout = "warn"
print_stderr = "warn"
allow_attributes_without_reason = "warn"
cognitive_complexity = "warn"
redundant_clone = "warn"
module_name_repetitions = "allow"
must_use_candidate = "allow"
missing_errors_doc = "allow"
missing_panics_doc = "allow"
```

What each lint catches is `rust-code-style`'s to explain. Two facts belong here: `panic = "warn"`
exists because a panic in a request path is a bug the `CatchPanicLayer` (tower-http feature `catch-panic`)
only turns into a logged, constant 500 — never a way to signal failure — and `redundant_clone` is a
nursery lint that has to be named to be on.

`too_many_lines` and `too_many_arguments` already arrive through `pedantic` and `all`; listing them again
changes nothing. `string_to_string` was removed in clippy 0.1.98 — do not add it.

## Cargo.toml — cargo-machete exemptions

```toml
[package.metadata.cargo-machete]
# The stack ships these for the code you are about to write; `rust_decimal` is
# additionally reachable only through sea-orm's `with-rust_decimal` feature, which
# machete cannot see. Delete a name here when you start using the crate.
ignored = ["bon", "derive_more", "itertools", "rust_decimal", "strum", "tokio-stream", "futures", "async-trait"]
```

The list is the template's: crates the stack ships for code not yet written, plus one reachable only
through a feature. Delete a name when you start using that crate. Add a new name only after deleting the
dependency line and watching the build fail, with a comment naming the path that makes it reachable.

## Cargo.toml — the dev profile

```toml
# Backtraces need only line tables, so your crates keep those and dependencies
# keep no debuginfo: target/ is smaller and links faster. For a debugger session
# that inspects variables, build with CARGO_PROFILE_DEV_DEBUG=true.
[profile.dev]
debug = "line-tables-only"

[profile.dev.package."*"]
debug = false
```

Profiles are read from the root manifest only; cargo ignores one in a member. The `test` profile inherits
from `dev`, so `cargo nextest run` gets the same setting. On the scaffold's ~900-crate dependency tree,
dependencies without debuginfo took a clean `cargo build --workspace --all-targets` from 3.1 GB to 2.1 GB and
from 84 s to 71 s (rustc 1.98.1, 15 September 2026). Line tables instead of full debuginfo for your own crates
then took the full dev loop from 5.4 to 4.7 GB at the same edit-rebuild time, and one test binary took it to the
4.1 GB [below](#target-size) (rustc 1.99.0, 25 October 2026). Backtraces keep file and line; a debugger loses local variables.

## target/ size

`target/` only grows. Cargo keeps one set of artifacts per toolchain, dependency version, feature set and
profile, plus an incremental cache per crate and build hash, and deletes none of them: stable cargo's garbage
collection covers `~/.cargo` only, and the `target/` half is cargo#13136, still open. One full dev loop on the
scaffold, measured on rustc 1.99.0:

| Command | Adds | Why it is a separate set |
|---|---|---|
| `make test` | 1.6 GB | the test build |
| `make run` | 0.5 GB | without dev-dependencies tokio loses `test-util`, so every crate above tokio builds again |
| `make lint` | 0.3 GB | clippy writes metadata only, never reusing the test build's artifacts |
| `make cov` | 1.6 GB | an instrumented build of everything in `target/llvm-cov-target` |

Rebuilding inside a set replaces files; what grows is the number of sets. A toolchain bump adds a whole new
one, and a `cargo update` that moves a crate most of the tree depends on nearly does. Hence:

- A toolchain bump leaves the previous toolchain installed too, about 600 MB: the patch-exact pin never
  removes it.
- `make test` and `make cov` run `cargo clean` once the target directory passes `TARGET_CAP_GB` (see the
  Makefile below): a full rebuild of about 1.5 minutes now and then, in exchange for a bound. Builds outside
  them (bacon, an IDE, plain cargo) are never capped, and `cargo clean` takes no lock: one of them running in
  the directory at that moment fails once.
- One integration-test binary (`rust-testing`): every extra file directly in `tests/` adds about 85 MB of
  binary plus its incremental cache, in the test build and the coverage build both.
- Each checkout of the repository, git worktrees included, pays for the whole set. Pointing several at one
  `CARGO_TARGET_DIR` shares the dependency builds, but their builds then queue on one lock (`Blocking waiting
  for file lock on build directory`). The cap only reports a directory outside the workspace, since it may be
  shared: run `cargo clean` there by hand when nothing is building.

`cargo clean` re-fetches nothing: sources stay under `~/.cargo`, which cargo prunes on its own (unused for a
month when it can rebuild them, three months when it downloaded them). Cargo's `cargo clean gc` size limits
(`--max-crate-size` and friends) are nightly-only in 1.99 and never touch `target/`. cargo-sweep, the usual
age-based pruner, is unmaintained.

## clippy.toml

```toml
# Levels live in Cargo.toml's [lints]; only thresholds belong here.

# `unwrap_used` / `expect_used` stay on for src/, off inside test functions.
# NOTE: this covers the body of a `#[test]` fn only. A helper in
# tests/api/common.rs still warns and needs its own `#![allow(...)]`.
allow-unwrap-in-tests = true
allow-expect-in-tests = true
allow-panic-in-tests = true
allow-dbg-in-tests = true

# The default of 25 lets a genuinely tangled function through.
cognitive-complexity-threshold = 15
# Past five arguments, pass a struct.
too-many-arguments-threshold = 5
```

The four `allow-*-in-tests` keys have the same scope: the body of a function carrying `#[test]` or
`#[tokio::test]`, nothing else. What a helper outside such a function needs is `rust-testing`'s call.

## rustfmt.toml

```toml
edition = "2024"          # for a bare `rustfmt file.rs`, which has no cargo to ask
style_edition = "2024"    # the knob cargo fmt actually reads for formatting rules
max_width = 100
use_field_init_shorthand = true
use_try_shorthand = true
newline_style = "Unix"
```

Both edition keys are stable on 1.99 and both are worth setting: `style_edition` selects the formatting rules,
while `edition` is what a bare `rustfmt some_file.rs` outside cargo parses with — dropping it makes that
invocation parse as edition 2015.

Stable options only. `imports_granularity` and `group_imports` are the two people reach for most and both are
nightly-only: on a stable toolchain rustfmt prints "unstable features are only available in nightly channel"
and ignores them, so the repo is silently unformatted in exactly the way that was configured.

## .config/nextest.toml

```toml
nextest-version = { required = "0.9.120", recommended = "0.9.148" }

[profile.default]
retries = 0
slow-timeout = { period = "30s", terminate-after = 4 }
failure-output = "immediate"
fail-fast = true

[profile.ci]
retries = { backoff = "exponential", count = 2, delay = "1s", jitter = true }
slow-timeout = { period = "60s", terminate-after = 5, grace-period = "30s" }
failure-output = "immediate-final"
final-status-level = "flaky"
fail-fast = false

[profile.ci.junit]
path = "junit.xml"
```

There is no built-in `ci` profile; select it with `cargo nextest run -P ci` or `NEXTEST_PROFILE=ci`. Profiles
inherit from `default` unless they set `inherits`. The JUnit file lands in `target/nextest/ci/junit.xml`:
the path is relative to the profile store dir, not to the repository root. `status-level` and
`final-status-level` use different ladders: `none|fail|retry|slow|leak|pass|skip|all` and
`none|fail|flaky|slow|skip|leak|pass|all`; the defaults (`pass` and `none`) are already right, so only the
second is set here.

Deliberately absent: a `[test-groups]` entry capping database tests. It would not reduce anything — nextest
forks one process per test, so a group only serializes tests that each already own a private database. Add one
only for a genuinely shared, uncloneable resource, and scope it to an area by module, `test(/^users::/)`: there
is one test binary, `api`, and `filter = 'binary(name)'` is validated at config-parse time, so a name matching
no test binary makes every nextest command exit non-zero with `operator didn't match any binary names`.

## deny.toml

`cargo deny init` writes `allow = []`, so its output fails on the first run (it also writes
`wildcards = "allow"`, which hides the path-dependency check until you set `"deny"`). Use this instead — it is
green on the whole stack for all four checks with no `license-not-encountered` warning: every id in `allow`
is met by some crate in the tree, so an id that stops being met is a signal to remove it, not noise to ignore.

```toml
[graph]
all-features = true

[advisories]
db-urls = ["https://github.com/rustsec/advisory-db"]
unmaintained = "workspace"   # a scope, not a level: all | workspace | transitive | none
unsound = "all"
yanked = "deny"
# Explicit and empty: an advisory is only ever waived here, with its RUSTSEC id
# and a reason, so the waiver stays reviewable.
ignore = []

[licenses]
confidence-threshold = 0.93
# `cargo deny init` writes `allow = []`, so its first run always fails. This list
# is green on the whole stack.
allow = [
    "MIT", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "BSD-2-Clause", "BSD-3-Clause",
    "ISC", "Unicode-3.0", "Zlib", "CDLA-Permissive-2.0", "Unlicense", "BSL-1.0",
    "CC0-1.0",                   # CC0: axum-tracing-opentelemetry and its sdk
]

[licenses.private]
ignore = true                    # needs `publish = false` on both crates

[bans]
multiple-versions = "warn"       # ~25 duplicates on this stack; "deny" is unusable
wildcards = "deny"
allow-wildcard-paths = true      # for `migration = { path = "migration" }`

[sources]
unknown-registry = "deny"
unknown-git = "deny"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
```

Both `[package]` tables (the service and `migration/`) need `publish = false`: without it
`[licenses.private] ignore = true` does nothing and your own crates fail as `unlicensed`, and
`allow-wildcard-paths` refuses to apply because crates.io forbids path dependencies in published crates.

Unknown keys are a hard parse error in cargo-deny 0.20, not a warning — `unlicensed`, `deny`, `copyleft`,
`default` and `allow-osi-fsf-free` were all removed from `[licenses]`. `[[licenses.exceptions]]` accepts
`crate` (or `name` plus `version`) and `allow` only; it has no `reason` field, unlike `[bans]` entries and
`[advisories] ignore`.

A green run still prints a tree of duplicate crate versions (about 25 on this stack): that is the
`multiple-versions = "warn"` output, not a failure. The verdict is the last line.

## bacon.toml

```toml
default_job = "clippy"

[jobs.check]
command = ["cargo", "check", "--workspace", "--all-targets", "--color", "always"]
need_stdout = false

[jobs.clippy]
command = ["cargo", "clippy", "--workspace", "--all-targets", "--all-features", "--color", "always"]
need_stdout = false

[jobs.nextest]
command = [
    "cargo", "nextest", "run", "--workspace",
    "--hide-progress-bar", "--failure-output", "final",
]
need_stdout = true
analyzer = "nextest"

[jobs.doc]
command = ["cargo", "doc", "--no-deps", "--color", "always"]
need_stdout = false

[keybindings]
c = "job:clippy"
n = "job:nextest"
d = "job:doc"
```

`need_stdout` and `analyzer` are orthogonal, not alternatives. `need_stdout` decides whether bacon captures
stdout at all — cargo diagnostics go to stderr, which is always captured, hence `false` for check, clippy and
doc. `analyzer` picks the parser for the captured lines; nextest output is unintelligible to the default
`standard` analyzer. The nextest job needs both, and omitting either silently parses nothing.

`--workspace` on check, clippy and nextest is not optional in a workspace: without it the dev loop never sees
`migration/`, and `cargo nextest run` never runs a member's tests. bacon already ships a built-in `test` job,
so redefining one only shadows it.

`watch` and `ignore` are per-job fields, not global tables. The implicit watch set is `src`, `tests`,
`benches`, `examples`, `build.rs`, and `apply_gitignore` defaults to true. `bacon --list-jobs` prints the
merged job list, which is the fastest way to confirm the file was picked up.

## .pre-commit-config.yaml

Run through prek (`prek install`, then `prek run --all-files`). The local hooks deliberately invoke the same
commands as the Makefile, so a hook cannot pass while CI fails.

```yaml
repos:
  - repo: https://github.com/pre-commit/pre-commit-hooks
    rev: v6.0.0
    hooks:
      - id: trailing-whitespace
        args: [--markdown-linebreak-ext=md]
      - id: end-of-file-fixer
      - id: check-yaml
      - id: check-toml
      - id: check-json
      - id: check-merge-conflict
      - id: check-added-large-files
        args: [--maxkb=512]

  - repo: local
    hooks:
      - id: fmt
        name: cargo fmt
        entry: cargo fmt --all -- --check
        language: system
        types: [rust]
        pass_filenames: false
      - id: clippy
        name: cargo clippy
        entry: cargo clippy --workspace --all-targets --all-features -- -D warnings
        language: system
        types: [rust]
        pass_filenames: false
      - id: deny
        name: cargo deny
        entry: cargo deny check
        language: system
        files: Cargo\.(toml|lock)$
        pass_filenames: false
      - id: machete
        name: cargo machete
        entry: cargo machete
        language: system
        files: Cargo\.toml$
        pass_filenames: false
```

The two `args:` lines are the only tuning worth making: `--markdown-linebreak-ext=md` stops
`trailing-whitespace` from eating the two-space line break Markdown uses, and `--maxkb=512` catches a fixture
or a binary committed by accident well before the default 500 kB would.

`types: [rust]` and `pass_filenames: false` are not in conflict: the filter decides whether the hook runs at
all, and the hook then ignores the file list and checks the whole crate. With no Rust file staged, the four
local hooks report "(no files to check) Skipped". `always_run: true` is the only way to bypass that.

prek is a drop-in reimplementation of pre-commit, so avoid its own extensions (`repo: builtin`, `groups`,
`priority`) while anyone on the team still runs pre-commit.

## Makefile

Cargo has no script section, so the Makefile is the task runner and the single entrypoint.

```makefile
.PHONY: install-tools fmt lint check test cov dev run migrate entity

# tests/api/common.rs names a run's databases test_<run>_<uuid> and its template
# test_<run>_<package>_template. Only test and cov set the run: any other nextest run, a
# bacon session included, takes nextest's own run id and builds a fresh template.
test cov: export TEST_RUN := $(shell date +%s)_$(shell echo $$$$)
TEST_PG := rust-powers-test-postgres
# $(call drop_test_dbs,CONDITION): drops the reused test container's databases that
# meet CONDITION. With TEST_DATABASE_URL instead, the server is left alone.
drop_test_dbs = if docker ps -q --filter name='^$(TEST_PG)$$' 2>/dev/null | grep -q .; then \
	docker exec $(TEST_PG) psql -U postgres -tAc "SELECT format('DROP DATABASE %I WITH (FORCE);', datname) \
	FROM pg_database WHERE datname LIKE 'test\_%' AND $(1)" | docker exec -i $(TEST_PG) psql -qU postgres; fi
# A database's directory is as old as the database.
STALE_TEST_DB := (pg_stat_file('base/' || oid || '/PG_VERSION', true)).modification < now() - interval '1 hour'
# Stable cargo never deletes anything from target/ (cargo#13136): every toolchain,
# dependency version and feature set ever built stays there. Past the cap, start over.
TARGET_CAP_GB ?= 15
# cargo metadata resolves CARGO_TARGET_DIR, .cargo/config.toml and build.build-dir alike;
# `sort` puts build_directory first, and a cargo without that field reports target_directory.
# `cargo clean` takes no lock, so a directory outside the workspace, maybe shared, is only reported.
cap_target = meta=$$(cargo metadata --no-deps --format-version 1); \
	dir=$$(printf '%s' "$$meta" | grep -oE '"(build|target)_directory":"[^"]*"' | sort | head -1 | cut -d'"' -f4); \
	root=$$(printf '%s' "$$meta" | grep -oE '"workspace_root":"[^"]*"' | cut -d'"' -f4); \
	kb=$$(du -sk "$$dir" 2>/dev/null | cut -f1); \
	if [ "$${kb:-0}" -gt $$(($(TARGET_CAP_GB) * 1048576)) ]; then case "$$dir" in \
	"$$root"/*) echo "$$dir is past $(TARGET_CAP_GB) GB: cargo clean"; cargo clean;; \
	*) echo "$$dir is past $(TARGET_CAP_GB) GB and outside this workspace: cargo clean it when nothing builds there";; \
	esac; fi

install-tools:
	cargo install cargo-binstall
	cargo binstall -y cargo-nextest cargo-llvm-cov cargo-deny cargo-machete cargo-chef bacon cargo-insta sea-orm-cli prek
	prek install

fmt:
	cargo fmt --all

lint:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets --all-features -- -D warnings

check: lint
	cargo deny check
	cargo machete

# A passing run drops its own databases. A failing run keeps them for an hour, so
# the database a test failed on can still be read; the next run drops them.
test:
	@$(cap_target)
	@$(call drop_test_dbs,$(STALE_TEST_DB))
	cargo nextest run --workspace --all-features
	cargo test --doc --workspace
	@$(call drop_test_dbs,datname LIKE 'test\_$(TEST_RUN)\_%')

cov:
	@$(cap_target)
	@$(call drop_test_dbs,$(STALE_TEST_DB))
	cargo llvm-cov nextest --workspace --all-features --no-report
	@$(call drop_test_dbs,datname LIKE 'test\_$(TEST_RUN)\_%')
	cargo llvm-cov report --lcov --output-path $(or $(CARGO_TARGET_DIR),target)/lcov.info --ignore-filename-regex '(entities|migration)/|main\.rs|telemetry\.rs'
	cargo llvm-cov report --summary-only --fail-under-lines 80 --ignore-filename-regex '(entities|migration)/|main\.rs|telemetry\.rs'

dev:
	bacon

run:
	cargo run

migrate:
	sea-orm-cli migrate up

entity:
	sea-orm-cli generate entity -o src/entities --with-serde both --entity-format dense
```

Recipes are tab-indented. `cargo test --doc` needs a library target, so it fails with "no library targets
found" on a binary-only crate — the service keeps `src/lib.rs` for this reason among others.

`--workspace` belongs on every cargo command here. Without it clippy skips member crates' `#[cfg(test)]` code
and nextest never runs a member's tests, so the Makefile and CI stop agreeing with each other.

`test cov: export TEST_RUN := ..` is a target-specific export (GNU make 3.81, the macOS default, accepts it),
so only those two recipes see the run id. It prefixes the run's test databases, and each recipe's last line
drops a passing run's own; a failing recipe stops before that line and keeps them. Exported globally, a
bacon session started by `make dev` would keep one id for its whole life; left alone, it falls back to
nextest's own `NEXTEST_RUN_ID`, new for every nextest run, as CI does. The cleanup reaches only the reused
`rust-powers-test-postgres` container; what it drops and why is `sea-orm-postgres`'s.

`cap_target` opens `test` and `cov`: it measures the build directory `cargo metadata` reports, wherever
`CARGO_TARGET_DIR` or a `.cargo/config.toml` put it. Past `TARGET_CAP_GB` whole gigabytes it runs `cargo clean`
when that directory is inside the workspace, and prints a reminder when it is outside. That costs about half a
second of `du` on a 9 GB directory, a few seconds cold. Raise the cap per run (`make test TARGET_CAP_GB=30`) or in
the environment; why the directory needs one is under [target/ size](#target-size).

`cov` writes `lcov.info` into the target directory (`CARGO_TARGET_DIR` when set; a hard-coded `target/` then
does not exist and the step fails) **before** the gate runs, because `--fail-under-lines` exits non-zero and make
stops at the first failing line: with the two in the other order a failing gate leaves no report to look at.
`--fail-under-lines` is the number, `--ignore-filename-regex` is the denominator — CI's coverage step repeats
both, so change the two together or the Makefile and CI drift apart. For which files to exclude and why, see
`rust-testing`; a regex that excludes everything reports zero lines and the gate can no longer pass.

Snapshot review is part of the same loop: `cargo insta review` walks the `.snap.new` files interactively,
`cargo insta accept` takes them all, and `cargo insta reject` discards them. `*.pending-snap` belongs in
`.gitignore`. Never set `INSTA_UPDATE=always` in CI — it rewrites the snapshots the run is supposed to be
checking, and every assertion passes.

## Optional: the typos hook

A spell checker over source and prose. Add the hook and its config together, or the defaults flag identifiers
inside `Cargo.lock`.

```yaml
  - repo: https://github.com/crate-ci/typos
    rev: v1.50.3
    hooks:
      - id: typos
        args: [--force-exclude]        # replaces the baked-in --write-changes
```

```toml
# _typos.toml
[files]
extend-exclude = ["Cargo.lock", "*.snap", "CHANGELOG.md"]

[default]
locale = "en"
```

The hook's baked-in arguments are `[--write-changes, --force-exclude]`, so by default it rewrites files in
place. Overriding `args` to `[--force-exclude]` turns it into a non-mutating gate; `--force-exclude` is also
what makes `files.extend-exclude` win over the filenames the hook passes on the command line.
