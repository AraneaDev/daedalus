#!/usr/bin/env bash
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
git ls-files -z -- '*.md' | xargs -0 rumdl check
git ls-files -z -- '*.sh' '.githooks/*' | xargs -0 shellcheck
# makepkg consumes the metadata and supplies srcdir and pkgdir.
shellcheck -s bash -e SC2034,SC2154 packaging/arch/PKGBUILD.in
ruff check scripts
ruff format --check scripts
python3 -B -m unittest discover -s scripts
actionlint
