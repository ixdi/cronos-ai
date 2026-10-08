#!/usr/bin/env bash
set -euo pipefail

cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
cargo audit --deny warnings
