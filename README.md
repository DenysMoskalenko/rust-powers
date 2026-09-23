# rust-powers

The complete and highly opinionated skillset for web development in the Rust language.

This repository is a shared playbook for humans and coding agents building Rust web
services. It makes one pick per concern - axum, sea-orm, PostgreSQL, nextest, OpenTelemetry,
rig, NATS, Redis - and captures the conventions that go with them, so a team stops
re-deciding the same things and AI-assisted changes come out predictable.

The main artifact is the [`skills/`](skills/) directory. Each skill is a focused Markdown
guide an agent loads when working in that domain. [`STACK.md`](STACK.md) is the single
source of truth for every version and library choice. python-powers is the sibling project
this repository mirrors for Python services.

## What's Inside

| Skill | Use it for |
| --- | --- |
| [`rust-code-style`](skills/rust-code-style/SKILL.md) | Ownership and cloning discipline, `thiserror` vs `anyhow`, no-panic rules, newtypes, async discipline, module layout, and edition 2024 idioms. |
| [`axum-service`](skills/axum-service/SKILL.md) | Routes, extractors, validated bodies, `AppError` and status mapping, utoipa and Swagger UI, settings, middleware order, health checks, pagination, SSE, graceful shutdown, JWT auth, and observability: `tracing` logs, OTLP export, `traceparent` propagation, Prometheus metrics. |
| [`sea-orm-postgres`](skills/sea-orm-postgres/SKILL.md) | Dense entities, typed column filters, pagination, eager loading and N+1 diagnosis, transactions, `sea-orm-migration`, pool sizing, and per-test database isolation. |
| [`rust-testing`](skills/rust-testing/SKILL.md) | `axum-test` API tests, the `test_app()` helper, `rstest` fixtures, `fake` and `bon` factories, `httpmock`, `insta` snapshots, an injected `Clock`, nextest filters, coverage gates. |
| [`rust-tooling`](skills/rust-tooling/SKILL.md) | `rust-toolchain.toml`, the `[workspace.lints]` table, `clippy.toml`, `rustfmt.toml`, nextest and llvm-cov config, cargo-deny, cargo-machete, bacon, prek, Makefile, Docker, and CI. |
| [`rust-scaffolding`](skills/rust-scaffolding/SKILL.md) | Generating a brand-new axum service from a buildable template: migrations, tests, lints, Docker, and CI green from the first commit. Greenfield only. |
| [`building-rig-agents`](skills/building-rig-agents/SKILL.md) | rig agents in an axum service - agent builder, tools, structured extraction, streaming, RAG, conversation history, rmcp clients, and offline testing. |
| [`rust-nats`](skills/rust-nats/SKILL.md) | async-nats in an axum service - core publish/subscribe/request, JetStream streams and durable pull consumers, the consumer loop, shutdown, readiness, and NATS tests. |
| [`rust-redis`](skills/rust-redis/SKILL.md) | redis-rs in an axum service - the `ConnectionManager`, cache-aside with TTLs, distributed locks, rate limiting, idempotency storage, readiness, and Redis tests. |

Some skills include additional reference material linked from their main guide.

## How To Use

1. Pick the skill that matches the task.
2. Load its `SKILL.md` before editing code.
3. Load related skills only when their domain is actually involved.
4. Follow the ownership boundaries in each skill instead of mixing unrelated rules.

Typical combinations:

| Task | Skills |
| --- | --- |
| Start a brand-new service from scratch | `rust-scaffolding` (then the domain skills it hands off to) |
| Add or refactor Rust application code | `rust-code-style` |
| Build an endpoint backed by PostgreSQL | `rust-code-style`, `axum-service`, `sea-orm-postgres`, `rust-testing` |
| Add an AI assistant endpoint to a service | `rust-code-style`, `axum-service`, `building-rig-agents`, `rust-testing` |
| Publish or consume messages, add a worker | `rust-nats`, `axum-service`, `rust-testing` |
| Cache, lock, rate-limit, or make a POST idempotent | `rust-redis`, `axum-service`, `rust-testing` |
| Wire or debug logs, traces, and metrics | `axum-service` |
| Change lints, dependencies, or test commands | `rust-tooling` |
| Edit one of this repository's skills | [`AGENTS.md`](AGENTS.md) plus the skill being changed |

## Install As A Claude Code Plugin

The plugin manifest lives at [`.claude-plugin/plugin.json`](.claude-plugin/plugin.json).

**From GitHub:**

```bash
claude plugin marketplace add https://github.com/DenysMoskalenko/rust-powers
claude plugin install rust-powers@rust-powers
```

**Local development:** install from a clean clone. A local install copies the directory
whole, so a working copy with build output in it copies gigabytes.

```bash
git clone https://github.com/DenysMoskalenko/rust-powers
claude plugin marketplace add ./rust-powers
claude plugin install rust-powers@rust-powers
```

Installed plugins update when the plugin `version` changes, which every change to `skills/`
bumps.

After installation, the skills listed above are available to Claude Code via the `Skill`
tool.

## Install As A Codex Plugin

This repository is a Codex plugin marketplace. The marketplace file lives at
[`.agents/plugins/marketplace.json`](.agents/plugins/marketplace.json), and the plugin
manifest lives at [`.codex-plugin/plugin.json`](.codex-plugin/plugin.json).

**Codex app:**

Open **Plugins**, add this GitHub marketplace, then install and enable **Rust Powers**:

```text
https://github.com/DenysMoskalenko/rust-powers
```

**Codex CLI:**

```bash
codex plugin marketplace add DenysMoskalenko/rust-powers
codex plugin add rust-powers@rust-powers
```

Start a new Codex thread after installation so the skills are loaded.

## Install As A Cursor Plugin

This repository is also importable as a Cursor plugin. The Cursor plugin manifest lives at
[`.cursor-plugin/plugin.json`](.cursor-plugin/plugin.json) and points Cursor at the root
[`skills/`](skills/) directory.

In a Cursor Agent chat, run:

```text
/add-plugin rust-powers@https://github.com/DenysMoskalenko/rust-powers
```

For local testing, copy a clean clone of this repository to the path below. Cursor skips a
symlink that points outside that folder.

```text
~/.cursor/plugins/local/rust-powers
```

Then restart Cursor or run `Developer: Reload Window`.

## Other Agents

Any agent that reads Agent Skills can install the nine skills directly:

```bash
npx skills add DenysMoskalenko/rust-powers
```

## Development

Skill content is verified, not just proofread.

```bash
make check      # validate + snippets, what CI runs
make validate   # frontmatter, budgets, ownership hygiene, evals, STACK.md parity
make snippets   # compile and lint every ```rust,verify block, run every verified test
make lint       # prek run --all-files
make evals      # billed: run evals/ with and without the plugin via claude plugin eval
```

[`.github/workflows/check.yml`](.github/workflows/check.yml) runs the same two scripts —
`validate_skills.py --strict` and `check_snippets.py` — plus `prek run --all-files`, on every
push to `main` and every pull request.

`make snippets` regenerates the gitignored `verify/` workspace from
`skills/rust-scaffolding/assets/app/`, extracts every verifiable snippet into it, and runs
`cargo check`, `cargo clippy -- -D warnings`, and `cargo nextest run`. The database-backed
tests need a running Docker daemon or a `TEST_DATABASE_URL` pointing at a PostgreSQL the
test user may create databases in; without either, run
`python3 scripts/check_snippets.py --skip-tests`.

[`AGENTS.md`](AGENTS.md) (symlinked as `CLAUDE.md`) has the full house conventions and the
checklist for adding or changing a skill. Per-skill acceptance scenarios live in
[`evals/`](evals/); `make evals` runs them against a model with and without the plugin, which
costs real model calls, so it runs before a release rather than in CI. A `tmp/` directory, when present, holds local research and review notes;
it is untracked and not part of the plugin.

[`maintenance/release-audit.md`](maintenance/release-audit.md) is the release procedure the
maintainer follows before tagging a version; it is repository documentation, not a shipped skill.

## Principles

- One pick per concern, recorded in `STACK.md`, with the rejected alternatives written down.
- Clear ownership over duplicated rules.
- Examples that compile, because CI compiles them on every push and pull request.
- Skills terse enough for an agent to load and follow.
- Supporting material in `references/` when it would bloat the main skill.

## Contributing

Good contributions clarify an existing pattern, correct one that has drifted, reduce overlap
between skills, or add reference material that supports an existing skill. Avoid
app-specific conventions, one-off team workflows, unproven library recommendations, and
claims about CI or release processes that are not represented here.

When changing a skill, read [`AGENTS.md`](AGENTS.md) first and keep the edit scoped to that
skill's ownership.

## License

This project is licensed under the [Apache License 2.0](LICENSE).
