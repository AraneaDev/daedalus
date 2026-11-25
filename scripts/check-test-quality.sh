#!/usr/bin/env bash
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
cargo llvm-cov clean --workspace
cargo llvm-cov --workspace --locked --no-report
if [[ $(uname -s) == Linux ]]; then
    cargo llvm-cov --locked -p daedalus-ui --test linux_desktop --no-report -- --ignored
fi
cargo llvm-cov report --fail-under-lines 92.9 --fail-under-functions 92.5 --fail-under-regions 92.4
mkdir -p target/quality
cargo mutants --file crates/daedalus-proto/src/codec.rs --output target/quality/frames --cargo-arg=--locked -- frame_
cargo mutants -p daedalus-vt --re 'Grid::erase_in_line' --output target/quality/erase --cargo-arg=--locked
