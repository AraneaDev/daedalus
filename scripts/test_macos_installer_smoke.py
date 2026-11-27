"""Exercise the installed macOS smoke with disposable daemon, UI, and CLI fixtures."""

import os
import subprocess
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

SMOKE = Path(__file__).resolve().parents[1] / "packaging/test-macos-installed.sh"

FIXTURE = """#!/usr/bin/env python3
import json
import os
import signal
import sys
from pathlib import Path

root = Path(os.environ["DAEDALUS_RUNTIME_DIR"])
socket = Path(os.environ["DAEDALUS_SOCKET"])
assert socket == root / "daemon.sock"
assert Path(os.environ["DAEDALUS_EVENTS_SOCKET"]) == root / "events.sock"
assert Path(os.environ["DAEDALUS_STATE_FILE"]) == root / "state.json"
program = Path(sys.argv[0]).name
args = sys.argv[1:]
inventory = root / "workspace.json"
project = root / "installer-smoke"
workspace = "5a895f32-25aa-47fc-b559-b63b28bd3a1d"
other_workspace = "30b8e71f-09ae-4ad8-a43b-f16e7f40bc0a"
pane_ids = [
    "d232f728-3811-42e1-858c-f85d0a322197",
    "5905a603-2734-42df-9067-8368dd1e1a4f",
    "99659e63-969e-4a31-92f0-9955a5a06261",
]
case = os.environ["SMOKE_FIXTURE_CASE"]


def publish(workspace_id, panes):
    temp = inventory.with_suffix(".tmp")
    temp.write_text(json.dumps([{
        "id": workspace_id,
        "name": "installer-smoke",
        "root_dir": str(project),
        "pane_ids": panes,
    }]))
    temp.replace(inventory)


def hydrate(workspace_id, panes):
    for pane in panes:
        print(
            "2026-11-27T12:00:00Z DEBUG daedalus_ui::bridge::session_init: "
            f"terminal snapshot hydrated workspace_id={workspace_id} pane_id={pane}",
            flush=True,
        )


if program == "daedalusd":
    Path(os.environ["SMOKE_FIXTURE_RUNTIME"]).write_text(str(root))
    print("fixture daemon listening", flush=True)
    socket.touch()
elif program == "daedalus-ctl":
    if not socket.exists():
        sys.exit(1)
    if args == ["workspace", "list", "--json"]:
        print(inventory.read_text() if inventory.exists() else "[]")
    else:
        assert args == ["ping"], args
    sys.exit(0)
else:
    assert program == "daedalus-ui", program
    assert args == ["--dir", str(project), "--agent", "shell", "--no-auto-spawn"], args
    assert Path(os.environ["DAEDALUS_CONFIG"]) == root / "config.toml"
    if not inventory.exists():
        print("fixture first UI created the workspace", flush=True)
        # This log is deliberately complete. A second idle process must still fail.
        hydrate(workspace, pane_ids)
        publish(workspace, pane_ids)
    else:
        assert os.environ["NO_COLOR"] == "1"
        assert os.environ["RUST_LOG"] == "info,daedalus_ui::bridge::session_init=debug"
        print("fixture second UI started", flush=True)
        if case == "early-exit":
            sys.exit(7)
        if case == "partial":
            hydrate(workspace, pane_ids[:2])
        if case == "other-workspace":
            hydrate(other_workspace, pane_ids)
        if case == "longer-field":
            hydrate(workspace, [*pane_ids[:2], pane_ids[2] + "f"])
        if case == "changed-inventory":
            publish(workspace, [*pane_ids[:2], other_workspace])
            hydrate(workspace, pane_ids)
        if case == "success":
            publish(workspace, pane_ids[::-1])
            hydrate(workspace, pane_ids)

signal.pause()
"""


class MacosInstallerSmokeTests(unittest.TestCase):
    def smoke(self, case):
        with TemporaryDirectory() as directory:
            directory = Path(directory)
            programs = [
                directory / name
                for name in ("daedalus-ui", "daedalusd", "daedalus-ctl")
            ]
            for program in programs:
                program.write_text(FIXTURE)
                program.chmod(0o755)
            runtime_marker = directory / "runtime"
            result = subprocess.run(
                ["bash", SMOKE, *programs],
                env={
                    **os.environ,
                    "DAEDALUS_SMOKE_TIMEOUT_SECONDS": "2",
                    "SMOKE_FIXTURE_CASE": case,
                    "SMOKE_FIXTURE_RUNTIME": str(runtime_marker),
                },
                capture_output=True,
                text=True,
                timeout=15,
                check=False,
            )
            if runtime_marker.exists():
                self.assertFalse(Path(runtime_marker.read_text()).exists())
            return result

    def test_requires_fresh_hydration_for_every_original_pane(self):
        for case in ("inert", "partial", "other-workspace", "longer-field"):
            with self.subTest(case=case):
                result = self.smoke(case)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("Timed out waiting for the reopened UI", result.stderr)
                self.assertIn("fixture daemon listening", result.stderr)
                self.assertIn("fixture first UI created the workspace", result.stderr)
                self.assertIn("fixture second UI started", result.stderr)

    def test_reports_early_reopened_process_exit(self):
        result = self.smoke("early-exit")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("The reopened UI exited", result.stderr)

    def test_rejects_an_inventory_change_after_hydration(self):
        result = self.smoke("changed-inventory")
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("Reconnect changed the workspace or pane IDs", result.stderr)

    def test_accepts_live_reconnect_with_the_same_pane_ids_in_any_order(self):
        result = self.smoke("success")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("reopened UI hydrated all 3 original panes", result.stdout)
