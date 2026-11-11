#!/bin/bash
# Installs snaps from the Snap Store while tolerating a temporarily
# unreachable store. GitHub-hosted runners and the detached clean-runtime
# container both talk to the same store; a single failed call ends the step at
# a point where the rest of the job cannot continue, so this script repeats the
# whole call and reports every attempt. The last attempt still fails the step:
# runs never continue as if the snap were installed.
#
# Usage:
#   bash scripts/ci/snap-store-install.sh [--in-container NAME] <snap install arguments>
#
# Without the flag the command is `snap install ...`, run through sudo when the
# caller is not root. With `--in-container NAME` it is
# `docker exec NAME snap install ...`, because the clean-runtime checks install
# core24 inside their own detached container. Local files are installed with
# `--dangerous` by the caller directly; they never reach this script.
#
# Environment:
#   AMONG_SNAP_ATTEMPTS  calls to try (default 5)
#   AMONG_SNAP_PAUSE     seconds between calls (default 30)
set -euo pipefail

attempts=${AMONG_SNAP_ATTEMPTS:-5}
pause=${AMONG_SNAP_PAUSE:-30}

case "$attempts" in
  '' | *[!0-9]*)
    echo "snap-store-install: AMONG_SNAP_ATTEMPTS must be a positive integer" >&2
    exit 2
    ;;
esac
# Arithmetic below is decimal: a leading zero would otherwise be read as octal.
attempts=$((10#$attempts))
if [ "$attempts" -lt 1 ]; then
  echo "snap-store-install: AMONG_SNAP_ATTEMPTS must be a positive integer" >&2
  exit 2
fi
case "$pause" in
  '' | *[!0-9]*)
    echo "snap-store-install: AMONG_SNAP_PAUSE must be a non-negative integer" >&2
    exit 2
    ;;
esac
pause=$((10#$pause))

usage() {
  echo "usage: $0 [--in-container NAME] <snap install arguments>" >&2
}

container=
if [ "$#" -gt 0 ] && [ "$1" = "--in-container" ]; then
  if [ "$#" -lt 2 ] || [ -z "$2" ]; then
    echo "snap-store-install: --in-container needs a non-empty container name" >&2
    usage
    exit 2
  fi
  container=$2
  shift 2
fi
if [ "$#" -eq 0 ]; then
  usage
  exit 2
fi

install=(snap install)
if [ -n "$container" ]; then
  install=(docker exec "$container" snap install)
elif [ "$(id -u)" -ne 0 ]; then
  install=(sudo snap install)
fi

for ((attempt = 1; attempt <= attempts; attempt++)); do
  echo "snap-store-install: attempt $attempt of $attempts: ${install[*]} $*" >&2
  if "${install[@]}" "$@"; then
    echo "snap-store-install: installed on attempt $attempt" >&2
    exit 0
  fi
  if [ "$attempt" -lt "$attempts" ]; then
    echo "snap-store-install: attempt $attempt failed; retrying in ${pause}s" >&2
    sleep "$pause"
  fi
done

echo "snap-store-install: snap install failed after $attempts attempts" >&2
exit 1
