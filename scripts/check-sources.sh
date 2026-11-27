#!/usr/bin/env bash
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
git ls-files -z -- '*.md' | xargs -0 rumdl check
shell_sources=()
found_bash_init=false
while IFS= read -r -d '' path; do
    shell_sources+=("$path")
    if [[ "$path" == crates/daedalusd/src/shell_integration/bash-init.bash ]]; then
        found_bash_init=true
    fi
done < <(git ls-files -z -- '*.sh' '*.bash' '.githooks/*')
if [[ "$found_bash_init" != true ]]; then
    echo "ShellCheck selection must include the embedded Bash startup file" >&2
    exit 1
fi
shellcheck "${shell_sources[@]}"
# makepkg consumes the metadata and supplies srcdir and pkgdir.
shellcheck -s bash -e SC2034,SC2154 packaging/arch/PKGBUILD.in
ruff check scripts
ruff format --check scripts
python3 -B -m unittest discover -s scripts
actionlint
