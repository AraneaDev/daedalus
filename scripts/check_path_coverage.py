"""Enforce per-file line coverage floors in an LLVM JSON export."""

import json
import sys
from pathlib import Path


def repo_relative(filename: str, root: Path) -> str | None:
    path = Path(filename)
    if not path.is_absolute():
        path = root / path
    try:
        return path.resolve().relative_to(root).as_posix()
    except ValueError:
        return None


def check(report: dict, floors: dict, root: Path) -> bool:
    if not isinstance(floors, dict) or not floors:
        raise ValueError("coverage policy must specify at least one target")
    if report["type"] != "llvm.coverage.json.export":
        raise ValueError("expected LLVM coverage JSON export")

    summaries = {path: [] for path in floors}
    for export in report["data"]:
        for file in export["files"]:
            name = repo_relative(file["filename"], root)
            if name in summaries:
                summaries[name].append(file["summary"]["lines"])

    passed = True
    for path, floor in floors.items():
        if (
            repo_relative(path, root) != path
            or type(floor) is not int
            or not 1 <= floor <= 100
        ):
            raise ValueError(f"{path}: invalid repository-relative path or percentage")
        if len(summaries[path]) != 1:
            print(
                f"{path}: expected one coverage summary, got {len(summaries[path])}",
                file=sys.stderr,
            )
            passed = False
            continue

        lines = summaries[path][0]
        covered, count = lines.get("covered"), lines.get("count")
        if (
            type(covered) is not int
            or type(count) is not int
            or not 0 <= covered <= count
            or count == 0
        ):
            print(f"{path}: invalid line counts", file=sys.stderr)
            passed = False
            continue

        meets_floor = covered * 100 >= count * floor
        message = f"{path}: {covered / count:.2%} ({covered}/{count}), floor {floor}%"
        print(message, file=sys.stdout if meets_floor else sys.stderr)
        passed &= meets_floor
    return passed


def main(report_path: Path, floors_path: Path) -> int:
    try:
        report = json.loads(report_path.read_text(encoding="utf-8"))
        floors = json.loads(floors_path.read_text(encoding="utf-8"))
        return 0 if check(report, floors, Path.cwd().resolve()) else 1
    except (OSError, TypeError, AttributeError, KeyError, ValueError) as error:
        print(f"coverage check: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit("usage: check_path_coverage.py <llvm-cov-json> <floors-file>")
    sys.exit(main(Path(sys.argv[1]), Path(sys.argv[2])))
