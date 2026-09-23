---
name: rust-tooling
description: "Use when changing or debugging the tooling of an existing Rust service — rust-toolchain.toml, cargo add, upgrading one dependency, the lockfile, the workspace lints table or a member clippy skips, clippy.toml, rustfmt.toml, nextest, llvm-cov and the coverage gate, cargo-deny, cargo-machete, bacon, prek or pre-commit hooks, the Makefile, a Dockerfile rebuilding every dependency, docker-compose, CI recompiling, target/ size, unknown lint warnings. Not for a new repository (rust-scaffolding), one clippy finding (rust-code-style), or what coverage measures (rust-testing)."
metadata:
  version: "0.2.0"
---

# Rust Tooling

Assumes Rust 1.98.1 edition 2024 with resolver 3, cargo-nextest 0.9, cargo-llvm-cov 0.8, cargo-deny 0.20,
cargo-machete 0.9, bacon 3.25, prek 0.5.

Names such as `AppError`, `test_app()`, `Valid<T>` and the Makefile targets come from the rust-scaffolding template. In a project built differently, use its own types, helpers and tooling, map outcomes onto its nearest existing error variant, and say so when none fits instead of adding one. Apply these rules to new code; when editing existing code, keep its public contract and tuned configuration and report differences instead of rewriting, unless asked. If `Cargo.lock` pins another major or minor version than the line above, follow the project and say which rules may not apply.

## Important

- Change the config file, never the invocation: a flag typed into a terminal is not reproduced by the hook,
  by CI, or by the next person. Every tool has one config file and one Makefile target.
- The Makefile, the prek hooks and CI run the same commands; a change to one is a change to all three.
- Lint levels live in `[workspace.lints.*]` with `[lints] workspace = true` in every member; thresholds in
  `clippy.toml`; never `#![deny(...)]` in source.
- `cargo add`, never a hand-written version; `Cargo.lock` is committed and CI passes `--locked`.
- A dependency, a lint or a tool version changes in the config and the lock file together, in one commit.

## References

- `references/configs.md` — every configuration file verbatim, identical to the template's, with the
  reasoning inline. Open it before creating or editing `rust-toolchain.toml`, the `[workspace.lints]` table,
  `clippy.toml`, `rustfmt.toml`, `.config/nextest.toml`, `deny.toml`, `bacon.toml`,
  `.pre-commit-config.yaml` or the Makefile.
- `references/ci.md` — the GitHub Actions workflow, the toolchain step, the RUSTFLAGS and cache-key trap, the
  Postgres service container and the optional Redis and NATS ones. Open it when adding or debugging a CI job.
- `references/docker.md` — the cargo-chef Dockerfile, `.dockerignore`, release profile, base image choice and
  docker-compose with the optional Redis and NATS services. Open it when containerising the service or
  running dependencies locally.

## Quick reference

| Task | Command |
|---|---|
| Check formatting and lint as CI does | `make lint` (`make fmt` rewrites) |
| Full local gate before a pull request | the `check` target, then `make test` |
| Watch loop while editing | `make dev` (bacon, default job clippy; `n` switches to nextest) |
| Coverage with the gate | `make cov` |
| Add a dependency | `cargo add <crate>` |
| Find duplicate versions | `cargo tree -d` |
| Install the cargo tools | `make install-tools` |
| Run the hooks over everything | `prek run --all-files` |
| Reclaim disk from `target/` | `cargo clean`; deps build without debuginfo (`references/configs.md`) |

## Toolchain

`rust-toolchain.toml` pins the channel and the components; rustup honours it for every cargo command inside
the repository and installs that channel, components included, on first use — so no CI step restates the list.

The channel is patch-exact (`1.98.1`) because the cargo-chef image tag names a full version and rustup matches
toolchain names literally: a `"1.98"` channel is a second toolchain, downloaded in every Docker stage.
`rust-version` in `Cargo.toml` stays `"1.98"`: it is the MSRV, and the edition 2024 resolver prefers dependency
versions that build on it, so an MSRV set too low silently holds dependencies back; the lock step prints
`(available: vX, requires Rust Y)` and moves on.

Bumping the toolchain is a lint bump: edit the channel and the chef image tag in one commit, run
`cargo clippy --workspace --all-targets --all-features -- -D warnings`, fix whatever the new clippy added.

## Dependencies and the lockfile

Use `cargo add <crate>`, with `--features`, `--no-default-features`, `--dev` or `-p <member>` as needed: it
resolves the latest compatible version, where a hand-written version string is a guess that ages.
`cargo remove <crate>` is the inverse, and `--dry-run` on either shows the change first.

A binary crate commits `Cargo.lock`, and `--locked` in CI makes a stale lockfile fail the build instead of
being quietly updated on the runner.

To bump one crate safely:

1. `cargo update -p <crate> --dry-run` — read what actually moves; a bump often drags transitive crates.
2. `cargo update -p <crate>`, or `cargo add <crate>@<major>` when `Cargo.toml` has to change too.
3. `make lint && make test`.
4. Read the crate's changelog for every version crossed, not just the top entry.

`cargo update` with no argument moves everything within semver — do that deliberately, never inside another
change. `--precise <version>` pins one crate exactly, the escape hatch when a release is broken.

## Lints

`#![deny(...)]` in source cannot be relaxed for one invocation, which makes an editor's background check
hostile; escalation belongs at the call site, with `-D warnings`. Lint groups need `priority = -1` so the
individual lines below them win. A member without `[lints] workspace = true`, the root package included,
inherits nothing, and `cargo clippy --workspace` then passes it unlinted.

`allow-unwrap-in-tests` in `clippy.toml` covers only the body of a function carrying `#[test]` or
`#[tokio::test]`; what a helper outside one needs is `rust-testing`'s call.

`rustfmt.toml` carries stable options only. `imports_granularity` and `group_imports` are nightly-only: on
stable rustfmt warns once and ignores them, leaving the repository unformatted in exactly the way that was
configured.

Both tables verbatim are in `references/configs.md`; why a lint is on is `rust-code-style`'s.

## Installing the cargo tools

`cargo install cargo-binstall` once, then `cargo binstall -y <tools>`, prek included: it fetches prebuilt
binaries, so the whole set lands in seconds instead of minutes of compilation. `make install-tools` does that
and runs `prek install`. Fall back to `cargo install --locked <tool>` when a crate publishes no binaries; in CI
use `taiki-e/install-action` instead of bootstrapping binstall.

## Tests and coverage

`.config/nextest.toml` holds a `default` profile for laptops and a `ci` profile with retries, a longer
slow-timeout and JUnit output; select it with `-P ci`. The JUnit file lands in `target/nextest/ci/junit.xml`,
relative to the profile store dir rather than the repository root.

There is no `[test-groups]` entry: each test owns a private database, so a concurrency cap only serializes
tests that never contend. A `filter = 'binary(name)'` override is validated at config-parse time, and a name
matching no test binary makes every nextest command fail.

nextest cannot run doctests, so `make test` runs `cargo test --doc` as a second step.

Coverage is `cargo llvm-cov nextest --no-report` then `cargo llvm-cov report`. `--fail-under-lines` is the
threshold and `--ignore-filename-regex` the denominator; the `cov` recipe and CI's coverage step each carry
both, so change them together. Write the lcov file before the gate, or a failure leaves no report. `report`
accepts `-p`, not `--workspace`; `--branch` (top-level, not a `report` flag) and `--doctests` are unstable.
For which files to leave out of coverage and why, see `rust-testing`.

Snapshots: `cargo insta review` steps through pending `.snap.new` files, `cargo insta accept` takes them all,
and `*.pending-snap` belongs in `.gitignore`. Never set `INSTA_UPDATE=always` in CI — it rewrites the
snapshots the run exists to check.

## Supply chain

`cargo deny check` runs advisories, bans, licenses and sources. `cargo deny init` writes an empty licence
allow-list, so its own output fails immediately; it also writes `wildcards = "allow"` and
`[licenses.private] ignore = false`, so an init-derived file never checks path dependencies and flags your
own crates as `unlicensed` — use the template in `references/configs.md`.

Both `[package]` tables need `publish = false`. Without it `[licenses.private] ignore = true` does nothing,
your own crates fail as unlicensed, and `allow-wildcard-paths` refuses to cover the `migration` path
dependency because published crates may not have one.

`cargo machete` finds unused dependencies and exits 1 on a finding, which makes it a gate: it runs in the
hook, in the `check` target and in CI. It reads source files, so a crate reachable only through another
crate's feature looks unused. The template exempts eight crates in `[package.metadata.cargo-machete]`
(shipped for code not yet written); delete a name when you start using the crate, and add one only after
deleting the dependency line and watching the build fail.

## Hooks and the dev loop

`prek install` writes the git hook once, `prek run --all-files` runs everything before a pull request, plain
`prek run` checks only what is staged, `prek update` bumps the pinned hook revisions. The local hooks run the
same commands as the Makefile, so a hook cannot pass while CI fails.

`bacon` is the background check loop — `make dev`, then `c` for clippy, `n` for nextest, `d` for docs.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `warning[E0602]: unknown lint` | a typo, or a lint removed in this release | A misspelled name in `[lints]` enforces nothing and fails nothing — no flag or manifest key promotes it. After editing the table, run `cargo clippy` once and read the first lines for `unknown lint`. |
| `lint ... has been removed` | e.g. `string_to_string`, gone in 0.1.98 | Delete the line; use the replacement named in the message. |
| deny: `error[unlicensed]` on your own crate | `publish = false` missing, or `[licenses.private] ignore` is not `true` (init writes `false`) | Both are needed: `publish = false` in the service and `migration/` `[package]` tables, and `[licenses.private] ignore = true` in `deny.toml`. |
| deny: `warning[license-not-encountered]` | an `allow` id no crate in the tree uses | Remove the id; the template lists only ids the stack meets. |
| deny: licenses FAILED on a dependency | its SPDX id is not in `allow` | Confirm the licence is acceptable, then add the id with a comment naming the crate. |
| deny: advisories FAILED, `unmaintained` | scope `"all"` reaches transitive crates you do not control | Narrow to `"workspace"`, or add the RUSTSEC id to `[advisories] ignore` with a reason. |
| deny: parse error, unknown key | 0.20 removed several `[licenses]` keys | Delete the key. `[[licenses.exceptions]]` has no `reason` field either. |
| deny prints a tree of duplicate versions | ~25 duplicates are normal here; the verdict is the last line | Keep `multiple-versions = "warn"`; inspect with `cargo tree -d` before skipping anything. |
| machete flags a crate that is used | reachable only through a feature or a macro | Exempt it in `[package.metadata.cargo-machete]` with a comment. |
| A panicking handler answers a 500 with `"error":"internal server error"` and logs `handler panicked` | the scaffold's `CatchPanicLayer` (tower-http feature `catch-panic`) caught a bug; without it the connection task dies with no response | Fix the panic; `[lints]` warns on `panic!`, `unwrap` and `expect` for this reason. |
| Coverage gate fails with `TOTAL 0` | the ignore regex excluded every file | Narrow the regex; with no lines left the gate cannot pass. |
| `cargo test --doc`: no library targets | binary-only crate | The service keeps `src/lib.rs`; drop the step otherwise. |
| CI recompiles everything in every job | job-wide `RUSTFLAGS` changes both the cargo fingerprint and the rust-cache key | Pass `-D warnings` to clippy instead. See `references/ci.md`. |
| Docker rebuilds all dependencies every time | the chef image tag and `rust-toolchain.toml` name different toolchains | Make the tag match the channel, patch version included (`latest-rust-1.98.1` and `1.98.1`). |
| rustfmt: unstable features are nightly-only | a nightly-only key in `rustfmt.toml` | Remove it; that key is doing nothing. |
