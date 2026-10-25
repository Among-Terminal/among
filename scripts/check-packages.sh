#!/bin/sh
# the examples are independent: a successful host build cannot validate them.
set -eu
cd "$(dirname "$0")/.."
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo test --locked --example fake-plugin
cargo build --release --locked
cargo fmt --check --manifest-path examples/plugins/Cargo.toml
cargo clippy --locked --all-targets --manifest-path examples/plugins/Cargo.toml -- -D warnings
cargo test --locked --manifest-path examples/plugins/Cargo.toml
cargo build --release --locked --manifest-path examples/plugins/Cargo.toml
cargo fmt --check --manifest-path tools/among-tools/Cargo.toml
cargo clippy --locked --all-targets --manifest-path tools/among-tools/Cargo.toml -- -D warnings
# promoted_snap_identity runs here too, but only meaningfully where built Snaps
# exist. Without AMONG_SNAP_ARTIFACTS it prints an explicit SKIPPED line and
# asserts nothing; snap-build.yml sets that variable with the real artifacts, so
# the gate is actually enforced exactly once, in the job that builds them.
cargo test --locked --manifest-path tools/among-tools/Cargo.toml
cargo build --release --locked --manifest-path tools/among-tools/Cargo.toml
