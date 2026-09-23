# Rust Powers - Agent Instructions

## Repository Identity

This repository is a shared, opinionated playbook for building web services in Rust. The
only shipped artifact is the `skills/` tree; everything else exists to keep those skills
accurate, portable, and installable in Claude Code, Codex, and Cursor.

`STACK.md` is the single source of truth for versions and library choices. It is not a
skill and is never quoted from inside one.

## The Skills

Nine skills, each owning its domain exclusively. If a rule belongs to a sibling, link the
sibling by name in one sentence instead of restating the rule.

| Skill | What it is | Owner boundary |
| --- | --- | --- |
| `rust-code-style` | Ownership and cloning, `thiserror` vs `anyhow`, no-panic rules, newtypes, `derive_more` and `strum`, async discipline, module layout, edition 2024 idioms. | Language and architecture only; never lint configuration, framework patterns, or test layout. |
| `axum-service` | Routes, extractors, `Valid<T>`, response DTOs, `AppError` and status mapping, utoipa and Swagger UI, settings and secrets, middleware order, health and readiness, pagination, SSE, background work, graceful shutdown, JWT auth, and all observability: `tracing` layers, OTLP export, `traceparent` propagation, Prometheus metrics. | The whole HTTP layer and its instrumentation; queries belong to `sea-orm-postgres`. |
| `sea-orm-postgres` | Dense entities, typed column filters, pagination queries, eager loading and N+1, transactions, `sea-orm-migration`, `sea-orm-cli generate entity`, pool sizing, per-test database isolation. | Everything below the handler; never routes or DTOs. |
| `rust-testing` | `axum-test` API tests, the `test_app()` helper, `rstest` fixtures, `fake` and `bon` factories, `httpmock`, `insta`, the injected `Clock`, nextest filters, coverage gates. | Test authoring only; the Postgres container lives in `sea-orm-postgres`, rig mocks in `building-rig-agents`. |
| `rust-tooling` | `rust-toolchain.toml`, `cargo add` and the lockfile, the `[workspace.lints]` table, `clippy.toml`, `rustfmt.toml`, nextest and llvm-cov config, cargo-deny, cargo-machete, bacon, prek, the Makefile, Dockerfile, compose, CI. | Configuration files and their contents; fixing one clippy finding is `rust-code-style`. |
| `rust-scaffolding` | Creating a brand-new axum service from zero, from the buildable template in `assets/app/`. | Greenfield only; changing tooling in an existing service is `rust-tooling`. It is the only place the initial file set lives. |
| `building-rig-agents` | rig agent builder, tools, structured extraction, streaming, RAG, conversation history, rmcp clients, mapping `PromptError` onto `AppError`. | Agent internals only; the endpoint around it is `axum-service`. |
| `rust-nats` | async-nats: core publish/subscribe/request, JetStream streams and durable pull consumers, the consumer loop, the client in `AppState`, the readiness entry, NATS tests. | Messaging only; the HTTP handler that publishes is `axum-service`, the Postgres side of an outbox is `sea-orm-postgres`. |
| `rust-redis` | redis-rs: the `ConnectionManager`, cache-aside with TTLs, distributed locks, Redis-backed rate limiting and idempotency storage, the readiness entry, Redis tests. | Cache and coordination only; where the rate-limit layer sits in the stack is `axum-service`'s, the idempotency contract is `axum-service`'s. |

Two contracts every add-on skill (rig, NATS, Redis) follows instead of restating: `AppError`
never grows a variant — an add-on maps its errors onto the existing ones (`Unavailable` 503
for a backend that is down, `TooManyRequests` 429 for a rate limit, `Other` 500 for a wiring
error, `BadRequest` 400 for bad input; no 502 except the existing `Http`). Readiness is
`GET /health/ready` returning `{ "status": "ok" | "degraded" | "unavailable", "checks": { "<name>": ... } }`
— a required dependency (the database) failing is 503 + `unavailable`, an optional one
(cache, messaging) is 200 + `degraded`; an add-on adds one entry under `checks`. Both are
owned by `axum-service` and implemented in the scaffold.

## House Conventions

**Frontmatter.** Exactly `name`, `description`, and `metadata` with a `version` line
(bumped with every change to the skill, together with the plugin version; see step 6 below). `name` is lowercase
kebab-case and identical to its directory name, 64 characters or fewer, and never contains
`claude` or `anthropic`. No `<`, `>`, or `#` anywhere in frontmatter - they break Codex and
Cursor discovery.

**Description.** Double-quoted, at most 75 words, third person, and it starts with
`"Use when "`. Follow that with the concrete triggers: the task shapes, the error strings an
agent will paste (`Handler is not satisfied`, `RecordNotUpdated`, `E0502`), and the crate and
API names a reader arrives with (`LoaderTrait`, `ConnectionManager`, `AgentBuilder`). End with the
negatives, aimed at the words that collide with a sibling's description (flaky, coverage,
bootstrapping, migrations), each naming the skill that wins. The description is the entire
triggering mechanism - it is loaded for every session while the body is not, so it earns
more editing time than any other line in the skill.

**Budgets and shape.** `SKILL.md` targets 250 lines and 1500 words; 300 lines and 2000 words
is the hard cap that fails validation. Claude Code keeps only the first 5,000 tokens of an
invoked skill after compaction, and a SKILL.md runs about 2.5 bytes per token, so size is
measured in tokens as well as words. Open with a one-line version pin naming only the crates
that skill touches, then the scope block below, then a `## Important` block of three to five
non-negotiable bullets, then the `## References` index, so the index survives compaction.
Discipline skills end with a `## Red Flags — STOP` table and, where one exists, a
prior-version correction table (sea-orm 1.x to 2.0, axum 0.7 to 0.8, edition 2021 to 2024).
Technique skills - `rust-tooling`, `rust-scaffolding` - have neither.

**Scope block.** Current models apply rules literally, including to code a task did not ask
them to change. Every skill except the greenfield `rust-scaffolding` carries this paragraph
verbatim after its version pin:

> Names such as `AppError`, `test_app()`, `Valid<T>` and the Makefile targets come from the rust-scaffolding template. In a project built differently, use its own types, helpers and tooling, map outcomes onto its nearest existing error variant, and say so when none fits instead of adding one. Apply these rules to new code; when editing existing code, keep its public contract and tuned configuration and report differences instead of rewriting, unless asked. If `Cargo.lock` pins another major or minor version than the line above, follow the project and say which rules may not apply.

**What earns a place in `## Important` and Red Flags.** A failure that reproduces on a model the
skills support: an eval case where the skill makes a difference, or a recorded incident. House
conventions the model cannot guess and API facts newer than its training usually qualify;
general practice a current model already follows usually does not. The skills target current
models first (Opus 5.5, Fable 5.1, GPT-6) and must keep working on older ones, so general
advice that only older models need moves to a one-line Red Flags row rather than disappearing.
Say each rule once, give its reason in one clause, and prefer what to do over what to avoid.

**References.** One level deep under `references/`, plural, no size cap. A reference over
100 lines needs a `Contents` heading or an anchor list in its first 20 lines. Every
`SKILL.md` with a `references/` directory carries a `## References` index with one line per
file saying when to open it - `SKILL.md` is the only index, so a reference never links to
another reference (the validator fails on such a link, in this skill or any other). No
`README.md` inside a skill directory. Skills ship instructions, not scripts: an agent follows a
checklist it can read and never runs a shell script it cannot see, so no `scripts/`
directory inside a skill.

**Fence tags.** Only these are verifiable:

- ` ```rust,verify ` - a complete, self-contained item set, imports included. Extracted to a
  bin, compiled, and linted. It may refer to the scaffold crate as `crate::...`; the
  extractor rewrites that to `app::...`.
- ` ```rust,verify,test ` - a complete test file body. Extracted to an integration test and
  run. It may `mod common;` to reach `test_app()`.
- ` ```rust,ignore ` - deliberately non-compiling: bad examples and counter-examples.
- ` ```rust ` - a fragment. Not extracted and not verified, so keep fragments short and push
  anything load-bearing into a tagged block.

Prefer tagged blocks in references and short fragments in `SKILL.md`.

**Ownership.** Exclusive. Cross-reference a sibling by name, never by repeating its rules.
The one deliberate exception is `rust-tooling`, which repeats config file contents inline,
byte-for-byte equal to the scaffold's, so it is readable without opening the scaffold.

**No repository plumbing inside skills.** A skill is read inside somebody else's project,
where `verify/`, the repository's `scripts/`, `evals/`, `make validate`, `make snippets`, `tmp/`, and `STACK.md`
do not exist.
Mentioning them is a validation error, with no exemptions. Version facts belong in the
skill's own version-pin line, not in a pointer to `STACK.md`.

**Codex metadata.** Each skill carries `agents/openai.yaml` with `interface.display_name`,
`interface.short_description`, and `policy.allow_implicit_invocation`.

## Evaluation Scenarios

Every skill has `evals/<skill>.md` at the repository root. It holds one `### Triggering`
block - prompts that should load the skill, and near misses that should not, each of those
naming the skill that should win instead - at least three `Eval N` blocks of Prompt / Must
produce / Must not produce, and `Probe N` blocks that each test one rule with a wrong-answer
regex. `evals/README.md` has the exact format, and the validator parses it. An eval never
contradicts the skill it tests or a sibling skill.

`make evals` generates `claude plugin eval` cases from these files into the gitignored
`evals/.run/` and runs every eval and probe with and without the plugin, so each case reports
what the skill adds. Runs are billed model calls: run the suite before a release, when a new
model ships, and after a description change, never in CI. Keep the per-case summary that
justifies an `## Important` bullet or a Red Flags row under `maintenance/`.

A behaviour change that changes what a good agent response looks like updates the matching
eval file in the same commit. Drafts written while authoring live in `tmp/evals/` and are
moved into `evals/` during integration; only `evals/` is tracked.

## Verification

`make check` is `make validate` followed by `make snippets`. Both must be green.

`.github/workflows/check.yml` runs both of those scripts, plus `prek run --all-files`, on every
push to `main` and every pull request.

`make validate` runs `scripts/validate_skills.py`: frontmatter parses and uses only the
three allowed keys with `metadata.version` present, `name` matches its directory, the
description is double-quoted, at most 75 words and free of forbidden characters, line and
word budgets, no skill `README.md` or `scripts/`, reference depth and TOCs, no reference-to-reference
links, fence tags in the allowed set, the `## References` index, `agents/openai.yaml`
presence, `evals/<skill>.md` presence and format, no repository plumbing and no Python in
skill text, equal versions in every plugin manifest, no build output inside `skills/`, and
that the `[dependencies]`, `[dev-dependencies]`, and `[workspace.lints]` tables in
`skills/rust-scaffolding/assets/app/Cargo.toml` still match STACK.md's `## Cargo.toml`
block. Warnings do not fail; errors exit 1. `--strict` promotes a missing eval file to an
error, `--json` emits findings as JSON, and `--base <ref>` fails when a skill changed since
that ref without a version bump (CI passes the pull request's base branch).

`make snippets` runs `scripts/check_snippets.py`: it deletes and regenerates `verify/` as a
copy of `skills/rust-scaffolding/assets/app/`, writes every `rust,verify` block into
`verify/src/bin/` and every `rust,verify,test` block into `verify/tests/`, appends the
add-when-needed crates to `verify/Cargo.toml`, then runs `cargo check --workspace
--all-targets`, `cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo nextest run --workspace`. When cargo fails it prints which `skill/file.md:line` the
failing bin came from. `--dry-run` extracts and summarizes without writing or compiling;
`--skip-tests` stops after clippy.

The database-backed tests need either a running Docker daemon - testcontainers starts
Postgres 18-alpine - or `TEST_DATABASE_URL` pointing at a Postgres where the test user may
create databases. Without one of those, use `--skip-tests` and say so in your report.

`verify/` is generated and gitignored. Never edit it; edit the markdown and re-run. The
Makefile builds into `$CARGO_TARGET_DIR`, by default `rust-powers-target` in the system temp
directory, because the plugin root is the repository root and a local install copies it whole;
never leave a `target/` inside the repository.

`make lint` runs `prek run --all-files` for whitespace, JSON, TOML, and YAML hygiene.

## Adding or Changing a Skill

1. **Research.** Confirm the topic has at least three distinct recurring triggers and that
   no sibling already owns it. Narrow topics extend an existing skill instead of becoming
   one. Read the current `SKILL.md` in full before editing it - the complete section, not
   only the line you were asked about. If the requested change is unnecessary or wrong, say
   so instead of making it.
2. **Write.** Follow the budgets and section order above. Prefer one excellent example over
   three mediocre ones. Preserve deliberately worded warnings and comments unless the change
   makes them irrelevant.
3. **Tag fences.** Anything a reader will copy gets `rust,verify` or `rust,verify,test`.
   Anything deliberately wrong gets `rust,ignore`.
4. **Evals.** Add or update `evals/<skill>.md` in the same change.
5. **`make check`.** Green before review. Without Docker or `TEST_DATABASE_URL`, run
   `python3 scripts/check_snippets.py --skip-tests` and report that the test leg was skipped.
6. **Version.** Any change under `skills/<name>/` bumps that skill's `metadata.version` and the
   `version` in every plugin manifest (`.claude-plugin/plugin.json`, both version fields of
   `.claude-plugin/marketplace.json`, `.codex-plugin/plugin.json`,
   `.cursor-plugin/plugin.json`). Installed plugins auto-update only when that version changes.
7. **Review.** Re-read the changed section end to end. Verify every relative path you touched
   exists. Update the rest of the plugin manifests only when the change alters what the
   plugin advertises.

## Versions

`STACK.md` is the single source of truth. To bump a dependency:

1. Edit the version in the `## Cargo.toml` block in `STACK.md`, plus any prose in that file
   that names the old version.
2. Mirror the change in `skills/rust-scaffolding/assets/app/Cargo.toml`.
3. Run `cargo update` inside `skills/rust-scaffolding/assets/app/` and commit the lockfile.
4. Run `make check`. The validator fails if the two Cargo tables have drifted; the snippet
   run fails if a documented snippet no longer compiles. Mirror any changed config block into
   `rust-tooling`'s references, which repeat the scaffold's files verbatim.
5. If the new version changes an API a skill teaches, update that skill's version-pin line
   and add a row to its prior-version correction table in the same commit.

Never bump a version only in a skill body. A version pinned in a skill and nowhere else is a
lie the next reader will trust. The full release procedure (inventory, research, locate, apply,
verify, review) is `maintenance/release-audit.md`, with `maintenance/sources.md` beside it.
`maintenance/` is repository documentation - outside `skills/`, unseen by the validator, never
shipped by the plugin manifests.

## What Belongs Here

Acceptable: conventions that generalize across Rust web services, corrections that make a
skill more accurate or less overlapping, reference material that supports a skill without
bloating its `SKILL.md`, and repository docs explaining how to use the skills.

Not acceptable: app-specific or team-private conventions, one-off workflows, new libraries
without a decision recorded in `STACK.md` first, formatting-only churn across many skills,
and claims about CI, review, or deployment that this repository does not actually have.

## Working Here

Read `README.md`, this file, and the relevant `SKILL.md` before editing. Check
`git status --short` and preserve existing working-tree changes. Keep changes scoped to this
repository's docs and skill content. Report what you changed and what you verified. Stage
files you create with `git add`; commits are made by the repository owner. This repository is
hosted on GitHub - say "pull request", show the complete diff, and get explicit approval
before opening one.
