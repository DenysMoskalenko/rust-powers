#!/usr/bin/env python3
"""Independently grade the files in one retained artifact eval workspace."""

import argparse
import os
import re
import shutil
import subprocess
import tempfile
import tomllib
from pathlib import Path

ARTIFACTS = Path(__file__).resolve().parent.parent / "evals" / "artifacts"


def check_scaffold(project: Path) -> None:
    manifest = tomllib.loads((project / "Cargo.toml").read_text(encoding="utf-8"))
    if manifest["package"]["name"] != "library-svc":
        raise SystemExit("expected a library-svc package in the workspace root")
    for path in (".config/nextest.toml", ".github/workflows/ci.yml", "migration/Cargo.toml", "tests/api/health.rs"):
        if not (project / path).is_file():
            raise SystemExit(f"missing starter file: {path}")

    expected = {
        "Dockerfile": [
            r"^RUN [^\n]*--bin library-svc(?:\s|$)",
            r"^COPY [^\n]*/release/library-svc /usr/local/bin/library-svc$",
            r'^CMD \[\s*"library-svc"\s*\]$',
        ],
        "docker-compose.yml": [r"^name: library-svc$", r"^ +POSTGRES_DB: library_svc$"],
        ".env.example": [
            r"^APP__DATABASE__URL=[^\n]+/library_svc$",
            r"^DATABASE_URL=[^\n]+/library_svc$",
            r"^RUST_LOG=(?:[^\n]*,)?library_svc=[^,\n]+",
        ],
    }
    for path, patterns in expected.items():
        value = (project / path).read_text(encoding="utf-8")
        for pattern in patterns:
            if not re.search(pattern, value, re.MULTILINE):
                raise SystemExit(f"incomplete library-svc rename in {path}: {pattern}")


def grade(case: str, workspace: Path) -> None:
    if not (workspace / "Cargo.toml").is_file():
        raise SystemExit(f"{workspace}: missing Cargo.toml")
    with tempfile.TemporaryDirectory(prefix="rust-powers-grade-") as scratch:
        project = Path(scratch) / "project"
        shutil.copytree(workspace, project, symlinks=True, ignore=shutil.ignore_patterns(".git", "target"))
        if any(path.is_symlink() for path in project.rglob("*")):
            raise SystemExit("the artifact workspace must be self-contained, without symlinks")
        env = {**os.environ, "CARGO_TARGET_DIR": os.environ.get("CARGO_TARGET_DIR", str(Path(scratch) / "target"))}
        if case == "existing":
            test = project / "tests" / "rust_powers_ledger_contract.rs"
            test.parent.mkdir(exist_ok=True)
            test.write_bytes((ARTIFACTS / "ledger_contract.rs").read_bytes())
            args = ["test", "--test", test.stem]
            required_tests = ("serves_batches_and_keeps_the_ledger_contract",)
        else:
            check_scaffold(project)
            test = project / "tests" / "rust_powers_openapi_title.rs"
            test.write_text(
                'use utoipa::OpenApi;\n#[test]\nfn renamed_openapi_title() {\n'
                '    assert_eq!(library_svc::ApiDoc::openapi().info.title, "library-svc");\n}\n', encoding="utf-8",
            )
            args = ["test", "--locked", "--workspace"]
            required_tests = ("health::liveness_is_ok_and_touches_nothing", "renamed_openapi_title")
        result = subprocess.run(["cargo", *args], cwd=project, env=env, timeout=1800, stdout=subprocess.PIPE, text=True)
        print(result.stdout, end="")
        if result.returncode:
            raise SystemExit(result.returncode)
        executed = set(result.stdout.splitlines())
        for name in required_tests:
            if f"test {name} ... ok" not in executed:
                raise SystemExit(f"required test did not run: {name}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("case", choices=("existing", "greenfield"))
    parser.add_argument("workspace", type=Path)
    args = parser.parse_args()
    grade(args.case, args.workspace)
