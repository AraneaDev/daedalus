"""Regression tests for release PR check verification."""

import json
import os
import subprocess
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory


class ReleasePrChecksTests(unittest.TestCase):
    def verify(self, rollup, runs, *, head="new-head", token="app"):
        with TemporaryDirectory() as directory:
            tools = Path(directory)
            for name, body in {
                "gh": """case "$1 $2" in
    'pr view') printf '%s\n' "$PR_JSON" ;;
    'run list') printf '%s\n' "$RUNS_JSON" ;;
    *) exit 2 ;;
esac""",
                "sleep": "exit 0",
            }.items():
                tool = tools / name
                tool.write_text(f"#!/bin/sh\n{body}\n")
                tool.chmod(0o755)
            return subprocess.run(
                [
                    Path(__file__).with_name("release-pr.sh"),
                    "verify",
                    "55",
                    "release-branch",
                    "new-head",
                ],
                env={
                    **os.environ,
                    "PATH": f"{tools}:{os.environ['PATH']}",
                    "PR_JSON": json.dumps(
                        {"headRefOid": head, "statusCheckRollup": rollup}
                    ),
                    "RUNS_JSON": json.dumps(runs),
                    "RELEASE_PR_TOKEN": token,
                },
                capture_output=True,
                text=True,
                check=False,
            )

    def test_reports_rerun_when_pr_head_moves(self):
        result = self.verify([], [], head="old-head")
        self.assertEqual(result.returncode, 1)
        self.assertIn("moved past new-head", result.stdout)

    def test_approval_required_run_cannot_be_replaced_by_branch_checks(self):
        rollup = [
            {"name": "check", "workflowName": "CI"},
            {"name": "title", "workflowName": "Commit style"},
        ]
        waiting_ci = {
            "workflowName": "CI",
            "createdAt": "2026-11-26T00:00:00Z",
            "status": "completed",
            "conclusion": "action_required",
        }
        result = self.verify(rollup, [waiting_ci])
        self.assertEqual(result.returncode, 1)
        self.assertIn("GitHub requires approval", result.stdout)

        latest_ci = {
            **waiting_ci,
            "createdAt": "2026-11-26T00:00:01Z",
            "status": "in_progress",
            "conclusion": "",
        }
        result = self.verify(rollup, [latest_ci, waiting_ci])
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_builtin_token_reports_failed_to_attach_checks(self):
        result = self.verify(
            [{"name": "title", "workflowName": "Commit style"}],
            [],
            token="github",
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("Approve workflows to run", result.stdout)
