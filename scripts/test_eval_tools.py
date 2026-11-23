import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import build_eval_cases as builder
from grade_eval_workspace import check_scaffold, grade


class EvalToolsTest(unittest.TestCase):
    def test_generator_refuses_unmarked_directories_and_source_trees(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            sentinel = root / "keep"
            sentinel.write_text("untouched")
            with self.assertRaisesRegex(SystemExit, "choose an unused path"):
                builder.regenerate(root)
            with patch.object(builder, "ROOT", root), self.assertRaisesRegex(SystemExit, "repository sources"):
                builder.regenerate(root / "skills" / "draft")
            self.assertEqual("untouched", sentinel.read_text())
            self.assertFalse((root / "skills").exists())

    def test_rebuild_keeps_results_in_place_and_adopts_the_legacy_default(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            shutil.copytree(builder.ROOT / ".claude-plugin", root / ".claude-plugin")
            shutil.copytree(builder.ROOT / builder.SCAFFOLD, root / builder.SCAFFOLD, ignore=shutil.ignore_patterns("target"))
            (root / "skills" / "rust-nats").mkdir()
            shutil.copytree(builder.ROOT / "evals" / "artifacts", root / "evals" / "artifacts")
            shutil.copy2(builder.ROOT / "evals" / "rust-nats.md", root / "evals" / "rust-nats.md")
            out = root / "evals" / ".run"
            with patch.object(builder, "ROOT", root):
                builder.regenerate(out)
                (out / builder.OUTPUT_MARKER).unlink()
                saved = out / "cases" / "rust-nats" / "results" / "last.json"
                saved.parent.mkdir()
                saved.write_text("paid results")
                result_inode = saved.stat().st_ino
                artifact_results = out / "artifacts" / "existing" / "results"
                artifact_results.mkdir()
                (artifact_results / "artifact-check.txt").write_text("historical run")
                legacy = out / "skills" / "unrelated.txt"
                legacy.write_text("retain unmarked contents")
                builder.regenerate(out)
                self.assertTrue((out / builder.OUTPUT_MARKER).is_file())
                backup, = out.glob(".previous-*/skills/unrelated.txt")
                self.assertEqual("retain unmarked contents", backup.read_text())
                obsolete = out / "cases" / "rust-nats" / "obsolete.md"
                obsolete.touch()
                builder.regenerate(out)
                self.assertFalse(obsolete.exists())
                self.assertEqual(result_inode, saved.stat().st_ino)
                self.assertEqual("paid results", saved.read_text())
                self.assertEqual("historical run", (artifact_results / "artifact-check.txt").read_text())

    def test_greenfield_rejects_a_partial_rename_and_unregistered_starter_test(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            project = Path(temp) / "library-svc"
            template = builder.ROOT / builder.SCAFFOLD
            shutil.copytree(template, project, ignore=shutil.ignore_patterns("target"))
            replacements = {
                "Cargo.toml": [('name = "app"', 'name = "library-svc"')],
                "Dockerfile": [
                    ("--bin app", "--bin library-svc"),
                    ("/release/app", "/release/library-svc"),
                    ("/bin/app", "/bin/library-svc"),
                    ('CMD ["app"]', 'CMD ["library-svc"]'),
                ],
                "docker-compose.yml": [("name: app", "name: library-svc"), ("POSTGRES_DB: app", "POSTGRES_DB: library_svc")],
                ".env.example": [("/app", "/library_svc"), ("app=debug", "library_svc=debug")],
            }
            for name, changes in replacements.items():
                path = project / name
                text = path.read_text()
                for original, renamed in changes:
                    text = text.replace(original, renamed)
                path.write_text(text)
            check_scaffold(project)
            for name in ("Dockerfile", "docker-compose.yml", ".env.example"):
                path = project / name
                renamed = path.read_text()
                shutil.copyfile(template / name, path)
                with self.subTest(name=name), self.assertRaisesRegex(SystemExit, "incomplete library-svc rename"):
                    check_scaffold(project)
                path.write_text(renamed)
            cargo = subprocess.CompletedProcess(["cargo", "test"], 0, stdout="test renamed_openapi_title ... ok\n")
            with patch("grade_eval_workspace.subprocess.run", return_value=cargo), self.assertRaisesRegex(SystemExit, "health::liveness"):
                grade("greenfield", project)

    def test_report_excludes_skipped_and_errored_runs_but_counts_real_failures(self) -> None:
        def run_report(baseline: list[dict]) -> str:
            case = {"name": "demo.eval-1", "arms": {"with": [{"score": 1}], "without": baseline}}
            with tempfile.TemporaryDirectory() as temp:
                result = Path(temp) / "result.json"
                result.write_text(json.dumps({"cases": [case]}))
                return subprocess.check_output(
                    [sys.executable, str(builder.ROOT / "scripts" / "summarize_evals.py"), str(result)], text=True,
                )

        skipped = {"score": 0, "skippedPaidGraders": True, "error": None}
        failed = {"score": 0, "error": "timeout"}
        self.assertIn("| 1.00 | - | - | without: 0/2 graded", run_report([skipped, failed]))
        self.assertIn("| 1.00 | 1.00 | 0.00 | without: 1/2 graded", run_report([skipped, {"score": 1}]))
        self.assertIn("| 1.00 | 0.00 | 1.00 |", run_report([{"score": 0}]))
