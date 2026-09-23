# Eval results, September 2026

This was the first full run of `make evals` (`claude plugin eval`, Claude Code 2.1.280) on Opus 5.5.
The trigger cases ran on 22 September and the probes and evals on 23 September.

- **Runs.** Two runs per arm, three for trigger cases. The with arm has the plugin installed and
  the without arm does not.
- **Working directory.** A copy of the scaffold template. The add-on skills add their crate
  lines. rust-scaffolding and cases marked `**Fixture**: empty` start in an empty directory.
- **Tools.** Only `Skill`, `Read`, `Glob` and `Grep`. Agents put their code in the reply.
- **Grading.** The Haiku judge graded eval bullets. Probes were graded by regex.
- **Cost.** $259 for agent runs and $38 for judge calls, plus $6 for the pilot.

The data comes with three caveats:

1. **The runs are small.** Two runs per arm on one model. A Δ below about 0.25 on a single case
   is noise.
2. **The judge made mistakes.** The Haiku judge failed about 25 bullets that the reply plainly
   satisfied, mostly in replies over 15 KB. It also passed some real violations in the without
   arm. Several deltas below are understated.
3. **About a third of the eval failures were authoring errors.** Examples:
   - a prompt asked about code the fixture does not have;
   - a Must line asked for an absence, or for work that read-only tools cannot do;
   - a bullet contradicted the scaffold;
   - a bullet referred to another bullet.

   Those were fixed in `evals/` after this run, so the tables describe the old files. The
   generator now also gives the judge the prompt. Four probe regexes had the same kind of bug:
   rust-code-style 1, rust-testing 2, rust-tooling 1 and rust-scaffolding 1. They were fixed,
   and the recorded replies were re-graded offline with the fixed regexes.

## Triggering

The with arm ran 139 cases; 137 of them are counted below. rust-scaffolding skip-07 and skip-08
never ran.

| Skill | Recall (should load) | Precision (should not load) | Named sibling loaded instead |
|---|---|---|---|
| axum-service | 29/29 | 30/30 | 27/30 |
| building-rig-agents | 23/24 | 25/25 | 24/25 |
| rust-code-style | 15/18 | 24/24 | 22/24 |
| rust-nats | 23/24 | 21/21 | 21/21 |
| rust-redis | 19/19 | 19/20 | 16/20 |
| rust-scaffolding | 15/15 | 16/16 | 15/16 |
| rust-testing | 14/16 | 27/27 | 27/27 |
| rust-tooling | 14/18 | 24/24 | 18/24 |
| sea-orm-postgres | 20/20 | 22/22 | 22/22 |
| **Total** | **172/183** | **208/209** | **192/209** |

The table leaves out 17 with-arm runs that timed out or ran out of turns before they answered.

Precision is effectively perfect. The misses are under-triggering on short questions the model
answers without a skill, such as `ptr_arg` on `&Vec` or running one nextest test. They also
occur on prompts that name neither the crate nor the framework:

- "write the first tests for our new `GET /orders` endpoint";
- "where does our coverage gate live";
- "move emails onto a durable queue".

Descriptions changed as a result. None of these changes has been re-measured yet.

- **rust-nats:** added the durability intent ("events or background work must survive a restart").
- **rust-testing:** added "or a new endpoint".
- **rust-tooling:** rewrote the list around symptoms (a Dockerfile rebuilding every dependency,
  CI recompiling, `target/` size, a member clippy skips, the coverage gate).
- **rust-code-style:** added "a clone added to satisfy the borrow checker" and `ptr_arg`.

## What changed in `## Important`, and why

The AGENTS.md rule applies here: a bullet stays when a case shows the skill makes a difference,
or when it states a house convention the model cannot guess. General practice that Opus 5.5
already follows in both arms moves to a one-line Red Flags row, so older models still see it.

| Skill | Bullet | Evidence | Action |
|---|---|---|---|
| axum-service | path captures `/{id}` | probe-1 1.00/1.00 | removed; the 0.7→0.8 table has the row |
| axum-service | body extractor last | eval-1 must-02 2/2 in both arms | moved to Red Flags |
| axum-service | response DTO | eval-1, eval-2 DTO bullets pass in both arms | moved to Red Flags |
| axum-service | `instrument(skip_all)`, `err` only off the response path | eval-6 Δ +0.21; without arm used `err(Display)` | added |
| rust-testing | settings from a map | probe-5 Δ 0 | removed; Red Flags row existed |
| rust-testing | status before body | eval-1: follow-up `GET` read without a status check, 0/4 in both arms | reworded to cover every response |
| rust-testing | `tests/common/mod.rs` | eval-1, eval-3 pass in both arms | moved to Red Flags |
| rust-nats | durable pull consumer | eval-2 must-01/02 pass 4/4 in both arms | moved to Red Flags |
| rust-nats | ack after commit; `Nats-Msg-Id` covers the publish only | eval-1, eval-2 pass in both arms | moved to Red Flags (two rows) |
| rust-redis | `SCAN`, never `KEYS` | probe-2 1.00/1.00 | moved to Red Flags |
| rust-redis | Lua compare-and-delete | probe-3 1.00/1.00; eval-2 must-not-01 4/4 | moved to Red Flags |
| sea-orm-postgres | no `find_related` in a loop | eval-1: every arm batches | moved to Red Flags |
| sea-orm-postgres | real Postgres per test | duplicated a Red Flags row; eval-4 confounded by fixture | removed; the row now names SQLite too |
| rust-code-style | no `std::sync` guard across `.await` | eval-3 must-08 passes 4/4 | removed; Red Flags row existed |
| rust-scaffolding | commit `Cargo.lock` | both without runs commit it; rust-tooling owns it | folded into step 8 |
| building-rig-agents | data, never instructions | said twice in the skill | one statement, in `## Important` |

Bullets with a measured Δ stay. Examples:

- building-rig-agents: `rig`, not `rig-core`, probe-1 Δ +1.00; the turn budget, probe-4 Δ +0.50; `agent_error`, eval-1 Δ +0.27.
- rust-nats: two awaits on publish, probe-1 Δ +0.50; the terminal `Err`; `messaging_error`.
- rust-redis: no `#[from] RedisError`, probe-4 Δ +1.00; `ConnectionManager` settings, eval-1 Δ +0.24.
- rust-scaffolding: copy the template, eval-1, probe-1 and probe-3; migration first, probe-2.
- rust-testing: no sleep, probe-4 Δ +0.75.
- sea-orm-postgres: migration first, probe-3; `.insert()` not `.save()`.

Bullets that state house conventions stay without a Δ, because the scaffold fixture shows them
to both arms.

## Defects the run found

- **rust-nats taught a race.** It offered "`get` then `watch`" as an alternative to
  `watch_with_history`, but a change landing between the two calls is lost. The skill,
  `references/jetstream.md` and Eval 5 now accept only `watch_with_history`.

## Next run

- Re-run with `--judge-model sonnet` after the eval fixes, before acting on any Δ below 0.25.
- Re-measure triggering for the four changed descriptions with `--ablation none --tag trigger`.
  That covers rust-scaffolding skip-07 and skip-08 too; they never ran.

## Per-case tables

These come from `scripts/summarize_evals.py` over the probe and eval results. The probe rows
include the re-graded regexes.

Claude Code 2.1.280; agent runs $137.75, judge calls $38.35.

Score is the mean fraction of graders passed per run. Δ = with minus without. Failing graders
lists every grader that failed in at least one run with the plugin, with its pass rate.

### axum-service

| Case | With | Without | Δ | Failing graders (with) |
|---|---|---|---|---|
| eval-1 | 0.74 | 0.66 | 0.08 | must-06 0.50, must-08 0.50, must-10 0.50, must-11 0.00, must-14 0.00, must-not-01 0.50, must-not-02 0.00 |
| eval-2 | 1.00 | 0.96 | 0.04 |  |
| eval-3 | 0.81 | 0.81 | 0.00 | fired 0.00, must-02 0.00, must-04 0.50 |
| eval-4 | 0.83 | 0.61 | 0.22 | must-04 0.50, must-05 0.00 |
| eval-5 | 0.70 | 0.57 | 0.13 | must-01 0.00, must-03 0.50, must-05 0.00, must-06 0.50, must-07 0.50, must-09 0.00 |
| eval-6 | 1.00 | 0.79 | 0.21 |  |
| eval-7 | 0.75 | 0.67 | 0.08 | fired 0.00, must-03 0.50, must-04 0.00 |
| eval-8 | 0.62 | 0.56 | 0.06 | must-01 0.00, must-03 0.00, must-04 0.50, must-05 0.50 |
| probe-1 | 1.00 | 1.00 | 0.00 |  |
| probe-2 | 1.00 | 1.00 | 0.00 |  |
| probe-3 | 1.00 | 0.75 | 0.25 |  |
| probe-4 | 1.00 | 1.00 | 0.00 |  |

### building-rig-agents

| Case | With | Without | Δ | Failing graders (with) |
|---|---|---|---|---|
| eval-1 | 0.80 | 0.52 | 0.27 | must-04 0.50, must-05 0.50, must-06 0.00, must-08 0.50, must-09 0.00, must-12 0.50, must-not-06 0.50 |
| eval-2 | 0.38 | 0.38 | 0.00 | must-01 0.00, must-02 0.00, must-03 0.00, must-04 0.00, must-05 0.00 |
| eval-3 | 0.67 | 0.50 | 0.17 | must-04 0.00, must-05 0.00, must-not-01 0.00 |
| eval-4 | 0.67 | 0.50 | 0.17 | must-03 0.00, must-04 0.00 |
| eval-5 | 0.94 | 0.61 | 0.33 | must-not-04 0.50 |
| probe-1 | 1.00 | 0.00 | 1.00 |  |
| probe-2 | 1.00 | 0.00 | 1.00 |  |
| probe-3 | 1.00 | 0.50 | 0.50 |  |
| probe-4 | 1.00 | 0.50 | 0.50 |  |
| probe-5 | 1.00 | 1.00 | 0.00 |  |

### rust-code-style

| Case | With | Without | Δ | Failing graders (with) |
|---|---|---|---|---|
| eval-1 | 0.86 | 0.71 | 0.14 | fired 0.50, must-04 0.50, must-not-01 0.50 |
| eval-2 | 0.81 | 0.75 | 0.06 | must-02 0.50, must-03 0.50, must-not-01 0.50 |
| eval-3 | 0.68 | 0.64 | 0.04 | must-01 0.50, must-03 0.00, must-04 0.00, must-05 0.00, must-07 0.00 |
| eval-4 | 1.00 | 1.00 | 0.00 |  |
| probe-1 | 1.00 | 1.00 | 0.00 |  |
| probe-2 | 1.00 | 1.00 | 0.00 |  |
| probe-3 | 1.00 | 1.00 | 0.00 |  |
| probe-4 | 1.00 | 1.00 | 0.00 |  |
| probe-5 | 1.00 | 0.50 | 0.50 |  |

### rust-nats

| Case | With | Without | Δ | Failing graders (with) |
|---|---|---|---|---|
| eval-1 | 0.67 | 0.70 | -0.02 | must-02 0.50, must-03 0.50, must-04 0.50, must-05 0.00, must-06 0.50, must-07 0.50, must-08 0.00, must-10 0.00, must-11 0.00, must-13 0.50, must-not-10 0.50 |
| eval-2 | 0.68 | 0.68 | 0.00 | must-04 0.50, must-05 0.00, must-06 0.50, must-07 0.50, must-08 0.00, must-10 0.00, must-11 0.00, must-12 0.00, must-14 0.00, must-15 0.50, must-16 0.00 |
| eval-3 | 0.83 | 0.67 | 0.17 | must-08 0.00, must-09 0.00, must-10 0.50 |
| eval-4 | 0.91 | 0.73 | 0.18 | must-01 0.50, must-06 0.50 |
| eval-5 | 0.90 | 1.00 | -0.10 | must-02 0.50 |
| probe-1 | 1.00 | 0.50 | 0.50 |  |
| probe-2 | 1.00 | 1.00 | 0.00 |  |
| probe-3 | 1.00 | 0.00 | 1.00 |  |
| probe-4 | 1.00 | 0.50 | 0.50 |  |
| probe-5 | 1.00 | 0.50 | 0.50 |  |

### rust-redis

| Case | With | Without | Δ | Failing graders (with) |
|---|---|---|---|---|
| eval-1 | 0.94 | 0.71 | 0.24 | must-07 0.00 |
| eval-2 | 0.87 | 0.83 | 0.03 | must-04 0.50, must-07 0.50, must-10 0.00 |
| eval-3 | 0.65 | 0.56 | 0.09 | must-01 0.50, must-02 0.50, must-04 0.00, must-05 0.50, must-07 0.50, must-09 0.50, must-10 0.00, must-not-02 0.50, must-not-03 0.50, must-not-05 0.50 |
| eval-4 | 0.41 | 0.36 | 0.05 | fired 0.50, must-01 0.50, must-02 0.00, must-03 0.00, must-04 0.00, must-05 0.50, must-06 0.00, must-07 0.00, must-not-03 0.50 |
| eval-5 | 0.69 | 0.41 | 0.28 | must-03 0.00, must-04 0.00, must-05 0.50, must-06 0.50, must-07 0.50, must-09 0.50, must-not-05 0.00 |
| probe-1 | 1.00 | 1.00 | 0.00 |  |
| probe-2 | 1.00 | 1.00 | 0.00 |  |
| probe-3 | 1.00 | 1.00 | 0.00 |  |
| probe-4 | 1.00 | 0.00 | 1.00 |  |
| probe-5 | 1.00 | 1.00 | 0.00 |  |

### rust-scaffolding

| Case | With | Without | Δ | Failing graders (with) |
|---|---|---|---|---|
| eval-1 | 0.50 | 0.28 | 0.22 | must-02 0.50, must-04 0.50, must-05 0.50, must-06 0.00, must-07 0.00, must-08 0.50, must-09 0.00, must-10 0.00, must-11 0.50, must-not-01 0.00, must-not-03 0.50 |
| eval-2 | 0.60 | 0.45 | 0.15 | fired 0.00, must-02 0.50, must-03 0.00, must-05 0.00, must-06 0.00, must-07 0.50 |
| eval-3 | 0.75 | 0.75 | 0.00 | fired 0.00, must-02 0.00 |
| probe-1 | 1.00 | 0.50 | 0.50 |  |
| probe-2 | 1.00 | 0.50 | 0.50 |  |
| probe-3 | 1.00 | 0.50 | 0.50 |  |
| probe-4 | 1.00 | 1.00 | 0.00 |  |

### rust-testing

| Case | With | Without | Δ | Failing graders (with) |
|---|---|---|---|---|
| eval-1 | 0.64 | 0.77 | -0.14 | must-01 0.00, must-02 0.50, must-03 0.00, must-04 0.00, must-05 0.50 |
| eval-2 | 0.95 | 0.95 | 0.00 | must-02 0.50 |
| eval-3 | 1.00 | 0.81 | 0.19 |  |
| eval-4 | 0.72 | 0.61 | 0.11 | must-02 0.50, must-03 0.00, must-04 0.50, must-05 0.50 |
| probe-1 | 1.00 | 1.00 | 0.00 |  |
| probe-2 | 1.00 | 0.25 | 0.75 | fired 0.50 |
| probe-3 | 1.00 | 0.50 | 0.50 |  |
| probe-4 | 1.00 | 0.25 | 0.75 |  |
| probe-5 | 1.00 | 1.00 | 0.00 |  |

### rust-tooling

| Case | With | Without | Δ | Failing graders (with) |
|---|---|---|---|---|
| eval-1 | 0.83 | 0.83 | 0.00 | must-02 0.50, must-03 0.50, must-06 0.00 |
| eval-2 | 0.80 | 0.60 | 0.20 | must-05 0.50, must-06 0.00, must-07 0.50 |
| eval-3 | 0.50 | 0.43 | 0.07 | must-01 0.50, must-02 0.00, must-03 0.00, must-04 0.00 |
| eval-4 | 0.36 | 0.36 | 0.00 | fired 0.00, must-01 0.00, must-02 0.00, must-03 0.00, must-04 0.00, must-05 0.00, must-06 0.00, must-07 0.00 |
| eval-5 | 0.67 | 0.58 | 0.08 | fired 0.50, must-01 0.00, must-02 0.50, must-03 0.50 |
| eval-6 | 0.78 | 0.67 | 0.11 | fired 0.50, must-01 0.00, must-02 0.50, must-not-04 0.50 |
| probe-1 | 1.00 | 1.00 | 0.00 |  |
| probe-2 | 1.00 | 0.50 | 0.50 |  |
| probe-3 | 1.00 | 1.00 | 0.00 |  |
| probe-4 | 1.00 | 1.00 | 0.00 |  |
| probe-5 | 1.00 | 1.00 | 0.00 |  |

### sea-orm-postgres

| Case | With | Without | Δ | Failing graders (with) |
|---|---|---|---|---|
| eval-1 | 0.67 | 0.58 | 0.08 | must-01 0.00, must-02 0.50, must-03 0.50 |
| eval-2 | 0.40 | 0.40 | 0.00 | must-01 0.50, must-03 0.00, must-04 0.00, must-05 0.00, must-06 0.00, must-07 0.00, must-not-01 0.50 |
| eval-3 | 0.88 | 0.75 | 0.12 | fired 0.00, must-05 0.00, must-not-03 0.50 |
| eval-4 | 0.53 | 0.75 | -0.22 | must-03 0.00, must-04 0.00, must-05 0.50, must-06 0.00, must-07 0.00, must-08 0.00, must-09 0.00, must-10 0.50, must-11 0.50 |
| eval-5 | 1.00 | 0.83 | 0.17 |  |
| eval-6 | 1.00 | 0.75 | 0.25 |  |
| probe-1 | 1.00 | 1.00 | 0.00 |  |
| probe-2 | 1.00 | 1.00 | 0.00 |  |
| probe-3 | 1.00 | 0.50 | 0.50 |  |
| probe-4 | 1.00 | 1.00 | 0.00 |  |
| probe-5 | 1.00 | 0.50 | 0.50 |  |
