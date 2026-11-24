#!/usr/bin/env bash
# Full Rust-port verification: cargo gates + every retargeted JS test, each separately, logs in tests-js/results/logs.
cd "$(dirname "$0")/../.."
export PATH="$HOME/.cargo/bin:$PATH"
out=rust/tests-js/results/logs; mkdir -p "$out"; sum=rust/tests-js/results/summary.txt; : > "$sum"
step() { local name=$1; shift; local t=$(date +%s); "$@" > "$out/$name.log" 2>&1; echo "$name exit=$? secs=$(( $(date +%s) - t ))" >> "$sum"; }
step cargo-fmt bash -c 'cd rust/app && cargo fmt --check'
step cargo-clippy bash -c 'cd rust/app && cargo clippy --all-targets -- -D warnings && cargo clippy --features test-hooks --all-targets -- -D warnings'
step cargo-test bash -c 'cd rust/app && cargo test --features test-hooks'
step cargo-build bash -c 'cd rust/app && cargo build --features test-hooks'
for t in smoke hover cursor minimizar drag reabrir extras luz; do step "$t" env -u ELECTRON_RUN_AS_NODE node "rust/tests-js/$t.js"; done
echo DONE >> "$sum"
