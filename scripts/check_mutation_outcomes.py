"""Require a nonempty, fully caught viable mutation corpus."""

import json
import sys
from pathlib import Path


def verify_mutants(data: dict) -> tuple[int, int]:
    fields = ("total_mutants", "caught", "unviable", "missed", "timeout", "success")
    counts = [data[field] for field in fields]
    if any(type(count) is not int or count < 0 for count in counts):
        raise ValueError("mutation counts must be nonnegative integers")
    total, caught, unviable, missed, timeout, success = counts
    if total != caught + unviable + missed + timeout + success:
        raise ValueError("mutation totals do not reconcile")
    if not caught or missed or timeout or success:
        raise ValueError(
            f"expected a nonempty caught corpus, got {caught} caught, "
            f"{missed} missed, {timeout} timed out, and {success} otherwise unresolved"
        )
    return caught, unviable


def main(path: Path) -> int:
    try:
        caught, unviable = verify_mutants(json.loads(path.read_text(encoding="utf-8")))
    except (OSError, TypeError, KeyError, ValueError) as error:
        print(f"mutation check: {error}", file=sys.stderr)
        return 1
    print(f"{caught} viable mutants caught, {unviable} unviable")
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: check_mutation_outcomes.py <outcomes.json>")
    sys.exit(main(Path(sys.argv[1])))
