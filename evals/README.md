# Evaluation Scenarios

One file per skill: `evals/<skill>.md`. These are the acceptance tests for skill behaviour.
A change that alters what a good agent response looks like updates the matching file in the
same commit.

Drafts written during authoring live in `tmp/evals/<skill>.md` and are moved here during
integration. Only `evals/` is tracked.

## Format

`scripts/validate_skills.py` parses every file, so the headings and labels below are exact:
the same characters, the same case, a plain hyphen in `Eval N - name`, a colon after each
bold label, and `-> ` followed by a backticked skill name on every should-not-load line.

```markdown
# <skill-name>

### Triggering

**Should load**

1. <prompt a user would actually type>
2. ... (five or more, including the crate and API names a reader arrives with)

**Should not load**

1. <prompt> -> `<skill-that-should-win>`
2. ... (five or more, each naming the owning skill)

### Eval 1 - <short name>

**Prompt**: <what the user asks>

**Fixture**: scaffold

**Must produce**:

- <observable, checkable property of a good answer>

**Must not produce**:

- <the specific wrong pattern this eval exists to catch>

### Probe 1 - <short name>

**Prompt**: <a realistic request that tempts the one mistake this probe exists to catch>

**Fixture**: scaffold

**Wrong answer**: `<JavaScript regex matching the mistake in code, never in a warning about it>`

**Right answer**: `<optional JavaScript regex that a correct answer contains>`
```

What the validator checks: one `### Triggering` heading; a `**Should load**` block with at
least five `N. ` lines; a `**Should not load**` block with at least five `N. ` lines, each
containing `` -> `skill` ``; at least three `### Eval N - <name>` headings, each followed by
the three labelled lines `**Prompt**:`, `**Must produce**:`, `**Must not produce**:`; every
`### Probe N - <name>` block has `**Prompt**:` and a backticked `**Wrong answer**:`; a
`**Fixture**:` line, where present, is `scaffold` or `empty`.

`**Note**:` is optional and written for maintainers. The prompt ends at the next `**` label, so a
note placed after `**Prompt**:` never reaches the model under test: put there what a reader of the
eval needs to know and the model must not be told.

`**Fixture**:` is optional. It says what the working directory holds when the case runs:
`scaffold` (the default) is a copy of `skills/rust-scaffolding/assets/app/`, with the skill's
add-when-needed crate uncommented in `Cargo.toml` for building-rig-agents, rust-nats and
rust-redis; `empty` is an empty directory, the default for rust-scaffolding. The crate
versions live in that `Cargo.toml`, so prompts do not name them.

Rules:

- Should-not-load prompts must name the skill that should win instead. That is how
  ownership boundaries get tested, not just asserted.
- "Must produce" items are observable in the answer: a named API, a file path, a specific
  rule being applied. Not "is helpful".
- "Must not produce" is the point of the eval. Write the failure you actually saw or
  expect, not a generic warning.
- An eval never contradicts its skill or a sibling skill. The status a skill maps an error
  to, the file a rule lives in, and the skill that owns a topic are read from the skills,
  not remembered.
- Should-load prompts use the words a user types, not phrases copied from the description.
  Should-not-load prompts are near misses: a request a sibling owns that shares words with
  this skill.
- A probe tests one rule from `## Important` or a Red Flags table. Its wrong-answer regex
  matches the mistake in code (`route\(\s*"[^"]*/:`), so an answer that warns against the
  mistake still passes.

## Running the suite

`make evals` turns these files into cases for `claude plugin eval` and runs them. Every run is
a billed model call on your account, so the suite is not part of `make check` or CI: run it
before a release, when a new model ships, and after any description change.

`scripts/build_eval_cases.py` writes a clean copy of the plugin to the gitignored
`evals/.run/`, with one case per prompt under `evals/.run/cases/<skill>/`:

- every should-load prompt checks that the skill fired;
- every should-not-load prompt checks that it did not fire and that the named sibling did;
- every eval gets one judged grader per `Must produce` and `Must not produce` line, run with and
  without the plugin, so the score difference shows what the skill adds;
- every probe gets a regex grader, run with and without the plugin.

Pass options through `EVAL_ARGS`, for example
`make evals EVAL_ARGS="--model claude-opus-5-5 --tag rust-nats"`. Each case is tagged with its
skill and one of `trigger`, `eval` or `probe`. Results land in `evals/.run/cases/results/`;
`python3 scripts/summarize_evals.py <result.json>...` turns `--json` output into the per-case
summary kept in `maintenance/`. A skipped judge, run error or missing score is excluded from the
mean and identified by arm. An arm without a valid score has no comparison delta.

A custom `--out` must be a new path or a root marked by this generator. Rebuilding keeps both
suites’ `results/` directories in place, including nested skill and case runs. On the first upgrade of the old default `evals/.run/`,
the generator retains its former files under a unique `.previous-*/` directory in that root.

Costs and limits, from the September 2026 run on Opus 5.5 (`maintenance/eval-results-2026-09-23.md`):

- The full suite at two runs per arm cost about $300, and judge calls were about an eighth of
  that. `--max-cost-usd` caps the agent runs but not the judge calls, so set it that much lower.
- The default Haiku judge failed about 25 bullets that the reply plainly satisfied, mostly in
  replies over 15 KB. Before you act on a small Δ, re-grade with `--judge-model sonnet`, and
  keep each Must line a single requirement that can be checked on its own.
- Trigger cases don't need a baseline arm: pass `--ablation none --tag trigger`.

## Grading files from an editable run

Two opt-in cases under `artifacts/` complement the read-only suite. The greenfield case builds a
hyphenated `library-svc`. The existing-service case adds a batch endpoint to a small ledger API
that already defines its own public error, response and tuned limit. The regular suite still
asks for code in the reply.

Generate the cases with `python3 scripts/build_eval_cases.py`. After the usual case, arm and
cost-cap approval, run the selected case once per arm and keep its workspace:

```bash
claude plugin eval evals/.run --eval-dir artifacts --case axum-service.artifact \
  --runs 1 --ablation with-without --scaffold --keep-temp --allow-tools Write Edit \
  --max-cost-usd "$APPROVED_AGENT_CAP" --no-publish
```

For the greenfield case substitute `rust-scaffolding.artifact` and grant `Bash` as well so it
can copy the template. The native file grader is a preliminary signal. The independent result
comes from compiling and exercising each retained workspace. Use the path containing its
`Cargo.toml` printed by `--keep-temp`; record the case, arm, attempt and exit status together.

```bash
python3 scripts/grade_eval_workspace.py existing "$EXISTING_WORKSPACE"
python3 scripts/grade_eval_workspace.py greenfield "$GREENFIELD_WORKSPACE"
```

Run the grader inside a disposable sandbox or CI worker suitable for building the agent’s
code. It copies the workspace before testing it. The existing-service verifier supplies its
own Rust integration test for the old route, public error, limit and new route. Greenfield
checks the crate, Docker, compose and environment renames, then runs
`cargo test --locked --workspace` and requires the original liveness test and independent
OpenAPI-title test to pass;
provide Docker or a disposable `TEST_DATABASE_URL`. For an offline control, run the existing
case’s `setup.sh` in an empty directory, or copy and rename the scaffold using the greenfield
skill. Run the same grader against those files with `CARGO_NET_OFFLINE=true` and cached crates.
An untouched ledger fixture must fail on the missing POST route.
