.PHONY: check validate snippets lint

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
