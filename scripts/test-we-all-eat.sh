#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
# No validator, RPC, wallet or mainnet transactions are used by this script.
manifest="programs/we-all-eat/Cargo.toml"
sbf_builder="${CARGO_BUILD_SBF:-cargo-build-sbf}"
"$sbf_builder" --manifest-path "$manifest" --sbf-out-dir programs/we-all-eat/target/deploy
cargo +stable test --locked --manifest-path "$manifest" --features no-entrypoint,svm-tests
cargo +stable test --locked --manifest-path crates/we-all-eat-math/Cargo.toml
cargo +stable clippy --locked --manifest-path "$manifest" --all-targets --features no-entrypoint,svm-tests -- -D warnings
