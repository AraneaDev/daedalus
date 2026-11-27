#!/usr/bin/env bash
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --locked --no-report
if [[ $(uname -s) == Linux ]]; then
    cargo llvm-cov --locked -p daedalus-ui --test linux_desktop --no-report -- --ignored
    xvfb-run -a env -u WAYLAND_DISPLAY cargo llvm-cov --locked -p daedalus-ui --bin daedalus-ui \
        --no-report -- --ignored controller_tests::coverage_paths::terminal_copy_and_paste_use_the_real_clipboard_and_focused_pane
fi
cargo llvm-cov report --fail-under-lines 92.9 --fail-under-functions 92.5 --fail-under-regions 92.4
mkdir -p target/quality
cargo llvm-cov report --json --summary-only --output-path target/quality/coverage.json
if [[ $(uname -s) == Linux ]]; then
    python3 scripts/check_path_coverage.py target/quality/coverage.json scripts/coverage-floors.json
fi
cargo mutants --file crates/daedalus-proto/src/codec.rs --output target/quality/frames --cargo-arg=--locked -- frame_
cargo mutants -p daedalus-vt --re 'Grid::erase_in_line' --output target/quality/erase --cargo-arg=--locked

# Exercise managed state and setup revision handling with production mutants.
cargo mutants -j 1 --file crates/daedalusd/src/workbench/store.rs \
    --re 'WorkbenchStore::read_workspace|next_revision' \
    --exclude-re 'replace \+= with ([-*])= in WorkbenchStore::read_workspace' \
    --output target/quality/workbench --cargo-arg=--locked -- --lib workbench::store::tests
python3 scripts/check_mutation_outcomes.py target/quality/workbench/mutants.out/outcomes.json
