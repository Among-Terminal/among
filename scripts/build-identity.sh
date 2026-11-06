#!/bin/sh
# Prints the build identity of the checked-out commit as NAME=VALUE lines, the
# form `$GITHUB_ENV` and `env` take. A package build exports these before it
# compiles so the About page can show them; the application reads them with
# `option_env!` and needs neither git nor a build script itself.
#
#   sh scripts/build-identity.sh <ref-type> <ref-name>
#
# AMONG_BUILD_COMMIT  the first seven characters of the commit
# AMONG_BUILD_DATE    the commit's UTC date, YYYY-MM-DD
# AMONG_BUILD_VERSION the tag without its `v` (0.2.0-rc.2); printed only when
#                     <ref-type> is `tag`, otherwise the crate version is shown
#
# Every value comes from the commit, never from the build machine's clock or
# checkout, so two builds of one tag produce the same executable.
set -eu

if [ "$#" -ne 2 ]; then
  echo "usage: build-identity.sh <ref-type> <ref-name>" >&2
  exit 2
fi
ref_type=$1
ref_name=$2
root=$(cd "$(dirname "$0")/.." && pwd -P)

# A container checkout can belong to another user; trust only this directory.
git_here() {
  git -c "safe.directory=$root" -C "$root" "$@"
}

commit=$(git_here rev-parse --verify HEAD)
case "$commit" in
  *[!0-9a-f]*) echo "HEAD is not a hexadecimal commit: $commit" >&2; exit 1 ;;
esac
if [ "${#commit}" -ne 40 ]; then
  echo "HEAD is not a full commit: $commit" >&2
  exit 1
fi
epoch=$(git_here show -s --format=%ct HEAD)
case "$epoch" in
  ''|*[!0-9]*) echo "invalid commit timestamp: $epoch" >&2; exit 1 ;;
esac

if [ "$ref_type" = tag ]; then
  case "$ref_name" in
    v[0-9]*.[0-9]*.[0-9]*) ;;
    *) echo "not a release tag: $ref_name" >&2; exit 1 ;;
  esac
  case "${ref_name#v}" in
    *[!0-9A-Za-z.-]*) echo "not a release tag: $ref_name" >&2; exit 1 ;;
  esac
  printf 'AMONG_BUILD_VERSION=%s\n' "${ref_name#v}"
fi
printf 'AMONG_BUILD_COMMIT=%.7s\n' "$commit"
printf 'AMONG_BUILD_DATE=%s\n' "$(date -u -d "@$epoch" +%F)"
