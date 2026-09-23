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
summary kept in `maintenance/`.

Costs and limits, from the September 2026 run on Opus 5.5 (`maintenance/eval-results-2026-09-23.md`):

- The full suite at two runs per arm cost about $300, and judge calls were about an eighth of
  that. `--max-cost-usd` caps the agent runs but not the judge calls, so set it that much lower.
- The default Haiku judge failed about 25 bullets that the reply plainly satisfied, mostly in
  replies over 15 KB. Before you act on a small Δ, re-grade with `--judge-model sonnet`, and
  keep each Must line a single requirement that can be checked on its own.
- Trigger cases don't need a baseline arm: pass `--ablation none --tag trigger`.
