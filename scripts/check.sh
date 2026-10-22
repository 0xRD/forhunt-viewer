#!/usr/bin/env bash
# Fast local gate before a commit: fmt, clippy -D warnings, tests.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -q -- -D warnings
cargo test --workspace --locked -q 2>&1 | grep -E "^test result|FAILED|panicked" | sort | uniq -c
