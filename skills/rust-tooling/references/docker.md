# Docker and docker-compose

A Rust release build recompiles every dependency on any source change unless the dependency build is its own
layer. cargo-chef produces that layer from the manifests alone, so the expensive part is cached until
`Cargo.toml` or `Cargo.lock` changes.

## Contents

- [Dockerfile](#dockerfile)
- [.dockerignore](#dockerignore)
- [The release profile](#the-release-profile)
- [Runtime base image](#runtime-base-image)
- [docker-compose.yml](#docker-composeyml)
  - [Optional services](#optional-services)
  - [Optional: the service itself](#optional-the-service-itself)

## Dockerfile

```dockerfile
# cargo-chef caches the dependency build: the expensive layer is only rebuilt when
# Cargo.toml or Cargo.lock changes, not when a source file does.
#
# The tag must match `rust-toolchain.toml` EXACTLY. `1.98` and `1.98.1` are two
# different rustup toolchain names, so a mismatch makes every stage download a
# second complete toolchain before it compiles anything.
FROM lukemathwalker/cargo-chef:latest-rust-1.98.1 AS chef
WORKDIR /build

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /build/recipe.json recipe.json
# Only the dependency graph so far, so this layer survives source edits.
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked --bin app

# Distroless would be smaller, but this image still has a shell for debugging.
FROM debian:trixie-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 app
COPY --from=builder /build/target/release/app /usr/local/bin/app
USER app
EXPOSE 8000
ENV APP__SERVER__HOST=0.0.0.0 APP__SERVER__PORT=8000
CMD ["app"]
```

Rules that keep the cache working:

- The chef tag and the `channel` in `rust-toolchain.toml` name the same toolchain, patch version included.
  The image tag carries a full version (`latest-rust-1.98.1`) and rustup matches toolchain names literally,
  so a `"1.98"` channel is a different name: `COPY . .` brings the file in, rustup sees a toolchain it does
  not have, and both the planner and the builder stage download a second one before compiling anything. A
  mismatch never errors, it just costs minutes per build. With the names equal, rustup only fetches the three
  components the file lists (`rustfmt`, `clippy`, `llvm-tools-preview`) — a few seconds, not a toolchain.
  Bump the channel and the tag in one commit.
- `cargo chef prepare` walks the workspace manifests, so a directory the manifest names must exist in the
  build context. Keep `tests/` out of `.dockerignore`.
- Install the tool with `cargo install cargo-chef --locked --version 0.1.78` when building on a plain
  `rust:` image yourself; the chef image ships the same version.

`ENV APP__SERVER__HOST=0.0.0.0` restates the settings default (`ServerSettings::default()` is already
`0.0.0.0`). It is belt and braces: a deployment that sets `APP__SERVER__HOST=127.0.0.1` for a local run and
then ships the image publishes a dead port, and the `ENV` line is where the next person looks first.

`--bin app` must match the `[package] name` in `Cargo.toml`, and so must the two paths below it. Rename all
three together — a build for a binary that does not exist fails at `cargo build`, not at `COPY`, which is the
easier half of the mistake to spot.

`COPY` before `USER`, so the binary is owned by root and not writable by the process that runs it. Setting
`USER` first and then creating a directory leaves it root-owned instead, which is the opposite of the
intention.

## .dockerignore

```
target/
.git/
.gitignore
.github/
.env*
Dockerfile*
.dockerignore
compose*.yml
docker-compose.yml
*.md
```

`target/` alone is worth it: shipping a local debug build into the context can add gigabytes to every build.

## The release profile

Not in the template. Add to `Cargo.toml` when image size or throughput is measured to matter:

```toml
[profile.release]
lto = "thin"        # "fat" costs much more build time for a marginal gain
codegen-units = 1
strip = "symbols"
```

Do not add `panic = "abort"`. With the default unwinding, the scaffold's `CatchPanicLayer` (tower-http
feature `catch-panic`) turns a panicking handler into a logged, constant 500 and the process keeps
serving. With `abort` there is nothing to catch: that one bad request takes down the whole process. It
also does not apply to `cargo test`, which builds with unwinding anyway.

Skip `-C target-cpu=native` in `RUSTFLAGS`: the builder CPU is not the runtime CPU. `x86-64-v3` is safe only
on known-fixed hardware.

## Runtime base image

Use glibc, not musl. musl's allocator is dramatically slower under the multithreaded allocation pattern a
tokio server produces, which is exactly the workload here.

`debian:trixie-slim` (Debian 13, the current stable) plus `ca-certificates` and a non-root user is the
default: it is small enough and you can still `exec` into it to debug. Trixie rather than `bookworm-slim`
simply because bookworm is now oldstable. `gcr.io/distroless/cc-debian12:nonroot` is smaller and ships glibc,
CA certificates and a non-root user, but has an open report of a four-times slowdown and growing RSS for Rust
services — measure before moving.

CA certificates are required at runtime even though the HTTP client uses rustls, because rustls resolves
roots through the platform verifier. `distroless/static` is not enough for a normal dynamically linked build.

## docker-compose.yml

The template's file: Postgres for development plus an OTLP collector behind a profile. Exporting
`TEST_DATABASE_URL` against this Postgres is what keeps the test suite off testcontainers.

```yaml
# The project name keeps this stack's containers and volume apart from every
# other compose stack on the machine.
name: app

services:
  postgres:
    image: postgres:18-alpine
    environment:
      POSTGRES_USER: postgres
      POSTGRES_PASSWORD: postgres
      POSTGRES_DB: app
    ports:
      # Override in .env when 5432 is already taken on this machine.
      - "${POSTGRES_PORT:-5432}:5432"
    volumes:
      # Postgres 18 keeps its data in a major-version subdirectory, so the mount
      # goes one level up. Mounting .../data instead makes the container exit.
      - pgdata:/var/lib/postgresql
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U postgres"]
      interval: 5s
      timeout: 3s
      retries: 10

  # Off by default. `docker compose --profile otel up -d` starts it; the app only
  # exports once APP__TELEMETRY__OTLP_ENDPOINT points at it.
  otel-collector:
    image: otel/opentelemetry-collector-contrib:0.140.0
    profiles: ["otel"]
    command: ["--config=/etc/otel-config.yaml"]
    configs:
      - source: otel
        target: /etc/otel-config.yaml
    ports:
      - "4317:4317"

configs:
  otel:
    content: |
      receivers:
        otlp:
          protocols:
            grpc:
              endpoint: 0.0.0.0:4317
      exporters:
        debug:
          verbosity: normal
      service:
        pipelines:
          traces:
            receivers: [otlp]
            exporters: [debug]

volumes:
  pgdata:
```

`name:` pins the compose project, so two services on one machine do not share a `pgdata` volume. The
service is `postgres` and the database `app` (`rust-scaffolding` renames both when the template is copied); `docker compose up -d
postgres` is the day-to-day command, then `cargo run` on the host keeps the compile loop fast.

`docker compose config` renders the merged file and exits non-zero on a mistake — run it after any edit.

### Optional services

Add under `services:` when the service takes on a cache or a broker. `rust-redis` and `rust-nats` own the
client side and the tests; the tests read `TEST_REDIS_URL` / `TEST_NATS_URL` the way the database tests read
`TEST_DATABASE_URL`, so point those at these containers.

```yaml
  redis:
    image: redis:8-alpine
    ports:
      - "6379:6379"
    healthcheck:
      test: ["CMD", "redis-cli", "ping"]
      interval: 5s
      timeout: 3s
      retries: 10

  nats:
    image: nats:2.12-alpine
    # `-js` turns JetStream on; the image default is core NATS only.
    command: ["-js", "-m", "8222"]
    ports:
      - "4222:4222"
    healthcheck:
      test: ["CMD", "wget", "-qO-", "http://localhost:8222/healthz"]
      interval: 5s
      timeout: 3s
      retries: 10
```

### Optional: the service itself

To check that the image works, add an `app` service that depends on the healthy database. Keep it down in
daily development; `cargo run` on the host is the fast loop.

```yaml
  app:
    build: .
    depends_on:
      postgres:
        condition: service_healthy
    environment:
      APP__DATABASE__URL: postgres://postgres:postgres@postgres:5432/app
      APP__AUTH__JWT_SECRET: dev-only-change-me
      RUST_LOG: info
    ports:
      - "8000:8000"
```

`condition: service_healthy` needs the `healthcheck` block on `postgres`; without it `depends_on` waits for
the container to start, not for Postgres to accept connections, and the service exits on a connection
refused. Settings arrive as `APP__SECTION__FIELD`: the double underscore is the nesting separator, so
`APP__DATABASE__URL` fills `Settings.database.url`.
