# Python to Rust

A lookup table for someone who knows the Python tool and needs the Rust one. It names the replacement and the skill that owns it — open that skill for the code, because this file deliberately contains none.

## Language and standard library

| Python | Rust | Owner |
|---|---|---|
| `@dataclass`, attrs | a struct with derives | `rust-code-style` |
| `Enum`, `StrEnum` | an enum, with `strum` for the string form | `rust-code-style` |
| `typing.Protocol` | a trait | `rust-code-style` |
| `None` | `Option<T>` | `rust-code-style` |
| `dict[str, Any]` for a known shape | a struct; `serde_json::Value` only at the edge | `rust-code-style` |
| custom exception classes | `thiserror` enums | `rust-code-style` |
| `raise ... from exc` | `#[from]` or `#[source]` | `rust-code-style` |
| `except Exception: pass` | an explicit `if let Err(error)` that logs | `rust-code-style` |
| `functools.cached_property`, module-level singleton | `OnceLock`, `LazyLock` | `rust-code-style` |
| `asyncio.gather` | `tokio::try_join!` or `JoinSet` | `rust-code-style` |
| `asyncio.to_thread` | `tokio::task::spawn_blocking` | `rust-code-style` |
| threads under the GIL | `tokio::spawn` with `Send + 'static` | `rust-code-style` |
| `decimal.Decimal` | `rust_decimal` | `rust-code-style` |
| `datetime.now(UTC)` | `chrono::Utc::now`, behind an injected `Clock` | `rust-code-style` |

## Web and configuration

| Python | Rust | Owner |
|---|---|---|
| FastAPI app and routers | `axum::Router`, `OpenApiRouter` | `axum-service` |
| `Depends()` | `AppState` and extractors | `axum-service` |
| pydantic request and response models | serde plus `validator` plus `utoipa` | `axum-service` |
| pydantic 422 error body | the `Valid` extractor feeding `AppError` | `axum-service` |
| FastAPI exception handlers | `impl IntoResponse for AppError` | `axum-service` |
| automatic `/docs` | `utoipa-swagger-ui` | `axum-service` |
| pydantic-settings | `config` plus dotenvy plus secrecy | `axum-service` |
| starlette middleware | tower layers | `axum-service` |
| `BackgroundTasks` | `tokio::spawn` after the response | `axum-service` |
| httpx | reqwest | `axum-service` |

## Database

| Python | Rust | Owner |
|---|---|---|
| SQLAlchemy 2.0 async | sea-orm 2 | `sea-orm-postgres` |
| declarative models | generated dense entities | `sea-orm-postgres` |
| alembic | sea-orm-migration | `sea-orm-postgres` |
| `selectinload` | `load_many`, `Entity::load().with` | `sea-orm-postgres` |
| `joinedload` | `find_with_related` on a collection, `find_also_related` on a to-one | `sea-orm-postgres` |
| `session.scalar`, `session.execute` | `Entity::find().one`, `.all` | `sea-orm-postgres` |
| `session.begin()` | `db.begin()` with `C: ConnectionTrait` | `sea-orm-postgres` |
| `IntegrityError` on a unique index | `DbErr::sql_err()` giving `UniqueConstraintViolation` | `sea-orm-postgres` |

## Testing

| Python | Rust | Owner |
|---|---|---|
| pytest | cargo-nextest | `rust-testing` |
| fixtures, `parametrize` | rstest | `rust-testing` |
| `httpx.AsyncClient` against the app | axum-test `TestServer` | `rust-testing` |
| dependency overrides | a `test_app()` helper that swaps state | `rust-testing` |
| polyfactory | fake plus bon | `rust-testing` |
| freezegun | an injected `Clock` plus `tokio::time::pause` | `rust-testing` |
| respx, responses | httpmock | `rust-testing` |
| syrupy | insta | `rust-testing` |
| `unittest.mock` | mockall, or a hand-written fake | `rust-testing` |
| testcontainers | testcontainers-modules | `sea-orm-postgres` |
| pytest-cov | cargo-llvm-cov | `rust-tooling` |

## Tooling and observability

| Python | Rust | Owner |
|---|---|---|
| uv | cargo plus rustup | `rust-tooling` |
| ruff format and lint | rustfmt plus clippy | `rust-tooling` |
| mypy, ty | the compiler | `rust-tooling` |
| complexipy | clippy complexity lints | `rust-tooling` |
| pip-audit | cargo-deny | `rust-tooling` |
| pre-commit | prek | `rust-tooling` |
| logging, structlog | tracing | `axum-service` |
| opentelemetry-sdk | opentelemetry plus tracing-opentelemetry | `axum-service` |
| prometheus-client | metrics plus axum-prometheus | `axum-service` |
| pydantic-ai | rig | `building-rig-agents` |
| nats-py | async-nats | `rust-nats` |
| redis-py, aioredis | redis-rs with `ConnectionManager` | `rust-redis` |

## Habits that do not port

- **No monkeypatching.** Nothing is swapped at runtime, so a dependency a test must control is injected at construction. That is why `Clock` exists and why a repository trait does not.
- **No `Any`.** There is no escape hatch that silences the type checker for everything downstream; if the type is hard to name, the design is telling you something.
- **Errors are values.** Nothing unwinds past a caller by accident, so every fallible call site is visible as `?` and every ignored one is visible as `let _`.
- **Ownership is part of the signature.** The Python habit of passing a mutable container around and letting several functions append to it has no direct translation; decide who owns the data and have the others borrow.
- **Validation happens once, at the edge.** `serde` plus `validator` parse into a type the service can trust, in place of repeated `isinstance` and `if not x` checks.
