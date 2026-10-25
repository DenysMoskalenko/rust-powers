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
cap_target = dir=$$(cargo metadata --no-deps --format-version 1 | grep -oE '"(build|target)_directory":"[^"]*"' \
	| sort | head -1 | cut -d'"' -f4); \
	kb=$$(du -sk "$$dir" 2>/dev/null | cut -f1); \
	if [ "$${kb:-0}" -gt $$(($(TARGET_CAP_GB) * 1048576)) ]; then \
	echo "$$dir is past $(TARGET_CAP_GB) GB: cargo clean"; cargo clean; fi

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
