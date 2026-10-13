#!/bin/sh
set -eu
# The workflow bind-mounts the retained evidence directory here and passes the
# same path to the checkers; refusing to start without it keeps the evidence
# from landing anywhere the upload does not look.
checks="${AMONG_CHECK_ROOT:?AMONG_CHECK_ROOT must name the mounted evidence directory}"
test -d "$checks"
cd /input
test "$(dpkg --print-architecture)" = "$EXPECTED_ARCH"
for executable in cargo rustc rustup cc gcc g++ clang; do
    if command -v "$executable" >/dev/null 2>&1; then
        echo "Unexpected runtime compiler: $executable" >&2
        exit 1
    fi
done
dpkg-query -W -f='${binary:Package} ${db:Status-Status}\n' > "$checks/runtime-packages.txt"
# Ubuntu systemd depends on systemd-dev's D-Bus XML/pkg-config metadata, not
# libsystemd-dev headers. Retain its file inventory instead of assuming its name.
dpkg-query -L systemd-dev > "$checks/systemd-metadata-files.txt"
if grep '/include/' "$checks/systemd-metadata-files.txt"; then
    echo 'Unexpected headers in systemd metadata package' >&2
    exit 1
fi
if grep -v '^systemd-dev installed$' "$checks/runtime-packages.txt" | grep -E '^(.*-dev(:[^ ]+)?|cargo|rustc) installed$'; then
    echo 'Development headers or Rust toolchain in acceptance environment' >&2
    exit 1
fi
test ! -e /input/Cargo.toml
sha256sum -c SHA256SUMS
tool=/input/tools/among-tools
"$tool" check-native-checker /input/tools "$EXPECTED_SOURCE" "$EXPECTED_TAG" ubuntu24.04 "$EXPECTED_ARCH"
"$tool" check-native-checker /input/clipboard "$EXPECTED_SOURCE" "$EXPECTED_TAG" ubuntu24.04 "$EXPECTED_ARCH"
cp SHA256SUMS "$checks/transferred-input-hashes.txt"
cp tools/checker-record.json "$checks/tool-record.json"
cp clipboard/checker-record.json "$checks/clipboard-record.json"
if "$tool" check-native-checker /input/tools 0000000000000000000000000000000000000000 "$EXPECTED_TAG" ubuntu24.04 "$EXPECTED_ARCH" > "$checks/wrong-source.log" 2>&1; then
    echo 'Mismatched checker source was accepted' >&2
    exit 1
fi
mkdir -m 700 "$checks/tampered-checker"
cp tools/among-tools tools/checker-record.json "$checks/tampered-checker/"
chmod u+w "$checks/tampered-checker/among-tools"
printf X >> "$checks/tampered-checker/among-tools"
if "$tool" check-native-checker "$checks/tampered-checker" "$EXPECTED_SOURCE" "$EXPECTED_TAG" ubuntu24.04 "$EXPECTED_ARCH" > "$checks/altered-digest.log" 2>&1; then
    echo 'Altered checker digest was accepted' >&2
    exit 1
fi
clipboard=/input/clipboard/among-tools
test "$(unsquashfs -cat /input/among.snap meta/snap.yaml | sed -n 's/^version: //p')" = "${EXPECTED_TAG#v}"
test -x /snap/among/current/bin/among
if unsquashfs -ll /input/among.snap | grep -E '/(among-tools|among-uninstall)$'; then
    echo 'Internal checker leaked into user-facing payload' >&2
    exit 1
fi
status=0
"$tool" ci-x11 "$tool" check-snap-runtime --github-hosted || status=1
"$tool" ci-x11 "$tool" check-context-menu-snap "$clipboard" light --github-hosted || status=1
"$tool" ci-x11 "$tool" check-context-menu-snap "$clipboard" dark --github-hosted || status=1
"$tool" ci-x11 "$tool" check-live-focus /snap/bin/among || status=1
"$tool" ci-x11 "$tool" check-live-status /input /snap/bin/among /snap/among/current/usr/share/among/plugins || status=1
"$tool" ci-x11 "$tool" check-sessions /snap/bin/among || status=1
exit "$status"
