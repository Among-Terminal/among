#!/bin/bash
# Installs packages with apt-get while tolerating a temporarily unreachable
# mirror. GitHub-hosted Ubuntu images fetch packages through the mirror named
# in /etc/apt/apt-mirrors.txt; when that host stops resolving, one attempt
# waits on every index and package until the job timeout cancels it. This
# script caps each connection, repeats the whole update/install pair, and moves
# the image to the public archive for its architecture only when an update
# fails and the mirror does not answer a probe (or curl is unavailable, so no
# probe can be made, or the mirror list has no parsable entry to probe). A
# failed install alone never changes the mirror. The last attempt still fails
# the step: packages are never skipped.
#
# Usage: bash scripts/ci/apt-install.sh [apt-get install arguments]
#
# Environment:
#   AMONG_APT_ATTEMPTS  update/install pairs to try (default 4)
#   AMONG_APT_PAUSE     seconds between attempts (default 15)
set -euo pipefail

mirror_list=/etc/apt/apt-mirrors.txt
attempts=${AMONG_APT_ATTEMPTS:-4}
pause=${AMONG_APT_PAUSE:-15}

case "$attempts" in
  '' | *[!0-9]*)
    echo "apt-install: AMONG_APT_ATTEMPTS must be a positive integer" >&2
    exit 2
    ;;
esac
# Arithmetic below is decimal: a leading zero would otherwise be read as octal.
attempts=$((10#$attempts))
if [ "$attempts" -lt 1 ]; then
  echo "apt-install: AMONG_APT_ATTEMPTS must be a positive integer" >&2
  exit 2
fi
case "$pause" in
  '' | *[!0-9]*)
    echo "apt-install: AMONG_APT_PAUSE must be a non-negative integer" >&2
    exit 2
    ;;
esac
pause=$((10#$pause))
if [ "$#" -eq 0 ]; then
  echo "usage: $0 <apt-get install arguments>" >&2
  exit 2
fi

# apt-get and the mirror file need root; the callers run as the runner user.
apt_get=(apt-get)
tee_mirror=(tee)
if [ "$(id -u)" -ne 0 ]; then
  apt_get=(sudo apt-get)
  tee_mirror=(sudo tee)
fi

# A failed index fetch must fail the attempt instead of silently leaving the
# old indexes in place, and no single connection may hold the job for minutes.
options=(
  -o Acquire::Retries=2
  -o Acquire::http::Timeout=15
  -o Acquire::https::Timeout=15
  -o Acquire::ftp::Timeout=15
  -o APT::Update::Error-Mode=any
)

for ((attempt = 1; attempt <= attempts; attempt++)); do
  echo "apt-install: attempt $attempt of $attempts: apt-get update" >&2
  update_failed=0
  if "${apt_get[@]}" "${options[@]}" update; then
    echo "apt-install: attempt $attempt of $attempts: apt-get install $*" >&2
    if "${apt_get[@]}" "${options[@]}" install "$@"; then
      echo "apt-install: packages installed on attempt $attempt" >&2
      exit 0
    fi
  else
    update_failed=1
  fi
  if [ "$attempt" -lt "$attempts" ]; then
    if [ "$update_failed" -eq 0 ]; then
      echo "apt-install: apt-get install failed; keeping the configured mirror for the next attempt" >&2
    # The Ubuntu 24.04 image points its sources at mirror+file; only that
    # indirection may be rewritten here, and only after a failed update whose
    # mirror did not answer a probe.
    elif [ -r "$mirror_list" ] &&
      grep -rqsF -- "mirror+file:$mirror_list" /etc/apt/sources.list /etc/apt/sources.list.d 2>/dev/null; then
      mirror=$(sed -n '/^[[:space:]]*[^#[:space:]]/{s/^[[:space:]]*//;s/[[:space:]].*//;p;q;}' "$mirror_list" || true)
      case "$(uname -m)" in
        x86_64 | amd64) fallback=http://archive.ubuntu.com/ubuntu/ ;;
        # arm64 packages come from a separate archive, not from the amd64 one.
        aarch64 | arm64) fallback=http://ports.ubuntu.com/ubuntu-ports/ ;;
        *) fallback= ;;
      esac
      if [ -z "$fallback" ]; then
        echo "apt-install: no public archive is known for $(uname -m)" >&2
      elif [ "$mirror" = "$fallback" ]; then
        echo "apt-install: mirror list already names $fallback" >&2
      else
        probe=unavailable
        if [ -n "$mirror" ]; then
          if ! command -v curl >/dev/null 2>&1; then
            probe=missing
          elif curl --fail --head --silent --show-error --location --max-time 15 "$mirror/" >/dev/null 2>&1; then
            probe=answers
          else
            probe=silent
          fi
        fi
        if [ "$probe" = answers ]; then
          echo "apt-install: apt-get update failed but $mirror answers, so the failed fetch came from elsewhere; keeping the mirror" >&2
        else
          if [ "$probe" = missing ]; then
            echo "apt-install: apt-get update failed; cannot probe $mirror because curl is missing, so the mirror may be unreachable; replacing it with $fallback" >&2
          elif [ -n "$mirror" ]; then
            echo "apt-install: apt-get update failed and $mirror does not answer; replacing it with $fallback" >&2
          else
            echo "apt-install: apt-get update failed and $mirror_list names no mirror; replacing it with $fallback" >&2
          fi
          printf '%s\n' "$fallback" | "${tee_mirror[@]}" "$mirror_list" >/dev/null
        fi
      fi
    fi
    echo "apt-install: attempt $attempt failed; retrying in ${pause}s" >&2
    sleep "$pause"
  fi
done

echo "apt-install: apt-get update and install failed after $attempts attempts" >&2
exit 1
