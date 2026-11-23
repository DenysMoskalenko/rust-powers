#!/usr/bin/env python3
"""Summarize a `claude plugin eval --json` result as the per-case markdown kept under maintenance/.

Stdlib only, Python 3.11+.

Usage:
    python3 scripts/summarize_evals.py RESULT.json [RESULT.json ...] > maintenance/eval-results-<date>.md
"""

from __future__ import annotations

import json
import sys
from collections import Counter, defaultdict
from pathlib import Path


def mean(values: list[float]) -> float | None:
    return sum(values) / len(values) if values else None


def fmt(value: float | None) -> str:
    return "-" if value is None else f"{value:.2f}"


def grader_rates(runs: list[dict]) -> dict[str, float]:
    passed: dict[str, list[bool]] = defaultdict(list)
    for run in runs:
        for grader in run.get("graders", []):
            passed[grader["name"]].append(bool(grader.get("passed")))
    return {name: sum(v) / len(v) for name, v in passed.items()}


def graded_runs(runs: list[dict], arm: str, notes: list[str]) -> list[dict]:
    graded: list[dict] = []
    missing: Counter[str] = Counter()
    for run in runs:
        if run.get("error"):
            missing["run error"] += 1
        elif run.get("skippedPaidGraders"):
            missing["paid graders skipped"] += 1
        elif run.get("score") is None:
            missing["score missing"] += 1
        else:
            graded.append(run)
    if missing:
        reasons = ", ".join(f"{count} {reason}" for reason, count in missing.items())
        notes.append(f"{arm}: {len(graded)}/{len(runs)} graded ({reasons})")
    return graded


def main() -> None:
    results = [json.loads(Path(path).read_text(encoding="utf-8")) for path in sys.argv[1:]]
    cost = sum(r.get("costUsd", 0) for r in results)
    judge = sum(run.get("judgeCostUsd", 0) for r in results for c in r["cases"] for arm in c["arms"].values() for run in arm)
    version = results[0].get("claudeVersion", "?") if results else "?"
    print(f"# Eval results\n\nClaude Code {version}; agent runs ${cost:.2f}, judge calls ${judge:.2f}.\n")
    print("Score is the mean fraction of graders passed over completed, graded runs. Δ = with minus without. Failing graders")
    print("lists every grader that failed in at least one run with the plugin, with its pass rate.\n")
    by_skill: dict[str, list[dict]] = defaultdict(list)
    for r in results:
        for case in r["cases"]:
            by_skill[case["name"].split(".")[0] if "." in case["name"] else "other"].append(case)
    for skill in sorted(by_skill):
        print(f"## {skill}\n\n| Case | With | Without | Δ | Failing graders (with) and incomplete runs |\n|---|---|---|---|---|")
        for case in sorted(by_skill[skill], key=lambda c: c["name"]):
            notes: list[str] = []
            with_runs = graded_runs(case["arms"].get("with", []), "with", notes)
            without_runs = graded_runs(case["arms"].get("without", []), "without", notes)
            w = mean([run["score"] for run in with_runs])
            wo = mean([run["score"] for run in without_runs])
            delta = None if w is None or wo is None else w - wo
            notes.extend(f"{name} {rate:.2f}" for name, rate in sorted(grader_rates(with_runs).items()) if rate < 1)
            print(f"| {case['name'].split('.', 1)[-1]} | {fmt(w)} | {fmt(wo)} | {fmt(delta)} | {', '.join(notes)} |")
        print()


if __name__ == "__main__":
    main()
