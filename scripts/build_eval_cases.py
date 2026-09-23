#!/usr/bin/env python3
"""Turn evals/<skill>.md into `claude plugin eval` cases under the gitignored evals/.run/.

Stdlib only, Python 3.11+. evals/.run/ becomes a clean plugin root: .claude-plugin/plugin.json,
a copy of skills/ without build output, the working-directory fixtures, and cases/<skill>/<case>/.
Earlier results in evals/.run/cases/results/ are kept.

Usage:
    python3 scripts/build_eval_cases.py [--out DIR]
    claude plugin eval evals/.run --eval-dir cases --no-publish
"""

from __future__ import annotations

import argparse
import re
import shutil
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RUN = ROOT / "evals" / ".run"
CASES = RUN / "cases"
FIXTURES = RUN / "fixtures"
SCAFFOLD = "skills/rust-scaffolding/assets/app"

# The add-when-needed dependency lines each add-on skill uncomments in the fixture's Cargo.toml.
ADDON_DEPS = {"building-rig-agents": ("rig", "rmcp"), "rust-nats": ("async-nats",), "rust-redis": ("redis",)}

TOOLS = "[Skill, Read, Glob, Grep]"
NO_EDITS = (
    "The tools that edit files or run commands are switched off in this session. "
    "Put the code and configuration changes in your reply."
)
LIMITS = {"trigger": (8, 300), "eval": (20, 900), "probe": (12, 600)}

NUMBERED_RE = re.compile(r"^\d+\. (.*)$")
WINNER_RE = re.compile(r"\s*->\s*`([a-z0-9-]+)`.*$")
LABEL_RE = re.compile(r"^\*\*(Prompt|Fixture|Wrong answer|Right answer)\*\*: *(.*)$")


def unquote(text: str) -> str:
    text = text.strip()
    if len(text) >= 2 and text[0] == text[-1] == '"':
        text = text[1:-1]
    return text.replace('\\"', '"')


def yaml_str(text: str) -> str:
    return "'" + text.replace("'", "''") + "'"


def bullets(block: list[str]) -> list[str]:
    """`- item` lines, with two-space continuation lines folded in."""
    items: list[str] = []
    for line in block:
        if line.startswith("- "):
            items.append(line[2:].strip())
        elif line.startswith("  ") and items:
            items[-1] += " " + line.strip()
    return items


def numbered(block: list[str]) -> list[str]:
    items: list[str] = []
    for line in block:
        if m := NUMBERED_RE.match(line):
            items.append(m.group(1).strip())
        elif line.startswith("   ") and items:
            items[-1] += " " + line.strip()
    return items


def sections(text: str, heading: str) -> list[tuple[str, list[str]]]:
    parts = re.split(rf"^### {heading} \d+ - (.*)$", text, flags=re.MULTILINE)
    return [(parts[i].strip(), parts[i + 1].splitlines()) for i in range(1, len(parts), 2)]


def block_after(lines: list[str], label: str) -> list[str]:
    start = next((i for i, ln in enumerate(lines) if ln.startswith(label)), None)
    if start is None:
        return []
    inline = lines[start][len(label) :].strip().removeprefix("- ")
    out: list[str] = [f"- {inline}"] if inline else []
    for ln in lines[start + 1 :]:
        if ln.startswith("**") or ln.startswith("#"):
            break
        out.append(ln)
    return out


def labels(lines: list[str]) -> dict[str, str]:
    """`**Label**: value`; a value runs on, newlines kept, until the next `**` label or heading."""
    out: dict[str, str] = {}
    current: str | None = None
    for ln in lines:
        if m := LABEL_RE.match(ln):
            current = m.group(1)
            out[current] = m.group(2)
        elif ln.startswith(("**", "### ")):
            current = None
        elif current:
            out[current] += "\n" + ln
    return {key: value.strip() for key, value in out.items()}


def backticked(value: str) -> str:
    return value[value.index("`") + 1 : value.rindex("`")]


def fired(skill: str, extra: str = "") -> str:
    pattern = r'"skill"\s*:\s*"(?:[\w-]+:)?' + skill + '"'
    return f"---\ntype: tool_used\ntool: Skill\ninput_match: {yaml_str(pattern)}\n{extra}---\n"


def write_case(skill: str, case: str, kind: str, prompt: str, fixture: str, graders: dict[str, str]) -> None:
    root = CASES / skill / case
    (root / "graders").mkdir(parents=True)
    max_turns, timeout = LIMITS[kind]
    front = [f"name: {case}", f"tags: [{skill}, {kind}]", f"max_turns: {max_turns}", f"timeout_seconds: {timeout}", f"allowed_tools: {TOOLS}"]
    if kind != "trigger":
        front.append(f"append_system_prompt: {yaml_str(NO_EDITS)}")
    (root / "prompt.md").write_text("---\n" + "\n".join(front) + "\n---\n\n" + prompt + "\n", encoding="utf-8")
    if fixture != "empty":
        (root / "setup.sh").write_text(f"#!/usr/bin/env bash\nset -euo pipefail\ncp -R '{FIXTURES / fixture}/.' .\n", encoding="utf-8")
        (root / "setup.sh").chmod(0o755)
        (root / "case.yaml").write_text(f'schema_version: "1.1"\nname: {case}\ncontext:\n  scaffold_script: setup.sh\n', encoding="utf-8")
    for name, body in graders.items():
        (root / "graders" / f"{name}.md").write_text(body, encoding="utf-8")


def judged(requirement: str, forbidden: bool, prompt: str) -> str:
    if forbidden:
        rule = f"FORBIDDEN: {requirement}\n\nFAIL if the response recommends or implements the forbidden thing. Naming it only to warn against it is not a FAIL.\nPASS otherwise."
    else:
        rule = f"REQUIRED: {requirement}\n\nPASS if the response clearly satisfies the requirement, in code or in explicit prose.\nFAIL if it does not, or only partially."
    quoted = "\n".join(f"> {line}" for line in prompt.splitlines())
    return f"---\ntype: llm\n---\n\nThe response answers this request about a Rust web service:\n\n{quoted}\n\nJudge only this one property.\n\n{rule}\n"


def fixture_for(skill: str, declared: str | None) -> str:
    if declared == "empty" or (declared is None and skill == "rust-scaffolding"):
        return "empty"
    return f"scaffold-{skill}" if skill in ADDON_DEPS else "scaffold"


def build_fixtures() -> None:
    ignore = shutil.ignore_patterns("target", ".DS_Store")
    shutil.copytree(ROOT / SCAFFOLD, FIXTURES / "scaffold", ignore=ignore)
    for skill, crates in ADDON_DEPS.items():
        dest = FIXTURES / f"scaffold-{skill}"
        shutil.copytree(FIXTURES / "scaffold", dest)
        cargo = dest / "Cargo.toml"
        text = cargo.read_text(encoding="utf-8")
        for crate in crates:
            text, n = re.subn(rf"^# ({re.escape(crate)} = .*)$", r"\1", text, flags=re.MULTILINE)
            if n != 1:
                raise SystemExit(f"{SCAFFOLD}/Cargo.toml: expected one commented `{crate} = ` line, found {n}")
        cargo.write_text(text, encoding="utf-8")


def build_skill(skill: str) -> int:
    text = (ROOT / "evals" / f"{skill}.md").read_text(encoding="utf-8")
    lines = text.splitlines()
    count = 0
    for n, item in enumerate(numbered(block_after(lines, "**Should load**")), start=1):
        write_case(skill, f"{skill}.load-{n:02}", "trigger", unquote(item), fixture_for(skill, None), {"fired": fired(skill)})
        count += 1
    for n, item in enumerate(numbered(block_after(lines, "**Should not load**")), start=1):
        m = WINNER_RE.search(item)
        winner = m.group(1) if m else None
        graders = {f"not-{skill}": fired(skill, "min: 0\nmax: 0\narm: both\n")}
        if winner:
            graders[f"fired-{winner}"] = fired(winner)
        fixture = "empty" if winner == "rust-scaffolding" else fixture_for(skill, "scaffold")
        write_case(skill, f"{skill}.skip-{n:02}", "trigger", unquote(WINNER_RE.sub("", item)), fixture, graders)
        count += 1
    eval_text = re.split(r"^### Probe \d+ - .*$", text, flags=re.MULTILINE)[0]
    for n, (_, body) in enumerate(sections(eval_text, "Eval"), start=1):
        meta = labels(body)
        prompt = unquote(meta["Prompt"])
        graders = {"fired": fired(skill)}
        for i, req in enumerate(bullets(block_after(body, "**Must produce**:")), start=1):
            graders[f"must-{i:02}"] = judged(req, forbidden=False, prompt=prompt)
        for i, req in enumerate(bullets(block_after(body, "**Must not produce**:")), start=1):
            graders[f"must-not-{i:02}"] = judged(req, forbidden=True, prompt=prompt)
        write_case(skill, f"{skill}.eval-{n}", "eval", prompt, fixture_for(skill, meta.get("Fixture")), graders)
        count += 1
    for n, (_, body) in enumerate(sections(text, "Probe"), start=1):
        meta = labels(body)
        graders = {"fired": fired(skill)}
        graders["wrong-answer"] = f"---\ntype: regex\npattern: {yaml_str(backticked(meta['Wrong answer']))}\nmatch: not_contains\n---\n"
        if "Right answer" in meta:
            graders["right-answer"] = f"---\ntype: regex\npattern: {yaml_str(backticked(meta['Right answer']))}\n---\n"
        write_case(skill, f"{skill}.probe-{n}", "probe", unquote(meta["Prompt"]), fixture_for(skill, meta.get("Fixture")), graders)
        count += 1
    return count


def main() -> None:
    global RUN, CASES, FIXTURES
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=RUN, help="plugin root to write (default: evals/.run)")
    RUN = parser.parse_args().out.resolve()
    CASES, FIXTURES = RUN / "cases", RUN / "fixtures"
    results = CASES / "results"
    kept = RUN.parent / ".results-keep"
    if results.is_dir():
        shutil.move(results, kept)
    shutil.rmtree(RUN, ignore_errors=True)
    (RUN / ".claude-plugin").mkdir(parents=True)
    shutil.copy2(ROOT / ".claude-plugin" / "plugin.json", RUN / ".claude-plugin" / "plugin.json")
    shutil.copytree(ROOT / "skills", RUN / "skills", ignore=shutil.ignore_patterns("target", ".DS_Store"))
    build_fixtures()
    CASES.mkdir()
    if kept.is_dir():
        shutil.move(kept, results)
    skills = sorted(d.name for d in (ROOT / "skills").iterdir() if (ROOT / "evals" / f"{d.name}.md").is_file())
    total = 0
    for skill in skills:
        n = build_skill(skill)
        total += n
        print(f"{skill}: {n} cases")
    print(f"{total} cases in {CASES}")


if __name__ == "__main__":
    main()
