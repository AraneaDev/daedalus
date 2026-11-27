"""Check production dependencies between workspace members from Cargo metadata."""

import json
import sys

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
    names = {
        pkg["id"]: pkg["name"] for pkg in metadata["packages"] if pkg["id"] in members
    }
    edges = {
        (names[node["id"]], names[dep["pkg"]])
        for node in metadata["resolve"]["nodes"]
        if node["id"] in members
        for dep in node["deps"]
        if dep["pkg"] in members
        and any(kind["kind"] != "dev" for kind in dep["dep_kinds"])
    }
    if edges != EXPECTED:
        sys.exit(
            f"Unexpected edges {sorted(edges - EXPECTED)}, missing edges "
            f"{sorted(EXPECTED - edges)}"
        )


if __name__ == "__main__":
    main()
