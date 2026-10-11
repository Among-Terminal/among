#!/bin/sh
# Build first: a failed compilation must not replace working installed copies.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cargo build --release --locked --bins --manifest-path "$root/examples/plugins/Cargo.toml"
cargo run --release --locked --example install --manifest-path "$root/examples/plugins/Cargo.toml" -- "$@"
