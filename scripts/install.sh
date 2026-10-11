#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
tools=${AMONG_TOOLS:-"$root/tools/among-tools/target/release/among-tools"}
if [ ! -x "$tools" ]; then
    if [ -n "${AMONG_TOOLS:-}" ]; then
        echo 'AMONG_TOOLS must point to a prebuilt native among-tools executable.' >&2
        exit 1
    fi
    if ! command -v cargo >/dev/null 2>&1; then
        echo 'Installer bootstrap requires Cargo/Rust >= 1.98 or prebuilt AMONG_TOOLS with its sibling among-uninstall.' >&2
        exit 1
    fi
    cargo build --release --locked --manifest-path "$root/tools/among-tools/Cargo.toml" \
        --target-dir "$root/tools/among-tools/target" --bins
fi
exec "$tools" install "$root" "$@"
