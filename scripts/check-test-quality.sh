#!/usr/bin/env bash
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
cargo llvm-cov --workspace --locked --fail-under-lines 79 --fail-under-functions 80 --fail-under-regions 80
mkdir -p target/quality
cargo mutants --file crates/daedalus-proto/src/codec.rs --output target/quality/frames -- --locked frame_
cargo mutants -p daedalus-vt --re 'Grid::erase_in_line' --output target/quality/erase -- --locked
