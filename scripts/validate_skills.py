#!/usr/bin/env python3
"""Validate every skills/*/SKILL.md against the rust-powers house rules.

Stdlib only, Python 3.11+. Prints `file:line: LEVEL: message`.
Exit 1 if any ERROR was found; warnings alone exit 0.

Usage:
    python3 scripts/validate_skills.py [--json] [--strict] [--base REF]
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass, asdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SKILLS = ROOT / "skills"
STACK = ROOT / "STACK.md"
EVALS = ROOT / "evals"
SCAFFOLD_CARGO = SKILLS / "rust-scaffolding" / "assets" / "app" / "Cargo.toml"

# The house set is deliberately smaller than the spec's: every skill carries exactly
# `name`, `description` and `metadata.version`.
ALLOWED_KEYS = {"name", "description", "metadata"}
NAME_RE = re.compile(r"^[a-z0-9]+(-[a-z0-9]+)*$")
RESERVED_IN_NAME = ("claude", "anthropic")
FORBIDDEN_IN_DESCRIPTION = "<>#"
DESCRIPTION_PREFIX = "Use when"
DESCRIPTION_MAX_WORDS = 75

# Body budgets: (warn, error)
LINE_BUDGET = (250, 300)
WORD_BUDGET = (1500, 2000)

# Fence tags an agent may write. Anything else is a typo or an unverifiable snippet.
ALLOWED_FENCES = {
    "",  # bare fence
    "rust",  # fragment - not extracted, not verified
    "rust,verify",  # compiled as a bin
    "rust,verify,test",  # compiled and run as an integration test
    "rust,ignore",  # deliberately non-compiling
    "toml",
    "yaml",
    "json",
    "bash",
    "sh",
    "console",
    "text",
    "sql",
    "diff",
    "dockerfile",
    "makefile",
    "ini",
    "env",
    "mermaid",
}

# Repository plumbing must not leak into skill bodies. No skill is exempt: a skill is read
# inside somebody else's project, where none of these paths exist.
REPO_PATH_PATTERNS = ["tmp/research", "tmp/evals", "verify/", "evals/", "make evals", "scripts/validate_skills", "scripts/check_snippets", "scripts/build_eval_cases", "STACK.md"]

# Cargo.toml sections that must match STACK.md verbatim.
PINNED_CARGO_SECTIONS = ("[dependencies]", "[dev-dependencies]", "[workspace.lints.rust]", "[workspace.lints.clippy]")

# A markdown link from one references/*.md to another. SKILL.md is the only index;
# a reference may link to SKILL.md or to an anchor in itself, nothing else.
MD_LINK_RE = re.compile(r"\]\(([^)#\s]+)(?:#[^)]*)?\)")

# evals/<skill>.md structure (see evals/README.md).
EVAL_TRIGGERING_RE = re.compile(r"^### Triggering\s*$", re.MULTILINE)
EVAL_SHOULD_LOAD_RE = re.compile(r"^\*\*Should load\*\*\s*$", re.MULTILINE)
EVAL_SHOULD_NOT_LOAD_RE = re.compile(r"^\*\*Should not load\*\*\s*$", re.MULTILINE)
EVAL_HEADING_RE = re.compile(r"^### Eval (\d+) - \S", re.MULTILINE)
EVAL_NUMBERED_RE = re.compile(r"^\d+\. \S")
EVAL_WINNER_RE = re.compile(r"-> `[a-z0-9-]+`")
PROBE_SPLIT_RE = re.compile(r"^### Probe \d+ - .*$", re.MULTILINE)
FIXTURE_RE = re.compile(r"^\*\*Fixture\*\*:(.*)$", re.MULTILINE)
WRONG_ANSWER_RE = re.compile(r"^\*\*Wrong answer\*\*: `.+`\s*$", re.MULTILINE)
FIXTURES = {"scaffold", "empty"}

# The skill set does not talk about Python (settled 2026-09-14); these words are how it slips back in.
PYTHON_WORDS_RE = re.compile(r"\b(python|pytest|fastapi|pydantic|polyfactory|uv)\b|lambda:|monkey-?patch", re.IGNORECASE)

# Every manifest carries the same release version; Claude Code ships an update only when it changes.
MANIFESTS = (
    (ROOT / ".claude-plugin" / "plugin.json", ("version",)),
    (ROOT / ".claude-plugin" / "marketplace.json", ("version",)),
    (ROOT / ".claude-plugin" / "marketplace.json", ("metadata", "version")),
    (ROOT / ".codex-plugin" / "plugin.json", ("version",)),
    (ROOT / ".cursor-plugin" / "plugin.json", ("version",)),
)


@dataclass
class Finding:
    file: str
    line: int
    level: str
    message: str

    def render(self) -> str:
        return f"{self.file}:{self.line}: {self.level}: {self.message}"


class Report:
    def __init__(self) -> None:
        self.findings: list[Finding] = []

    def add(self, path: Path, line: int, level: str, message: str) -> None:
        try:
            rel = path.relative_to(ROOT)
        except ValueError:
            rel = path
        self.findings.append(Finding(str(rel), line, level, message))

    def error(self, path: Path, line: int, message: str) -> None:
        self.add(path, line, "ERROR", message)

    def warn(self, path: Path, line: int, message: str) -> None:
        self.add(path, line, "WARN", message)

    @property
    def error_count(self) -> int:
        return sum(1 for f in self.findings if f.level == "ERROR")


# --------------------------------------------------------------------------- YAML


def parse_frontmatter(lines: list[str], path: Path, report: Report) -> dict[str, tuple[str, int]]:
    """Tiny YAML subset: `key: value`, quoted scalars, booleans, one nested map level.

    Returns {key: (raw_value_text, line_number)}. Nested maps get raw value "".
    """
    if not lines or lines[0].rstrip("\n") != "---":
        report.error(path, 1, "missing YAML frontmatter (file must start with ---)")
        return {}
    end = next((i for i, ln in enumerate(lines[1:], start=1) if ln.rstrip("\n") == "---"), None)
    if end is None:
        report.error(path, 1, "frontmatter is never closed with ---")
        return {}

    out: dict[str, tuple[str, int]] = {}
    current_parent: str | None = None
    for idx in range(1, end):
        lineno = idx + 1
        raw = lines[idx].rstrip("\n")
        if not raw.strip() or raw.lstrip().startswith("#"):
            continue
        indented = raw[:1].isspace()
        if indented:
            if current_parent is None:
                report.error(path, lineno, f"indented line has no parent key: {raw.strip()!r}")
            elif ":" not in raw:
                report.error(path, lineno, f"unparseable nested frontmatter line: {raw.strip()!r}")
            else:
                child, _, value = raw.strip().partition(":")
                out[f"{current_parent}.{child.strip()}"] = (value.strip(), lineno)
            continue
        if ":" not in raw:
            report.error(path, lineno, f"unparseable frontmatter line: {raw.strip()!r}")
            continue
        key, _, value = raw.partition(":")
        key = key.strip()
        value = value.strip()
        if key in out:
            report.error(path, lineno, f"duplicate frontmatter key {key!r}")
        out[key] = (value, lineno)
        current_parent = key if value == "" else None
    return out


def unquote(value: str) -> str:
    if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
        return value[1:-1]
    return value


# --------------------------------------------------------------------------- checks


def check_frontmatter(skill_dir: Path, path: Path, fm: dict[str, tuple[str, int]], report: Report) -> None:
    unknown = {key for key in fm if "." not in key} - ALLOWED_KEYS
    for key in sorted(unknown):
        report.error(path, fm[key][1], f"unknown frontmatter key {key!r} (allowed: {', '.join(sorted(ALLOWED_KEYS))})")

    if "metadata.version" not in fm or not unquote(fm["metadata.version"][0]):
        report.error(path, fm["metadata"][1] if "metadata" in fm else 1, "frontmatter needs `metadata:` with a `version:` line")

    if "name" not in fm:
        report.error(path, 1, "frontmatter is missing required key 'name'")
    else:
        raw, lineno = fm["name"]
        name = unquote(raw)
        if not NAME_RE.match(name):
            report.error(path, lineno, f"name {name!r} must match ^[a-z0-9]+(-[a-z0-9]+)*$")
        if len(name) > 64:
            report.error(path, lineno, f"name is {len(name)} chars, max 64")
        if name != skill_dir.name:
            report.error(path, lineno, f"name {name!r} must equal the directory name {skill_dir.name!r}")
        for reserved in RESERVED_IN_NAME:
            if reserved in name.lower():
                report.error(path, lineno, f"name must not contain the reserved word {reserved!r}")

    if "description" not in fm:
        report.error(path, 1, "frontmatter is missing required key 'description'")
        return
    raw, lineno = fm["description"]
    if not (len(raw) >= 2 and raw.startswith('"') and raw.endswith('"')):
        report.error(path, lineno, "description must be a double-quoted YAML scalar")
    text = unquote(raw)
    if not 1 <= len(text) <= 1024:
        report.error(path, lineno, f"description is {len(text)} chars, must be 1-1024")
    bad = sorted({c for c in FORBIDDEN_IN_DESCRIPTION if c in text})
    if bad:
        report.error(path, lineno, f"description must not contain {' '.join(repr(c) for c in bad)}")
    if not text.startswith(DESCRIPTION_PREFIX):
        report.error(path, lineno, f'description must start with "{DESCRIPTION_PREFIX}"')
    n_words = len(text.split())
    if n_words > DESCRIPTION_MAX_WORDS:
        report.error(path, lineno, f"description is {n_words} words, max {DESCRIPTION_MAX_WORDS}")


def check_budgets(path: Path, lines: list[str], body_start: int, report: Report) -> None:
    body = lines[body_start:]
    n_lines = len(body)
    n_words = len(" ".join(body).split())
    warn_l, err_l = LINE_BUDGET
    warn_w, err_w = WORD_BUDGET
    if n_lines > err_l:
        report.error(path, body_start + 1, f"body is {n_lines} lines, hard cap {err_l}")
    elif n_lines > warn_l:
        report.warn(path, body_start + 1, f"body is {n_lines} lines, target {warn_l}")
    if n_words > err_w:
        report.error(path, body_start + 1, f"body is {n_words} words, hard cap {err_w}")
    elif n_words > warn_w:
        report.warn(path, body_start + 1, f"body is {n_words} words, target {warn_w}")


def check_fences(path: Path, lines: list[str], report: Report) -> None:
    inside = False
    for idx, raw in enumerate(lines, start=1):
        if not raw.startswith("```"):
            continue
        if inside:
            inside = False
            continue
        inside = True
        tag = raw[3:].strip()
        if tag not in ALLOWED_FENCES:
            report.error(path, idx, f"fence tag ```{tag} is not in the allowed set")
    if inside:
        report.error(path, len(lines), "unclosed code fence")


def check_reference_links(path: Path, lines: list[str], report: Report) -> None:
    """A references/*.md may link to SKILL.md or to itself; never to another reference."""
    for idx, raw in enumerate(lines, start=1):
        for target in MD_LINK_RE.findall(raw):
            if "://" in target or target.endswith("SKILL.md"):
                continue
            if target.endswith(".md"):
                report.error(path, idx, f"reference links to another file {target!r}; SKILL.md is the only index")


def check_eval_file(path: Path, report: Report) -> None:
    """The exact format in evals/README.md, so ownership tests are machine-checkable."""
    text = path.read_text(encoding="utf-8")
    if not EVAL_TRIGGERING_RE.search(text):
        report.error(path, 1, "missing '### Triggering' heading")
        return
    lines = text.splitlines()

    def numbered_after(marker: re.Pattern[str]) -> list[tuple[int, str]]:
        m = marker.search(text)
        if m is None:
            return []
        start = text[: m.start()].count("\n") + 1
        items: list[tuple[int, str]] = []
        for lineno in range(start + 1, len(lines) + 1):
            ln = lines[lineno - 1]
            if ln.startswith("#") or ln.startswith("**"):
                break
            if EVAL_NUMBERED_RE.match(ln):
                items.append((lineno, ln))
        return items

    if not EVAL_SHOULD_LOAD_RE.search(text):
        report.error(path, 1, "missing '**Should load**' block")
    elif len(numbered_after(EVAL_SHOULD_LOAD_RE)) < 5:
        report.error(path, 1, "'**Should load**' needs at least five numbered prompts")
    if not EVAL_SHOULD_NOT_LOAD_RE.search(text):
        report.error(path, 1, "missing '**Should not load**' block")
    else:
        items = numbered_after(EVAL_SHOULD_NOT_LOAD_RE)
        if len(items) < 5:
            report.error(path, 1, "'**Should not load**' needs at least five numbered prompts")
        for lineno, ln in items:
            if not EVAL_WINNER_RE.search(ln):
                report.error(path, lineno, "should-not-load prompt must end with '-> `<skill-that-should-win>`'")

    evals = EVAL_HEADING_RE.findall(text)
    if len(evals) < 3:
        report.error(path, 1, f"needs at least three '### Eval N - <name>' blocks, found {len(evals)}")
    eval_part = PROBE_SPLIT_RE.split(text)[0]
    for n, section in enumerate(re.split(r"^### Eval \d+ - .*$", eval_part, flags=re.MULTILINE)[1:], start=1):
        for label in ("**Prompt**:", "**Must produce**:", "**Must not produce**:"):
            if label not in section:
                report.error(path, 1, f"Eval {n} is missing the {label!r} line")
    for n, section in enumerate(PROBE_SPLIT_RE.split(text)[1:], start=1):
        if "**Prompt**:" not in section:
            report.error(path, 1, f"Probe {n} is missing the '**Prompt**:' line")
        if not WRONG_ANSWER_RE.search(section):
            report.error(path, 1, f"Probe {n} needs a '**Wrong answer**: `<regex>`' line")
    for m in FIXTURE_RE.finditer(text):
        if m.group(1).strip() not in FIXTURES:
            report.error(path, text[: m.start()].count("\n") + 1, f"**Fixture** must be one of {sorted(FIXTURES)}")


def check_repo_paths(path: Path, lines: list[str], report: Report) -> None:
    for idx, raw in enumerate(lines, start=1):
        for pattern in REPO_PATH_PATTERNS:
            if pattern in raw:
                report.error(path, idx, f"skill content must not mention repository plumbing {pattern!r}")
        if m := PYTHON_WORDS_RE.search(raw):
            report.error(path, idx, f"skill content must not talk about Python ({m.group(0)!r})")


def check_references(skill_dir: Path, skill_md: Path, body: str, report: Report) -> None:
    refs = skill_dir / "references"
    if not refs.is_dir():
        return
    if not re.search(r"^##+\s+References\b", body, re.MULTILINE):
        report.error(skill_md, 1, "references/ exists but SKILL.md has no '## References' heading")
    for item in sorted(refs.rglob("*")):
        rel = item.relative_to(refs)
        if len(rel.parts) > 1:
            report.error(item, 1, f"references/ must be one level deep, found {rel.as_posix()!r}")
            continue
        if item.is_dir():
            report.error(item, 1, f"references/ must contain files only, found directory {rel.as_posix()!r}")
            continue
        if item.suffix != ".md":
            continue
        lines = item.read_text(encoding="utf-8").splitlines()
        if len(lines) <= 100:
            continue
        head = lines[:20]
        has_heading = any(ln.startswith("#") and re.search(r"contents|toc", ln, re.IGNORECASE) for ln in head)
        link_list = sum(1 for ln in head if re.match(r"^\s*[-*] \[[^\]]+\]\(#[^)]+\)", ln))
        if not has_heading and link_list < 3:
            report.error(item, 1, f"reference is {len(lines)} lines (>100) and needs a TOC in the first 20 lines (a Contents heading or a list of anchor links)")


def validate_skill(skill_dir: Path, report: Report, strict: bool) -> None:
    skill_md = skill_dir / "SKILL.md"
    if not skill_md.is_file():
        report.error(skill_dir, 1, "skill directory has no SKILL.md")
        return

    lines = skill_md.read_text(encoding="utf-8").splitlines()
    fm = parse_frontmatter(lines, skill_md, report)
    check_frontmatter(skill_dir, skill_md, fm, report)

    body_start = 0
    if lines and lines[0] == "---":
        close = next((i for i, ln in enumerate(lines[1:], start=1) if ln == "---"), None)
        body_start = (close + 1) if close is not None else 0
    body_text = "\n".join(lines[body_start:])

    check_budgets(skill_md, lines, body_start, report)
    check_fences(skill_md, lines, report)
    check_repo_paths(skill_md, lines, report)
    check_references(skill_dir, skill_md, body_text, report)

    if (skill_dir / "README.md").exists():
        report.error(skill_dir / "README.md", 1, "a skill directory must not contain a README.md")
    if (skill_dir / "scripts").exists():
        report.error(skill_dir / "scripts", 1, "skills ship instructions, not scripts: no scripts/ inside a skill")
    if (skill_dir / "reference").is_dir():
        report.error(skill_dir / "reference", 1, "supporting material goes in 'references/' (plural), not 'reference/'")
    if not (skill_dir / "agents" / "openai.yaml").is_file():
        report.error(skill_dir / "agents" / "openai.yaml", 1, "missing agents/openai.yaml (Codex per-skill metadata)")
    for dirpath, dirnames, _ in os.walk(skill_dir):
        if "target" in dirnames:
            dirnames.remove("target")
            report.warn(Path(dirpath) / "target", 1, "build output inside a skill: local plugin installs copy it and `claude plugin eval` refuses its hard links; delete it")

    eval_file = EVALS / f"{skill_dir.name}.md"
    if not eval_file.is_file():
        level = report.error if strict else report.warn
        level(eval_file, 1, f"missing evaluation scenarios for {skill_dir.name} (see evals/README.md)")
    else:
        check_eval_file(eval_file, report)

    for ref in sorted((skill_dir / "references").glob("*.md")) if (skill_dir / "references").is_dir() else []:
        ref_lines = ref.read_text(encoding="utf-8").splitlines()
        check_fences(ref, ref_lines, report)
        check_repo_paths(ref, ref_lines, report)
        check_reference_links(ref, ref_lines, report)


# --------------------------------------------------------------------------- Cargo.toml parity


def stack_cargo_block() -> tuple[list[str], int] | None:
    """The ```toml block that follows STACK.md's `## Cargo.toml` heading."""
    if not STACK.is_file():
        return None
    lines = STACK.read_text(encoding="utf-8").splitlines()
    start = next((i for i, ln in enumerate(lines) if re.match(r"^##\s+Cargo\.toml\s*$", ln)), None)
    if start is None:
        return None
    open_idx = next((i for i in range(start + 1, len(lines)) if lines[i].startswith("```")), None)
    if open_idx is None:
        return None
    close_idx = next((i for i in range(open_idx + 1, len(lines)) if lines[i].startswith("```")), None)
    if close_idx is None:
        return None
    return lines[open_idx + 1 : close_idx], open_idx + 2


def pinned_sections(lines: list[str]) -> dict[str, list[str]]:
    """Significant (non-blank, non-comment) lines of each pinned section."""
    out: dict[str, list[str]] = {}
    current: str | None = None
    for raw in lines:
        stripped = raw.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            current = stripped if stripped in PINNED_CARGO_SECTIONS else None
            if current:
                out.setdefault(current, [])
            continue
        if current is None or not stripped or stripped.startswith("#"):
            continue
        out[current].append(re.sub(r"\s*#.*$", "", stripped).strip())
    return out


def check_cargo_parity(report: Report) -> None:
    block = stack_cargo_block()
    if block is None:
        report.warn(STACK, 1, "could not locate the ```toml block under '## Cargo.toml'; skipping Cargo parity check")
        return
    stack_lines, stack_line0 = block
    if not SCAFFOLD_CARGO.is_file():
        report.warn(
            STACK,
            stack_line0,
            "skills/rust-scaffolding/assets/app/Cargo.toml does not exist yet; skipping Cargo parity check",
        )
        return

    want = pinned_sections(stack_lines)
    got = pinned_sections(SCAFFOLD_CARGO.read_text(encoding="utf-8").splitlines())
    for section in PINNED_CARGO_SECTIONS:
        w, g = want.get(section), got.get(section)
        if w is None:
            report.warn(STACK, stack_line0, f"STACK.md Cargo block has no {section} section")
            continue
        if g is None:
            report.error(SCAFFOLD_CARGO, 1, f"missing {section} section required by STACK.md")
            continue
        for line in sorted(set(w) - set(g)):
            report.error(SCAFFOLD_CARGO, 1, f"{section}: STACK.md pins `{line}` but the scaffold does not")
        for line in sorted(set(g) - set(w)):
            report.error(SCAFFOLD_CARGO, 1, f"{section}: scaffold has `{line}` which STACK.md does not pin")


# --------------------------------------------------------------------------- release versions


def manifest_version(text: str, keys: tuple[str, ...]) -> str | None:
    value = json.loads(text)
    for key in keys:
        value = value.get(key) if isinstance(value, dict) else None
    return value if isinstance(value, str) else None


def check_manifest_versions(report: Report) -> None:
    seen: dict[str, list[str]] = {}
    for path, keys in MANIFESTS:
        if not path.is_file():
            report.error(path, 1, "plugin manifest is missing")
            continue
        version = manifest_version(path.read_text(encoding="utf-8"), keys)
        if version is None:
            report.error(path, 1, f"no version at {'.'.join(keys)}")
            continue
        seen.setdefault(version, []).append(f"{path.relative_to(ROOT)}:{'.'.join(keys)}")
    if len(seen) > 1:
        report.error(ROOT / ".claude-plugin" / "plugin.json", 1, f"manifest versions disagree: {seen}")


def git_show(ref: str, rel: str) -> str | None:
    result = subprocess.run(["git", "show", f"{ref}:{rel}"], cwd=ROOT, capture_output=True, text=True)
    return result.stdout if result.returncode == 0 else None


def check_version_bumps(report: Report, base: str) -> None:
    """A change under skills/<name>/ must bump that skill's metadata.version and the plugin version."""
    diff = subprocess.run(["git", "diff", "--name-only", base, "--", "skills/"], cwd=ROOT, capture_output=True, text=True)
    if diff.returncode != 0:
        report.error(ROOT, 1, f"git diff against {base!r} failed: {diff.stderr.strip()}")
        return
    changed = sorted({Path(p).parts[1] for p in diff.stdout.split() if len(Path(p).parts) > 2})
    for name in changed:
        rel = f"skills/{name}/SKILL.md"
        old = git_show(base, rel)
        path = ROOT / rel
        if old is None or not path.is_file():
            continue
        version_re = re.compile(r"^  version: *(.+)$", re.MULTILINE)
        before, after = version_re.search(old), version_re.search(path.read_text(encoding="utf-8"))
        if before and after and before.group(1) == after.group(1):
            report.error(path, 1, f"skills/{name}/ changed since {base} but metadata.version is still {after.group(1)}")
    if changed:
        rel = ".claude-plugin/plugin.json"
        old = git_show(base, rel)
        if old is not None and manifest_version(old, ("version",)) == manifest_version((ROOT / rel).read_text(encoding="utf-8"), ("version",)):
            report.error(ROOT / rel, 1, f"skills/ changed since {base} but the plugin version did not: Claude Code ships updates only on a version change")


# --------------------------------------------------------------------------- main


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", action="store_true", help="emit findings as JSON")
    parser.add_argument("--strict", action="store_true", help="treat missing evals/<skill>.md as an error")
    parser.add_argument("--base", metavar="REF", help="require version bumps for skills changed since this git ref")
    args = parser.parse_args()

    report = Report()
    if not SKILLS.is_dir():
        print(f"{SKILLS}: ERROR: skills/ directory not found", file=sys.stderr)
        return 1

    skill_dirs = sorted(d for d in SKILLS.iterdir() if d.is_dir() and not d.name.startswith("."))
    for skill_dir in skill_dirs:
        validate_skill(skill_dir, report, args.strict)
    check_cargo_parity(report)
    check_manifest_versions(report)
    if args.base:
        check_version_bumps(report, args.base)

    if args.json:
        print(json.dumps([asdict(f) for f in report.findings], indent=2))
    else:
        for finding in report.findings:
            print(finding.render())
        warns = len(report.findings) - report.error_count
        print(f"\n{len(skill_dirs)} skill(s) checked: {report.error_count} error(s), {warns} warning(s)")
    return 1 if report.error_count else 0


if __name__ == "__main__":
    sys.exit(main())
