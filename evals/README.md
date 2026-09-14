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
2. ... (five or more, including the Python vocabulary a reader arrives with)

**Should not load**

1. <prompt> -> `<skill-that-should-win>`
2. ... (five or more, each naming the owning skill)

### Eval 1 - <short name>

**Prompt**: <what the user asks>

**Must produce**:

- <observable, checkable property of a good answer>

**Must not produce**:

- <the specific wrong pattern this eval exists to catch>
```

What the validator checks: one `### Triggering` heading; a `**Should load**` block with at
least five `N. ` lines; a `**Should not load**` block with at least five `N. ` lines, each
containing `` -> `skill` ``; at least three `### Eval N - <name>` headings, each followed by
the three labelled lines `**Prompt**:`, `**Must produce**:`, `**Must not produce**:`.

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
