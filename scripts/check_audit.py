"""Fail on RustSec vulnerabilities, yanks and new informational advisories."""

import json
import subprocess
import sys

# Reviewed transitive warnings. A version change requires a fresh review.
# glyphon 0.12 calls pop_lru, not the affected pop operation.
# Font loading and the Linux accessibility adapter require the other four crates.
REVIEWED = {
    ("unsound", "RUSTSEC-2026-0253", "lru", "0.16.4"),
    ("unmaintained", "RUSTSEC-2026-0192", "ttf-parser", "0.25.1"),
    ("unmaintained", "RUSTSEC-2026-0371", "zbus_names", "4.3.5"),
    ("unmaintained", "RUSTSEC-2026-0370", "zvariant", "5.15.1"),
    ("unmaintained", "RUSTSEC-2026-0372", "zvariant_derive", "5.15.1"),
}


def audit() -> int:
    """Keep every warning visible and propagate database or vulnerability failures."""
    result = subprocess.run(
        ["cargo", "audit", "--json"], stdout=subprocess.PIPE, check=False
    )
    if not result.stdout:
        return result.returncode or 1
    status = check_report(json.loads(result.stdout))
    return result.returncode or status


def check_report(report: dict) -> int:
    """Apply the exact-version warning policy to one cargo audit report."""
    failed = report["vulnerabilities"]["found"]
    for vulnerability in report["vulnerabilities"]["list"]:
        advisory = vulnerability["advisory"]
        print(f"{advisory['id']} {advisory['title']}", file=sys.stderr)
    for kind, warnings in report["warnings"].items():
        for warning in warnings:
            advisory = warning.get("advisory") or {}
            package = warning["package"]
            key = (kind, advisory.get("id"), package["name"], package["version"])
            reviewed = key in REVIEWED
            print("reviewed" if reviewed else "new", *key, file=sys.stderr)
            failed |= not reviewed
    return int(failed)


if __name__ == "__main__":
    sys.exit(audit())
