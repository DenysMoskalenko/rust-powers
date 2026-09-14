# Release audit

*The procedure the maintainer follows before a rust-powers release. Repository
documentation for this project, not a shipped skill.*

A release audit inside this repository: compare every pinned version with what is current, read
every crossed changelog, and let only the changes that alter what a skill teaches reach the skill, at
the line that teaches it. Never rewrite a section to modernize its wording; a bump that changes nothing
a skill teaches changes only the version-pin line.

## Important

- Understand first, then make the smallest edit in the right place.
- Read the whole section before editing it (the AGENTS.md rule), not only the line at issue.
- Every edit traces to a changelog line or a compiled probe, never to memory of the crate.
- Smallest diff, in the owning skill; the ownership table in AGENTS.md decides which.
- Snippets compiling is necessary, not sufficient: `make check` proves the API exists, not the teaching.
- A change of philosophy (a major with a different model, a crate replaced by another) is an owner
  decision, never made inside a skill.

## 1. Inventory

Everything pinned, and where it lives:

| Pin | Lives in |
|---|---|
| crate versions | STACK.md `## Cargo.toml` block, add-when-needed comments included; its mirror `skills/rust-scaffolding/assets/app/Cargo.toml` (validator-enforced); `migration/Cargo.toml` (`tokio`, `sea-orm-migration`) drifts silently, check by hand |
| toolchain | `rust-toolchain.toml` channel, `rust-version` in both scaffold `Cargo.toml` files, the cargo-chef tag in the scaffold `Dockerfile` (channel patch included); mirrored in `rust-tooling/references/docker.md`, `configs.md` and STACK.md |
| Docker images | scaffold `docker-compose.yml` (postgres, otel-collector), `ci.yml` service, `Dockerfile` runtime `debian:trixie-slim`; `rust-tooling/references/docker.md` and `ci.md`, optional redis and nats blocks too |
| testcontainers tags | `grep -rn 'with_tag' skills/`: scaffold `tests/common/mod.rs`, the `testing-*.md` references of `sea-orm-postgres`, `rust-redis`, `rust-nats`, and the SKILL.md sentences quoting them |
| cargo tools | scaffold `Makefile` `install-tools`, `.config/nextest.toml` `nextest-version`; versions in STACK.md `### Tooling` and the `rust-tooling` pin line |
| GitHub Actions | `.github/workflows/check.yml`, scaffold `.github/workflows/ci.yml`, `rust-tooling/references/ci.md` (block and `## Action versions`) |
| prek hooks | `rev:` in both `.pre-commit-config.yaml` files, STACK.md, `rust-tooling/references/configs.md` |
| versions quoted in skill bodies | `grep -rn '= "[0-9]' skills/ --include='*.md'` plus the `Assumes ...` pin line at the top of every SKILL.md |

Current versions:

- `cargo info <crate>`: the `version:` line is the latest release (`version: 0.8.9` for axum); skip `-rc`
  and yanked ones (marked in its output).
- Inside the scaffold, with `export CARGO_TARGET_DIR=<scratch>/target-shared CARGO_INCREMENTAL=0`:
  `cargo update --dry-run --workspace --verbose`. `Unchanged X (available: Y)` is a release the resolver
  refused: a new major (`testcontainers v0.27.3 (available: v0.28.0)`) needs research; a compatible
  one held back by `rust-version` (`matchit v0.8.4 (available: v0.8.6)`) waits for the toolchain bump.
- `rustup check`.
- `gh api repos/<o>/<r>/releases/latest --jq .tag_name` for tools and actions; an action without a
  floating major tag (`astral-sh/setup-uv`) needs `matching-refs/tags/v` instead.
- Docker Hub tags, and the `gh` stdin caveat: `sources.md` `## Lookup commands`.

Save it as `tmp/release/<date>-inventory.md` (gitignored):
crate/tool | pinned | current | changelog range | bucket.

## 2. Research each change

Per changed item, in order of authority:

1. The crate's CHANGELOG or GitHub releases for every version between pinned and current, not only the
   top entry; `sources.md` has the paths.
2. docs.rs for every identifier the skills use from that crate (`grep -rn '<crate>::' skills/` lists
   them): each still exists with the same signature.
3. Upstream agent docs: the Agent docs column of `sources.md`; re-check every `none` cell.
4. Rust release notes and the clippy CHANGELOG: new lints that fire under the scaffold's
   `[workspace.lints]` (pedantic on), and edition changes.
5. Postgres, Redis, NATS and collector release notes when an image tag moves.

Classify every finding into exactly one bucket, recorded in the inventory:

| Bucket | Finding | Action |
|---|---|---|
| a | no teaching impact | pin line only |
| b | rename, removal, signature change | snippet fix plus a row in that skill's prior-version correction table |
| c | behaviour change | prose fix at the one place that teaches it |
| d | new feature replacing a workaround we teach | a proposal for the owner in `tmp/release/`; do not apply |
| e | new lint or toolchain change that breaks the gate | fix scaffold and snippets; mirror any changed config file into `rust-tooling` |
| f | philosophy change | stop and ask |

## 3. Locate the place

For each (b), (c) and (e) row: grep the identifier or the claim across `skills/` and the scaffold, list
every hit as `file:line`, read each whole section, then pick one owning location from the AGENTS.md
ownership table. A fact in both a reference and its SKILL.md is fixed in both; never add a third copy.
A fact that lives only in a Red Flags or correction-table row stays there.

The two shapes a bump edits:

- The pin line, `skills/axum-service/SKILL.md` line 10:
  `Assumes Rust 1.98 edition 2024, axum 0.8, tower-http 0.7, ... jsonwebtoken 11, argon2 0.6.`
- The correction table, `skills/sea-orm-postgres/SKILL.md` under `## sea-orm 2.0 is not 1.x`:

  ```markdown
  | 1.x | 2.0 |
  |---|---|
  | `Column::Field.eq(..)` in a filter | `Entity::COLUMN.field.eq(..)` |
  ```

  A (b) row is appended in that shape: old spelling left, new right, plus what the old one does when it
  still compiles. `axum-service` has `## axum 0.7 to 0.8 corrections`, same layout.

## 4. Apply

The bump itself is AGENTS.md `## Versions`; the order across a release:

1. STACK.md: the `## Cargo.toml` block, prose naming the old version, the `Re-validated on ...`
   sentence at the top.
2. The scaffold `Cargo.toml` mirror, then `cargo update` there with `CARGO_TARGET_DIR` exported to a
   scratch directory and `CARGO_INCREMENTAL=0`; stage `Cargo.lock`. Every cargo run inside the scaffold
   (`make check`, `make test`, `make cov`, the root `prek run --all-files` through the nested config)
   writes `assets/app/target` unless `CARGO_TARGET_DIR` is exported; delete it afterwards.
3. `rust-tooling` references re-mirrored byte-for-byte; `diff` each block against the scaffold file.
4. Skill pin lines, correction-table rows, snippets.
5. `evals/<skill>.md` in the same commit whenever what a good answer looks like changed.
6. `metadata.version` in every skill whose teaching changed: patch for wording, minor for an API change.
7. The release `version` in the three `plugin.json` manifests and `.claude-plugin/marketplace.json`
   `metadata.version`; a toolchain bump also edits `Rust 1.98 edition 2024` in the `marketplace.json`
   plugin `description` and `.codex-plugin/plugin.json` `interface.longDescription`; README only if it
   names the version.

A Postgres, Redis or NATS major moves in every place `sources.md` lists for it - the compose file,
the testcontainers tag, the CI service, and the skill prose that names the version - in one commit,
never one place at a time.

Editing rules: no reflow, no touching the neighbouring sentence, keep every deliberately worded warning.
`rust-code-style`, `axum-service` and `rust-redis` SKILL.md sit within 40 words of the 2000-word hard
cap: trim before adding. Never edit `verify/`; it is regenerated.

## 5. Verify

- `make check` with Docker running, so the snippet tests run, not `--skip-tests`.
- In the scaffold, `make check && make test`; at the root, `prek run --all-files`.
- The exercise: scaffold a throwaway service from the `rust-scaffolding` text alone in a scratch
  directory, run `make check && make test`, delete it.
- `grep -rn '<crate>'` and read each hit for the old number (a bare `0.27` also hits
  `rstest 0.27`): it survives only in correction tables and STACK.md history.
- Containers: only the reusable `rust-powers-test-postgres`, `-redis`, `-nats`; `docker ps -a | wc -l`
  is equal before and after. Report `du -sh` of the scratch directory after cleanup.

## 6. Review and release

An independent, fresh-context review of the diff against the inventory table: every (b) and (c) row
has exactly one edit, no edit lacks a row. Show the complete diff; the pull request opens only on
the owner's explicit approval, its body the inventory table plus what was verified. After merge, tag
`vX.Y.Z` matching the manifests and update the `Decided` and `Re-validated on` date lines in STACK.md.

## Red Flags — STOP

| Signal | Instead |
|---|---|
| Rewriting a section because it reads better | Revert; only the line the changelog invalidates changes. |
| A version bumped in a skill body only | STACK.md and the scaffold mirror first, the pin line last. |
| A compile pass taken as proof the teaching is right | Read the prose beside the block against the changelog entry. |
| A changelog example pasted into a snippet | Run it under the scaffold's lints first; pedantic rejects most upstream examples. |
| A new crate appearing in a skill | Stop; it needs a STACK.md `## Decisions` entry and a `## Rejected` row first. |
| A rename fixed in a snippet with no correction-table row | Add the row; it saves the reader arriving with the old spelling. |
| An edit outside the owning skill | Move it; the sibling gets a one-sentence cross-reference at most. |
| `verify/` edited | Discard; edit the markdown, re-run `make check`. |
| An action pinned to an unverified floating major tag | `matching-refs/tags/v` first; `setup-uv` has none. |
| A bump run while a fresh test container was being created | Stop, `docker ps -a`, remove everything but the three reusable ones. |

## Sources

`sources.md`, beside this file, lists the changelog, API docs and agent docs for every pinned
item, verified on 14 September 2026, plus the lookup commands. Open it in steps 1 and 2.
