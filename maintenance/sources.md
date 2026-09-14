# Sources per pinned item

Where each pinned item is declared in this repository, where its changes are announced, where its API
is documented, and whether upstream ships agent-facing docs. Every GitHub path below was checked with
`gh api repos/<o>/<r>/contents/<path>` on 14 September 2026; "releases" means the repository publishes
no changelog file and the GitHub releases page is the record. "none" under Agent docs means no
`CLAUDE.md`, `AGENTS.md` or `llms.txt` at the repository root on that date; "(contributor-facing)" marks
files that describe the repository itself, not how to use the crate or tool. Re-check on each release.
Companion to `release-audit.md`, which reads this table in its inventory and research steps.

## Contents

- [Toolchain](#toolchain)
- [Web](#web)
- [API](#api)
- [Config](#config)
- [Database](#database)
- [Observability](#observability)
- [Testing](#testing)
- [Tooling](#tooling)
- [Add when needed](#add-when-needed)
- [Servers](#servers)
- [Actions and hooks](#actions-and-hooks)
- [Lookup commands](#lookup-commands)

## Toolchain

| Item | Pinned where | Changelog / releases | API docs | Agent docs |
|---|---|---|---|---|
| rustc | scaffold `rust-toolchain.toml`, `Cargo.toml` `rust-version`, STACK.md, `rust-tooling/references/configs.md`, scaffold `Dockerfile` chef tag | https://github.com/rust-lang/rust/blob/main/RELEASES.md, https://releases.rs/ | https://doc.rust-lang.org/std/ | `CLAUDE.md`, `AGENTS.md` (contributor-facing) |
| clippy | same channel; lint table in STACK.md and scaffold `Cargo.toml` | https://github.com/rust-lang/rust-clippy/blob/master/CHANGELOG.md | https://rust-lang.github.io/rust-clippy/master/index.html | none |
| rustfmt | scaffold `rustfmt.toml` | https://github.com/rust-lang/rustfmt/blob/main/CHANGELOG.md | https://rust-lang.github.io/rustfmt/ | none |
| edition 2024 | `Cargo.toml` `edition`, every `Assumes ...` line | https://doc.rust-lang.org/edition-guide/ | same | none |

## Web

| Item | Pinned where | Changelog / releases | API docs | Agent docs |
|---|---|---|---|---|
| axum | STACK.md, scaffold `Cargo.toml`, `axum-service` pin line | https://github.com/tokio-rs/axum/blob/main/axum/CHANGELOG.md | https://docs.rs/axum | none |
| tower-http | same | https://github.com/tower-rs/tower-http/blob/main/tower-http/CHANGELOG.md | https://docs.rs/tower-http | none |
| tower | same | https://github.com/tower-rs/tower/blob/master/tower/CHANGELOG.md | https://docs.rs/tower | none |
| hyper | transitive, lock file only | https://github.com/hyperium/hyper/blob/master/CHANGELOG.md | https://docs.rs/hyper | none |
| tokio | STACK.md, scaffold `Cargo.toml`, `rust-code-style` pin line | https://github.com/tokio-rs/tokio/blob/master/tokio/CHANGELOG.md | https://docs.rs/tokio | none |
| reqwest | STACK.md, scaffold `Cargo.toml`, `axum-service` pin line | https://github.com/seanmonstar/reqwest/blob/master/CHANGELOG.md | https://docs.rs/reqwest | none |

## API

| Item | Pinned where | Changelog / releases | API docs | Agent docs |
|---|---|---|---|---|
| utoipa | STACK.md, scaffold `Cargo.toml`, `axum-service` pin line | https://github.com/juhaku/utoipa/blob/master/utoipa/CHANGELOG.md | https://docs.rs/utoipa | none |
| utoipa-axum | same | https://github.com/juhaku/utoipa/blob/master/utoipa-axum/CHANGELOG.md | https://docs.rs/utoipa-axum | none |
| utoipa-swagger-ui | same | https://github.com/juhaku/utoipa/blob/master/utoipa-swagger-ui/CHANGELOG.md | https://docs.rs/utoipa-swagger-ui | none |
| validator | same | https://github.com/Keats/validator/blob/master/CHANGELOG.md | https://docs.rs/validator | none |
| serde | STACK.md, scaffold `Cargo.toml` | https://github.com/serde-rs/serde/releases | https://serde.rs/, https://docs.rs/serde | none |

## Config

| Item | Pinned where | Changelog / releases | API docs | Agent docs |
|---|---|---|---|---|
| config | STACK.md, scaffold `Cargo.toml`, `axum-service` pin line | https://github.com/rust-cli/config-rs/blob/main/CHANGELOG.md | https://docs.rs/config | none |
| dotenvy | STACK.md, scaffold `Cargo.toml` | https://github.com/allan2/dotenvy/blob/main/CHANGELOG.md | https://docs.rs/dotenvy | none |
| secrecy | STACK.md, scaffold `Cargo.toml`, `axum-service` pin line | https://github.com/iqlusioninc/crates/blob/main/secrecy/CHANGELOG.md | https://docs.rs/secrecy | none |

## Database

| Item | Pinned where | Changelog / releases | API docs | Agent docs |
|---|---|---|---|---|
| sea-orm | STACK.md, scaffold `Cargo.toml`, `sea-orm-postgres` pin line and correction table, `rust-testing` pin line | https://github.com/SeaQL/sea-orm/blob/master/CHANGELOG.md | https://docs.rs/sea-orm, https://www.sea-ql.org/SeaORM/docs/index/ | https://github.com/SeaQL/sea-orm/blob/master/CLAUDE.md |
| sea-orm-migration | STACK.md prose, scaffold `migration/Cargo.toml`, `sea-orm-postgres` pin line | same CHANGELOG.md (one file for the workspace) | https://docs.rs/sea-orm-migration | same CLAUDE.md |
| sea-orm-cli | STACK.md `### Tooling` version line, scaffold `Makefile` | same CHANGELOG.md | https://www.sea-ql.org/SeaORM/docs/generate-entity/sea-orm-cli/ | same CLAUDE.md |
| sqlx | transitive through sea-orm; STACK.md names the major | https://github.com/launchbadge/sqlx/blob/main/CHANGELOG.md | https://docs.rs/sqlx | none |

## Observability

| Item | Pinned where | Changelog / releases | API docs | Agent docs |
|---|---|---|---|---|
| tracing, tracing-subscriber | STACK.md, scaffold `Cargo.toml` | https://github.com/tokio-rs/tracing/blob/main/tracing/CHANGELOG.md | https://docs.rs/tracing | none |
| tracing-opentelemetry | STACK.md version set, scaffold `Cargo.toml` | https://github.com/tokio-rs/tracing-opentelemetry/blob/v0.1.x/CHANGELOG.md | https://docs.rs/tracing-opentelemetry | none |
| opentelemetry, opentelemetry_sdk | same, `axum-service` pin line | https://github.com/open-telemetry/opentelemetry-rust/blob/main/opentelemetry/CHANGELOG.md, https://github.com/open-telemetry/opentelemetry-rust/blob/main/opentelemetry-sdk/CHANGELOG.md, https://github.com/open-telemetry/opentelemetry-rust/releases | https://docs.rs/opentelemetry | `CLAUDE.md`, `AGENTS.md` |
| opentelemetry-otlp | same | https://github.com/open-telemetry/opentelemetry-rust/blob/main/opentelemetry-otlp/CHANGELOG.md | https://docs.rs/opentelemetry-otlp | same |
| axum-tracing-opentelemetry | same | https://github.com/davidB/tracing-opentelemetry-instrumentation-sdk/blob/main/CHANGELOG.md | https://docs.rs/axum-tracing-opentelemetry | none |
| reqwest-middleware, reqwest-tracing | same | https://github.com/TrueLayer/reqwest-middleware/blob/main/reqwest-middleware/CHANGELOG.md, https://github.com/TrueLayer/reqwest-middleware/blob/main/reqwest-tracing/CHANGELOG.md | https://docs.rs/reqwest-middleware, https://docs.rs/reqwest-tracing | none |
| axum-prometheus | same | https://github.com/Ptrskay3/axum-prometheus/blob/master/CHANGELOG.md | https://docs.rs/axum-prometheus | none |
| metrics | same | https://github.com/metrics-rs/metrics/blob/main/metrics/CHANGELOG.md | https://docs.rs/metrics | none |

## Testing

| Item | Pinned where | Changelog / releases | API docs | Agent docs |
|---|---|---|---|---|
| rstest | STACK.md, scaffold `Cargo.toml`, `rust-testing` pin line | https://github.com/la10736/rstest/blob/master/CHANGELOG.md | https://docs.rs/rstest | `CLAUDE.md` |
| axum-test | same | https://github.com/JosephLenton/axum-test/releases | https://docs.rs/axum-test | `CLAUDE.md` |
| testcontainers-modules | STACK.md, scaffold `Cargo.toml`, `testing-*.md` references | https://github.com/testcontainers/testcontainers-rs-modules-community/blob/main/CHANGELOG.md | https://docs.rs/testcontainers-modules | none |
| testcontainers | same | https://github.com/testcontainers/testcontainers-rs/blob/main/CHANGELOG.md | https://docs.rs/testcontainers | none |
| fake | STACK.md, scaffold `Cargo.toml`, `rust-testing` pin line and `references/factories.md` (rand pairing) | https://github.com/cksac/fake-rs/releases | https://docs.rs/fake | none |
| bon | STACK.md, scaffold `Cargo.toml`, `rust-code-style` and `rust-testing` pin lines | https://github.com/elastio/bon/blob/master/CHANGELOG.md | https://bon-rs.com/, https://docs.rs/bon | none |
| httpmock | STACK.md, scaffold `Cargo.toml`, `rust-testing` pin line | https://github.com/alexliesenfeld/httpmock/blob/master/CHANGELOG.md | https://docs.rs/httpmock | none |
| mockall | same | https://github.com/asomers/mockall/blob/master/CHANGELOG.md | https://docs.rs/mockall | none |
| insta, cargo-insta | same; scaffold `Makefile` `install-tools` | https://github.com/mitsuhiko/insta/blob/master/CHANGELOG.md | https://insta.rs/docs/, https://docs.rs/insta | none |
| cargo-nextest | STACK.md `### Tooling`, scaffold `.config/nextest.toml` `nextest-version`, `rust-tooling` and `rust-testing` pin lines | https://github.com/nextest-rs/nextest/blob/main/cargo-nextest/CHANGELOG.md, https://nexte.st/changelog/ | https://nexte.st/ | `CLAUDE.md`, `AGENTS.md` (contributor-facing) |
| cargo-llvm-cov | STACK.md `### Tooling`, `rust-tooling` pin line | https://github.com/taiki-e/cargo-llvm-cov/blob/main/CHANGELOG.md | same README | none |

## Tooling

| Item | Pinned where | Changelog / releases | API docs | Agent docs |
|---|---|---|---|---|
| cargo-deny | STACK.md `### Tooling`, scaffold `deny.toml`, `rust-tooling` pin line | https://github.com/EmbarkStudios/cargo-deny/blob/main/CHANGELOG.md | https://embarkstudios.github.io/cargo-deny/ | none |
| cargo-machete | same, scaffold `Cargo.toml` `[package.metadata.cargo-machete]` | https://github.com/bnjbvr/cargo-machete/blob/main/CHANGELOG.md | same README | none |
| cargo-chef, `lukemathwalker/cargo-chef` image | scaffold `Dockerfile`, `rust-tooling/references/docker.md` | https://github.com/LukeMathWalker/cargo-chef/blob/main/CHANGELOG.md; image tags https://hub.docker.com/r/lukemathwalker/cargo-chef/tags | same README | none |
| bacon | STACK.md `### Tooling`, scaffold `bacon.toml`, `rust-tooling` pin line | https://github.com/Canop/bacon/blob/main/CHANGELOG.md | https://dystroy.org/bacon/ | none |
| prek | same; both `.pre-commit-config.yaml` files | https://github.com/j178/prek/blob/master/CHANGELOG.md | https://prek.j178.dev/ | `AGENTS.md` (contributor-facing) |
| cargo-binstall | scaffold `Makefile` `install-tools` | https://github.com/cargo-bins/cargo-binstall/releases | same README | none |

## Add when needed

| Item | Pinned where | Changelog / releases | API docs | Agent docs |
|---|---|---|---|---|
| rig (facade), rig-core, rig-agent | STACK.md `### AI`, add-when-needed comment, `building-rig-agents` pin line and references | https://github.com/0xPlaygrounds/rig/blob/main/CHANGELOG.md, https://github.com/0xPlaygrounds/rig/blob/main/crates/rig-core/CHANGELOG.md, https://github.com/0xPlaygrounds/rig/blob/main/crates/rig-agent/CHANGELOG.md, https://github.com/0xPlaygrounds/rig/blob/main/MIGRATING.md | https://docs.rs/rig, https://docs.rig.rs/ | https://github.com/0xPlaygrounds/rig/blob/main/AGENTS.md |
| rmcp | same | https://github.com/modelcontextprotocol/rust-sdk/releases | https://docs.rs/rmcp | none |
| jsonwebtoken | STACK.md `### Auth`, `axum-service` pin line and `references/auth.md` | https://github.com/Keats/jsonwebtoken/blob/master/CHANGELOG.md | https://docs.rs/jsonwebtoken | none |
| argon2 | same | https://github.com/RustCrypto/password-hashes/blob/master/argon2/CHANGELOG.md | https://docs.rs/argon2 | none |
| axum-extra | same | https://github.com/tokio-rs/axum/blob/main/axum-extra/CHANGELOG.md | https://docs.rs/axum-extra | none |
| nutype | STACK.md `### Web`, add-when-needed comment | https://github.com/greyblake/nutype/blob/master/CHANGELOG.md | https://docs.rs/nutype | none |
| async-nats | STACK.md `### Messaging`, `rust-nats` pin line and `SKILL.md` dependency block | https://github.com/nats-io/nats.rs/blob/main/async-nats/CHANGELOG.md | https://docs.rs/async-nats | https://github.com/nats-io/nats.rs/blob/main/CLAUDE.md |
| redis | STACK.md `### Cache`, `rust-redis` pin line and `SKILL.md` dependency block | https://github.com/redis-rs/redis-rs/blob/main/redis/CHANGELOG.md | https://docs.rs/redis | none |
| deadpool-redis | same | https://github.com/deadpool-rs/deadpool/blob/main/crates/deadpool-redis/CHANGELOG.md | https://docs.rs/deadpool-redis | none |
| sha2 | add-when-needed comment, `rust-redis/references/locks-and-limits.md` | https://github.com/RustCrypto/hashes/blob/master/sha2/CHANGELOG.md | https://docs.rs/sha2 | none |

## Servers

| Item | Pinned where | Changelog / releases | API docs | Agent docs |
|---|---|---|---|---|
| Postgres `postgres:18-alpine` | scaffold `docker-compose.yml`, scaffold `ci.yml`, `tests/common/mod.rs` `with_tag`, `rust-tooling/references/docker.md` and `ci.md`, `sea-orm-postgres` pin line and `references/testing-db.md` | https://www.postgresql.org/docs/release/ | https://www.postgresql.org/docs/current/ | none |
| Redis `redis:8-alpine` | `rust-tooling/references/docker.md` and `ci.md` optional blocks, `rust-redis` SKILL.md and `references/testing-redis.md` | https://github.com/redis/redis/blob/unstable/00-RELEASENOTES, https://github.com/redis/redis/releases | https://redis.io/docs/latest/commands/ | none |
| NATS `nats:2.12-alpine` | `rust-tooling/references/docker.md` optional block, `rust-nats` SKILL.md and `references/testing-nats.md` | https://github.com/nats-io/nats-server/releases | https://docs.nats.io/ | none |
| otel collector `otel/opentelemetry-collector-contrib` | scaffold `docker-compose.yml`, `rust-tooling/references/docker.md` | https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/CHANGELOG.md, https://github.com/open-telemetry/opentelemetry-collector-contrib/releases | https://opentelemetry.io/docs/collector/ | `CLAUDE.md`, `AGENTS.md` (contributor-facing) |

## Actions and hooks

The real list comes from reading both workflow files: `.github/workflows/check.yml` uses `actions/checkout`,
`taiki-e/install-action`, `astral-sh/setup-uv`, `actions/cache`; the scaffold's `ci.yml` uses
`actions/checkout`, `dtolnay/rust-toolchain`, `Swatinem/rust-cache`, `taiki-e/install-action`,
`actions/upload-artifact`, `EmbarkStudios/cargo-deny-action`.

| Item | Pinned where | Changelog / releases | API docs | Agent docs |
|---|---|---|---|---|
| actions/checkout | both workflows, `rust-tooling/references/ci.md` | https://github.com/actions/checkout/releases | same README | none |
| actions/cache | `.github/workflows/check.yml` | https://github.com/actions/cache/releases | same README | none |
| actions/upload-artifact | scaffold `ci.yml`, `rust-tooling/references/ci.md` | https://github.com/actions/upload-artifact/releases | same README | none |
| astral-sh/setup-uv | `.github/workflows/check.yml`; pinned to a full `vX.Y.Z` because no floating `vX` tag exists | https://github.com/astral-sh/setup-uv/releases | same README | `AGENTS.md` (contributor-facing) |
| taiki-e/install-action | both workflows, `rust-tooling/references/ci.md` | https://github.com/taiki-e/install-action/releases | same README | none |
| dtolnay/rust-toolchain | scaffold `ci.yml`, `rust-tooling/references/ci.md`; referenced by branch (`stable`), not a tag | https://github.com/dtolnay/rust-toolchain/releases | same README | none |
| Swatinem/rust-cache | same | https://github.com/Swatinem/rust-cache/releases | same README | none |
| EmbarkStudios/cargo-deny-action | same | https://github.com/EmbarkStudios/cargo-deny-action/releases | same README | none |
| pre-commit/pre-commit-hooks `rev:` | both `.pre-commit-config.yaml` files, STACK.md, `rust-tooling/references/configs.md` | https://github.com/pre-commit/pre-commit-hooks/blob/main/CHANGELOG.md | same README | none |

## Lookup commands

crates.io rejects a request without a `User-Agent` header (403), so name the caller:

```sh
curl -s -H 'User-Agent: rust-powers-refresh (github.com/DenysMoskalenko/rust-powers)' \
  https://crates.io/api/v1/crates/axum | jq -r '.crate.max_stable_version'
```

`cargo info <crate>` gives the same answer without the header and is preferred; the curl is for a loop
over many crates.

Docker Hub tags, `name=` being a substring filter:

```sh
curl -s 'https://hub.docker.com/v2/repositories/library/postgres/tags?page_size=100&name=18-alpine' \
  | jq -r '.results[].name'
# non-library images: repositories/<org>/<image>/tags
curl -s 'https://hub.docker.com/v2/repositories/lukemathwalker/cargo-chef/tags?page_size=100&name=latest-rust-1.98' \
  | jq -r '.results[].name'
```

GitHub, latest release or every tag with a prefix (add `</dev/null` inside a `while read` loop, or `gh`
consumes the loop's stdin):

```sh
gh api repos/nextest-rs/nextest/releases/latest --jq .tag_name
gh api repos/astral-sh/setup-uv/git/matching-refs/tags/v --jq '.[].ref' | sort -V | tail -3
```
