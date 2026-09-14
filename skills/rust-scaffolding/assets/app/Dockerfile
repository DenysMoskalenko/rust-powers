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
