.PHONY: install-tools fmt lint check test cov dev run migrate entity

install-tools:
	cargo install cargo-binstall
	cargo binstall -y cargo-nextest cargo-llvm-cov cargo-deny cargo-machete cargo-chef bacon cargo-insta sea-orm-cli
	uv tool install prek
	prek install

fmt:
	cargo fmt --all

lint:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets --all-features -- -D warnings

check: lint
	cargo deny check
	cargo machete

test:
	cargo nextest run --workspace --all-features
	cargo test --doc --workspace

cov:
	cargo llvm-cov nextest --workspace --all-features --no-report
	cargo llvm-cov report --lcov --output-path target/lcov.info --ignore-filename-regex '(entities|migration)/|main\.rs|telemetry\.rs'
	cargo llvm-cov report --summary-only --fail-under-lines 80 --ignore-filename-regex '(entities|migration)/|main\.rs|telemetry\.rs'

dev:
	bacon

run:
	cargo run

migrate:
	sea-orm-cli migrate up

entity:
	sea-orm-cli generate entity -o src/entities --with-serde both --entity-format dense
