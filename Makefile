.PHONY: check validate snippets lint evals

# Outside the repository, which local plugin installs copy whole; the OS reclaims the temp dir.
export CARGO_TARGET_DIR ?= $(or $(TMPDIR),/tmp)/rust-powers-target

CLAUDE ?= claude
EVAL_ARGS ?=

# Everything CI runs.
check: validate snippets

# Frontmatter, budgets, ownership hygiene, STACK.md <-> scaffold Cargo.toml parity.
validate:
	python3 scripts/validate_skills.py

# Extract every ```rust,verify block into verify/ and cargo check + clippy + nextest it.
# Needs Docker, or TEST_DATABASE_URL pointing at a Postgres the tests may create databases in.
snippets:
	python3 scripts/check_snippets.py

lint:
	prek run --all-files

# Billed model calls on your account: run before a release or when a new model ships, not in CI.
evals:
	python3 scripts/build_eval_cases.py
	$(CLAUDE) plugin eval evals/.run --eval-dir cases --scaffold --no-publish $(EVAL_ARGS)
