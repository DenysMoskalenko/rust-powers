#!/usr/bin/env python3
"""Compile every verifiable Rust snippet in skills/ against the scaffold app.

Stdlib only, Python 3.11+.

  ```rust,verify        -> verify/src/bin/<skill>_<file>_<n>.rs   (cargo check + clippy)
                           (blocks mentioning sea_orm_migration go to verify/migration/src/bin/)
  ```rust,verify,test   -> verify/tests/<skill>_<file>_<n>.rs     (cargo nextest run)
  ```rust,ignore        -> deliberately non-compiling, skipped
  ```rust               -> fragment, skipped

`verify/` is fully generated and gitignored: it is a copy of
skills/rust-scaffolding/assets/app/ plus the extracted snippets.

Usage:
    python3 scripts/check_snippets.py [--dry-run] [--skip-tests]
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SKILLS = ROOT / "skills"
ASSETS = SKILLS / "rust-scaffolding" / "assets" / "app"
VERIFY = ROOT / "verify"

BIN_TAG = "rust,verify"
TEST_TAG = "rust,verify,test"
IGNORE_TAG = "rust,ignore"
FRAGMENT_TAG = "rust"

ALLOW_LINTS = [
    "dead_code",
    "unused",
    "clippy::print_stdout",
    "clippy::print_stderr",
    "clippy::unused_async",
    "clippy::unused_async_trait_impl",
]
ALLOW_REASON = "extracted skill snippet: examples print and implement async trait signatures"


def allow_prefix(code: str) -> str:
    """Crate-level allow for lints that example code legitimately trips.

    A lint the snippet already allows itself is left out, otherwise clippy's
    `duplicated_attributes` fires on the pair."""
    own = re.findall(r"#!\[allow\(([^)]*)\)", code)
    already = {name.strip() for group in own for name in group.split(",")}
    lints = [l for l in ALLOW_LINTS if l not in already]
    return f"#![allow({', '.join(lints)}, reason = \"{ALLOW_REASON}\")]\n"

# Crates STACK.md keeps commented out as "add when needed"; snippets may use any of them.
ADD_WHEN_NEEDED = [
    'rig = { version = "0.42", features = ["memory", "rmcp", "test-utils"] }',
    'rmcp = { version = "2", features = ["client", "macros", "transport-streamable-http-client-reqwest"] }',
    'deadpool-redis = "0.23"',
    'sha2 = "0.11"',
    'jsonwebtoken = "11"',
    'argon2 = "0.6"',
    'axum-extra = { version = "0.12", features = ["typed-header"] }',
    'nutype = { version = "0.7", features = ["serde"] }',
    'async-nats = "0.50"',
    'redis = { version = "1.7", features = ["tokio-comp", "tokio-rustls-comp", "connection-manager", "script"] }',
]


@dataclass
class Block:
    skill: str
    source: str  # path relative to ROOT
    line: int  # 1-based line of the opening fence
    kind: str  # "bin" | "test"
    code: str
    stem: str = ""

    @property
    def origin(self) -> str:
        return f"{self.source}:{self.line}"


@dataclass
class Stats:
    bins: int = 0
    tests: int = 0
    ignored: int = 0
    fragments: int = 0
    other: int = 0


def slug(text: str) -> str:
    return re.sub(r"[^a-z0-9]+", "_", text.lower()).strip("_")


def extract(md: Path, skill: str, stats: Stats) -> list[Block]:
    """Pull every tagged fence out of one markdown file."""
    lines = md.read_text(encoding="utf-8").splitlines()
    rel = md.relative_to(ROOT).as_posix()
    blocks: list[Block] = []
    counter = 0
    i = 0
    while i < len(lines):
        if not lines[i].startswith("```"):
            i += 1
            continue
        tag = lines[i][3:].strip()
        open_line = i + 1
        j = i + 1
        while j < len(lines) and not lines[j].startswith("```"):
            j += 1
        body = "\n".join(lines[i + 1 : j])
        i = j + 1

        if tag == TEST_TAG:
            kind = "test"
            stats.tests += 1
        elif tag == BIN_TAG:
            kind = "bin"
            stats.bins += 1
        elif tag == IGNORE_TAG:
            stats.ignored += 1
            continue
        elif tag == FRAGMENT_TAG:
            stats.fragments += 1
            continue
        else:
            stats.other += 1
            continue

        counter += 1
        stem = f"{slug(skill)}_{slug(md.stem)}_{counter}"
        blocks.append(Block(skill=skill, source=rel, line=open_line, kind=kind, code=body, stem=stem))
    return blocks


def collect() -> tuple[list[Block], dict[str, Stats]]:
    blocks: list[Block] = []
    per_skill: dict[str, Stats] = {}
    for skill_dir in sorted(d for d in SKILLS.iterdir() if d.is_dir() and not d.name.startswith(".")):
        stats = per_skill.setdefault(skill_dir.name, Stats())
        for md in sorted(skill_dir.rglob("*.md")):
            if ASSETS in md.parents:  # the scaffold's own docs are not skill content
                continue
            blocks.extend(extract(md, skill_dir.name, stats))
    return blocks, per_skill


def is_migration(block: Block) -> bool:
    """Migration snippets live in the migration member: its derive macros expand to
    absolute ::sea_orm_migration:: paths, which only resolve where that crate is a
    direct dependency."""
    return block.kind == "bin" and "sea_orm_migration" in block.code


def render(block: Block) -> str:
    root_crate = "migration" if is_migration(block) else "app"
    code = block.code.replace("crate::", f"{root_crate}::")
    out = allow_prefix(code) + code
    if not out.endswith("\n"):
        out += "\n"
    if block.kind == "bin" and "fn main" not in code:
        out += "\nfn main() {}\n"
    return out


def ensure_dependencies(cargo_toml: Path) -> list[str]:
    """Append the add-when-needed crates to [dependencies] if absent."""
    lines = cargo_toml.read_text(encoding="utf-8").splitlines()
    try:
        start = lines.index("[dependencies]")
    except ValueError:
        raise SystemExit(f"{cargo_toml}: no [dependencies] section")
    end = next(
        (i for i in range(start + 1, len(lines)) if lines[i].startswith("[") and lines[i].rstrip().endswith("]")),
        len(lines),
    )
    present = {
        m.group(1)
        for ln in lines[start + 1 : end]
        if (m := re.match(r'^\s*"?([A-Za-z0-9_-]+)"?\s*=', ln))
    }
    added = []
    for entry in ADD_WHEN_NEEDED:
        name = entry.split("=", 1)[0].strip()
        if name not in present:
            added.append(entry)
    if added:
        lines[end:end] = added
        cargo_toml.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return added


def print_summary(per_skill: dict[str, Stats]) -> None:
    print(f"{'skill':<28}{'bins':>6}{'tests':>7}{'ignored':>9}{'fragments':>11}")
    print("-" * 61)
    total = Stats()
    for name, s in per_skill.items():
        print(f"{name:<28}{s.bins:>6}{s.tests:>7}{s.ignored:>9}{s.fragments:>11}")
        total.bins += s.bins
        total.tests += s.tests
        total.ignored += s.ignored
        total.fragments += s.fragments
    print("-" * 61)
    print(f"{'TOTAL':<28}{total.bins:>6}{total.tests:>7}{total.ignored:>9}{total.fragments:>11}\n")


def run_cargo(cmd: list[str], blocks: list[Block]) -> bool:
    print(f"$ {' '.join(cmd)}   (in verify/)")
    # verify/ is regenerated every run, so insta snapshots never pre-exist: accept
    # the first capture instead of failing on it. Snapshot *content* is the skill
    # author's concern (cargo insta review in their own project), not the checker's.
    env = {**os.environ, "INSTA_UPDATE": "always"}
    proc = subprocess.run(cmd, cwd=VERIFY, text=True, capture_output=True, env=env)
    sys.stdout.write(proc.stdout)
    sys.stderr.write(proc.stderr)
    if proc.returncode == 0:
        return True
    output = proc.stdout + proc.stderr
    blamed = [b for b in blocks if b.stem in output]
    if blamed:
        print("\nFailing snippet(s) came from:", file=sys.stderr)
        for b in blamed:
            print(f"  {b.stem}.rs  <-  {b.origin}", file=sys.stderr)
    return False


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true", help="extract and summarize only; write nothing")
    parser.add_argument("--skip-tests", action="store_true", help="skip cargo nextest run")
    args = parser.parse_args()

    if not SKILLS.is_dir():
        print("skills/ not found", file=sys.stderr)
        return 1

    blocks, per_skill = collect()
    print_summary(per_skill)

    if args.dry_run:
        for b in blocks:
            print(f"{b.origin}  ->  {'tests' if b.kind == 'test' else 'src/bin'}/{b.stem}.rs")
        if not ASSETS.is_dir():
            print(f"\nnote: {ASSETS.relative_to(ROOT)} does not exist yet, so a full run is not possible.")
        return 0

    if not ASSETS.is_dir():
        print(
            f"{ASSETS.relative_to(ROOT)} does not exist yet - the rust-scaffolding assets are required "
            f"to compile snippets. Use --dry-run until they land.",
            file=sys.stderr,
        )
        return 1

    if VERIFY.exists():
        shutil.rmtree(VERIFY)
    shutil.copytree(ASSETS, VERIFY)
    (VERIFY / "src" / "bin").mkdir(parents=True, exist_ok=True)
    (VERIFY / "tests").mkdir(parents=True, exist_ok=True)

    (VERIFY / "migration" / "src" / "bin").mkdir(parents=True, exist_ok=True)
    for b in blocks:
        if b.kind == "test":
            sub = "tests"
        elif is_migration(b):
            sub = "migration/src/bin"
        else:
            sub = "src/bin"
        (VERIFY / sub / f"{b.stem}.rs").write_text(render(b), encoding="utf-8")

    added = ensure_dependencies(VERIFY / "Cargo.toml")
    if added:
        print("added to verify/Cargo.toml [dependencies]:")
        for entry in added:
            print(f"  {entry}")
        print()

    commands = [
        ["cargo", "check", "--workspace", "--all-targets"],
        ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
    ]
    if not args.skip_tests:
        commands.append(["cargo", "nextest", "run", "--workspace"])

    for cmd in commands:
        if not run_cargo(cmd, blocks):
            return 1
    print("\nall snippets compile, lint clean" + ("" if args.skip_tests else ", and tests pass"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
