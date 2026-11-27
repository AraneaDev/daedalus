"""Exercise the command used by CI to enforce per-file line coverage."""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("check_path_coverage.py")
TARGET = "crates/daedalus-ui/src/terminal_commands.rs"


class PathCoverageTests(unittest.TestCase):
    def run_policy(self, files, *, floors=None, absolute=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            if absolute:
                for file in files:
                    file["filename"] = str(root / file["filename"])
            report = root / "report.json"
            policy = root / "floors.json"
            report.write_text(
                json.dumps(
                    {
                        "type": "llvm.coverage.json.export",
                        "data": [{"files": files}],
                    }
                )
            )
            policy.write_text(json.dumps({TARGET: 85} if floors is None else floors))
            return subprocess.run(
                [sys.executable, str(SCRIPT), str(report), str(policy)],
                cwd=root,
                capture_output=True,
                text=True,
                check=False,
            )

    @staticmethod
    def record(path, covered, count):
        return {
            "filename": path,
            "summary": {"lines": {"covered": covered, "count": count}},
        }

    def test_fails_when_high_coverage_cannot_offset_the_target(self):
        result = self.run_policy(
            [
                self.record(TARGET, 84, 100),
                self.record("other.rs", 5000, 5000),
            ]
        )
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("84.00%", result.stderr)

    def test_missing_duplicate_and_unrelated_suffix_do_not_pass(self):
        record = self.record(TARGET, 90, 100)
        for files in (
            [],
            [record, record],
            [self.record(f"vendored/{TARGET}", 90, 100)],
        ):
            with self.subTest(files=files):
                result = self.run_policy(files)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn("expected one coverage summary", result.stderr)

    def test_zero_and_malformed_counts_do_not_pass(self):
        for covered, count in [(0, 0), (101, 100), (-1, 100), (True, 1), (90, "100")]:
            with self.subTest(covered=covered, count=count):
                result = self.run_policy([self.record(TARGET, covered, count)])
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn("invalid line counts", result.stderr)

    def test_equality_and_absolute_checkout_path_pass(self):
        filename = str(Path.cwd() / TARGET)
        # The subprocess uses its fixture as the checkout root, so the source
        # checkout filename is deliberately outside that root.
        out_of_tree = self.run_policy([self.record(filename, 85, 100)])
        self.assertEqual(out_of_tree.returncode, 1, out_of_tree.stderr)
        for absolute in (False, True):
            result = self.run_policy([self.record(TARGET, 85, 100)], absolute=absolute)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("85.00%", result.stdout)

    def test_empty_policy_cannot_pass(self):
        result = self.run_policy([], floors={})
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("at least one target", result.stderr)
