"""Refresh Cargo.lock for a release and verify only workspace versions changed."""

import json
import subprocess
import tomllib
from pathlib import Path


lockfile = Path("Cargo.lock")
before = tomllib.loads(lockfile.read_text())
metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--format-version", "1"]))
workspace = set(metadata["workspace_members"])
versions = {p["name"]: p["version"] for p in metadata["packages"] if p["id"] in workspace}
for package in before["package"]:
    if "source" not in package and package["name"] in versions:
        package["version"] = versions[package["name"]]

if tomllib.loads(lockfile.read_text()) != before:
    raise SystemExit("Cargo changed more than workspace package versions in Cargo.lock")
