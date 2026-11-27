"""Check production dependencies between workspace members from Cargo metadata."""

import json
import sys
from pathlib import Path

EXPECTED = {
    ("daedalus-ctl", "daedalus-proto"),
    ("daedalus-ui", "daedalus-proto"),
    ("daedalus-vt", "daedalus-proto"),
    ("daedalusd", "daedalus-proto"),
    ("daedalusd", "daedalus-vt"),
}


def main():
    metadata = json.load(sys.stdin)
    members = set(metadata["workspace_members"])
    packages = [pkg for pkg in metadata["packages"] if pkg["id"] in members]
    member_paths = {
        (pkg["name"], Path(pkg["manifest_path"]).resolve().parent) for pkg in packages
    }
    # The resolved graph omits optional dependencies behind disabled features.
    # Cargo reports the original package name in "name" even for renamed dependencies.
    edges = {
        (pkg["name"], dep["name"])
        for pkg in packages
        for dep in pkg["dependencies"]
        if dep["kind"] != "dev"
        and dep["source"] is None
        and dep.get("path")
        and (dep["name"], Path(dep["path"]).resolve()) in member_paths
    }
    if edges != EXPECTED:
        sys.exit(
            f"Unexpected edges {sorted(edges - EXPECTED)}, missing edges "
            f"{sorted(EXPECTED - edges)}"
        )


if __name__ == "__main__":
    main()
