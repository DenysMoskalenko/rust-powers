# Eval results, November 2026

Two new `sea-orm-postgres` cases for the UUID rule, run on 4 November 2026 with `claude plugin eval`
(Claude Code 2.1.318) on Opus 5.5.

- **Runs.** Two runs per arm. The with arm has the plugin installed and the without arm does not.
- **Working directory.** A copy of the scaffold, whose migrations already show `DEFAULT uuidv7()`,
  except the row marked empty.
- **Grading.** Probe 6 by regex, Eval 8 by a Sonnet judge.
- **Cost.** $4.56 including judge calls: $2.78 of a $3 cap, $1.20 of a $1.50 cap for the Eval 8
  baseline, and $0.58 of a $1 cap for the empty fixture.

| Case | With plugin | Without plugin |
| --- | --- | --- |
| Probe 6, `uuidv7()` present | 2 of 2 | 2 of 2 |
| Probe 6, v4 regex absent | 1 of 2 | 1 of 2 |
| Eval 8, every bullet | 2 of 2 | 2 of 2 |
| Probe 6 on an empty fixture, both regexes | 2 of 2 | 0 of 2 |

The data comes with three caveats:

1. **Eval 8 ties.** Its first without arm ran past the `--max-cost-usd` ceiling and lost its paid
   graders, so the baseline was re-run alone from a plugin-free copy of the case. Both replies
   followed the scaffold's `post` migration and used `id: NotSet`.
2. **Probe 6 shows no difference between arms.** The first regex also matched prose, against the
   rule in `evals/README.md`, and the replies were not kept, so the two misses may be warnings about
   `gen_random_uuid()` rather than code. The regex now matches only a column default in code.
3. **The scaffold teaches the rule.** Its migrations show `uuidv7()`, so the without arm copies it.
   Probe 6 therefore moved to `**Fixture**: empty`, where both without-arm replies chose
   `Expr::cust("gen_random_uuid()")` and both with-arm replies chose `uuidv7()`.

The empty-fixture Probe 6 justifies the UUID bullet in `## Important`: a Δ of 1.0 at two runs per
arm. Eval 8 stays on the scaffold, so it checks the pattern, not what the skill adds.
