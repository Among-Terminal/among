# Among tools

Independent Apache-2.0 Rust tooling. Pure tests and headless commands do not link
GTK or require a display. Use Rust 1.98.1 and the committed lockfile:

```sh
cargo fmt --check --manifest-path tools/among-tools/Cargo.toml
cargo clippy --locked --all-targets --manifest-path tools/among-tools/Cargo.toml -- -D warnings
cargo test --locked --manifest-path tools/among-tools/Cargo.toml
cargo build --release --locked --manifest-path tools/among-tools/Cargo.toml
tools/among-tools/target/release/among-tools --help
```

Run automation inside a disposable container, never against the owner's desktop
or personal configuration. The project-level `scripts/check-packages.sh` also
checks this independent third package.

## Shared storage contracts

Storage primitives require caller-owned directories. Relative names reject
absolute paths, traversal, dot/empty components and existing symlinks. Atomic
writes create a private same-directory temporary, apply the requested mode, sync
data and rename only after precommit validation. Precommit failure retains old
bytes and cleans only the temporary created by this invocation. A directory-sync
failure after rename is durability uncertainty, not evidence that old bytes remain.
Callers must report it rather than claim rollback.

The exclusive lock refuses another opener and releases on drop. SHA-256 streams
data. JSON parsing rejects duplicate keys at every depth and trailing/malformed
input; it adds no fields to existing versioned schemas. Tar primitives write sorted
regular members with zero time/uid/gid and reject unsafe, duplicate or nonregular
members on inspection. The basic tar writer uses mode 0644; executable-source mode
preservation and exact legacy archive-format parity belong to the source-snapshot
port, and are not inferred from these primitive tests. Existing immutable release
assets must never be regenerated with this writer.

## Owned processes

Process primitives use argv, clear ambient environment and accept only an explicit
environment map. `allowed_environment` selects approved names; callers then add
their synthetic HOME/XDG/display values. A successfully spawned child receives
its own process group. Bounded wait and RAII cleanup signal that owned group only,
including descendants, then reap the child. Spawn failure never triggers a
caller-wide process search or cleanup. Process output goes to caller-owned logs.
Display readiness, D-Bus and private window-manager setup are separate
concerns; this primitive alone is not evidence of graphical parity.

## Platform discovery

`among-tools platform discover` prints JSON with OS ID, version and native
architecture from `/etc/os-release` and the executable target. `platform check
OS VERSION ARCH` rejects mismatches and targets outside Debian 13 / Ubuntu 24.04,
amd64 / arm64. Discovery is not a claim that a package was accepted on that target.
The shared hosted-consent helper preserves the requirement for explicit opt-in
and both GitHub-hosted markers; markers are intent checks, not authentication.

## Display runners and ignored tests

`ci-x11 COMMAND ARGS...` creates private Xvfb/Openbox and D-Bus sessions with an
Alt+F10 maximize binding. `isolated-x11 COMMAND ARGS...` creates an owned parent
Xvfb, nested Xephyr and GNOME plus a synthetic system bus; neither personal display
nor host bus is reused. `vm-x11 COMMAND ARGS...` retains explicit KVM/QEMU-only
execution. Display-number reads handle partial lines and bounded readiness.
Evidence logs stay in private scratch directories owned by the display runner.
Commands retain their exit status; failures must not be relabeled as passes.
Injected manager exit/nonreadiness and command timeout now have executed container
tests proving that owned displays stop accepting clients and unrelated children
remain alive. A genuine disposable Ubuntu 24.04 amd64 KVM guest also executed
the Rust live-focus replay and a failed guest display command; an unrelated guest
child survived both. This guest evidence is separate from installed-package,
Wayland/GPU and full release acceptance.

Build test executables with `cargo test --locked --no-run --message-format=json`
and save stdout to a JSON-lines file. `run-ignored-tests FILE` parses those records,
rejects malformed/ambiguous input and runs every listed ignored test on its own
display. The phase-based Preferences test remains in its dedicated orchestration,
not a context-free invocation. No Cargo or compiler is called by this runner.

## Independent Linux resource measurements

`check-resource-measurements SYSTEM_STATUS_BINARY` drives the real prebuilt plugin
over its JSON-lines stdin/stdout protocol. It checks initial CPU N/A, waits 1.1
seconds, and compares CPU/MEM/root-DISK with independently read `/proc/stat`,
`/proc/meminfo` and `df -P /` accounting. Original tolerances stay at 2/2/1
percentage points, with exact meter-to-displayed-percentage consistency. It checks
response IDs and graceful shutdown, retains the original measurements JSON fields,
and fails on missing/malformed accounting or a stalled child. Requires Linux procfs,
`df` and the compiled plugin, not a display, GTK, Cargo or an interpreter at runtime.

## Fresh-process Preferences

Within `ci-x11`, run `check-preferences PREBUILT_APP_TEST PREBUILT_PLUGINS_DIR`.
The executable must be the application's Rust test binary; this is a source
regression, not an installed application acceptance substitute. Supply the directory
containing prebuilt clock, system-status and pane-pwd binaries. The runner refuses
an unrelated display before writing fixtures, creates private HOME/XDG state and
launches every original edit/restore/fault/disable/off/failure/invalid phase in a
fresh process and fresh D-Bus session. Malformed/version-invalid documents and
unrelated plugin settings retain their original assertions. Requires xdotool,
ImageMagick `import`, bash and the display prerequisites. No compiler/interpreter
is invoked. Sessions orchestration and installed Preferences acceptance are separate
pending ports; task 4.2 is not closed by implementing this half alone.
Alternatively use `check-preferences-from-cargo RECORDS PLUGINS_DIR`: it enumerates
prebuilt Cargo JSON artifacts, requires exactly one executable listing the precise
phase-driven test, and refuses a failed/missing/ambiguous listing. It does not
glob a stale test binary from an old build or invoke Cargo itself.

## Standalone removal (migration in progress)

The separately built `among-uninstall` reads version-1 ownership manifests under
XDG_STATE_HOME and validates fixed HOME/XDG_DATA_HOME destinations plus safe
THIRD_PARTY paths. It refuses altered hashes, unknown destinations, symlinks,
unsupported versions and a concurrent operation before removing files. It needs
neither the source checkout, Cargo nor an interpreter, and preserves preferences,
sessions, external plugins and shell startup files. Do not copy it manually over
a launcher written by the former installer: let the installer below replace it,
because only the installer checks that launcher's recorded hash first.
`install-prebuilt SOURCE_ROOT AMONG_BINARY NATIVE_UNINSTALLER` now installs the
explicit prebuilt payload, including desktop/icon/metadata/license files and the
complete regular-file THIRD_PARTY tree. It upgrades only verified owned files,
removes the obsolete Python helper, replaces the launcher with the native binary,
and commits manifest v1 after payload writes. `install SOURCE_ROOT` preserves the
public `--binary`, `--install-deps`, `--uninstall` options. Source builds validate
native pkg-config dependencies, compile into private staging as the normal user,
and never use sudo for Cargo. Only explicit `--install-deps` invokes sudo apt-get.
Available desktop/icon cache tools are refreshed; cache errors remain warnings
with retained logs. On failure it attempts rollback of
bytes, modes and the prior manifest and reports any rollback failures. This is
not crash-atomic across files; concurrent hostile filesystem mutation is not
covered by the current path checks. `scripts/install.sh` uses prebuilt native
tools or a locked headless Cargo bootstrap and then dispatches to Rust. Full
source/bootstrap replay and legacy test parity are tracked in migration evidence;
an installed fixture is not DEB/Snap release acceptance.

`check-plugin-install SOURCE_ROOT AMONG_BINARY PREBUILT_PLUGINS PLUGIN_INSTALLER`
must run inside an owned display. Build the existing Rust plugin installer example
first (`cargo build --release --locked --example install --manifest-path
examples/plugins/Cargo.toml`). The checker requires exactly the approved three
plugin sources and installed native bytes, verifies default-disabled discovery and
runtime-only enabling, and exercises the selected application without compiling
at runtime. Plugin input/output logs and fresh HOME/XDG state stay isolated.

## Debian input and staging primitives

`deb-release-inputs SOURCE_ROOT TAG TARGET ARCH` checks strict stable tags against
both Cargo manifests and AppStream, and emits the original five-field identity
schema. Supported targets are debian13/ubuntu24.04 × amd64/arm64.
`stage-deb SOURCE_ROOT BINARIES NEW_DESTINATION` validates the complete app/plugin/
metadata/license inventory before creating a new system-payload tree, preserves
attribution bytes and exact file modes, rewrites only the desktop executable path,
and never invokes the per-user installer or touches HOME/XDG. Existing output is
refused. Individual writes are atomic; multi-file crash atomicity is not implied.
`deb-tag-source SOURCE_ROOT` refuses anything except a GitHub release-tag push and
checks exact checkout/tag/event source plus origin/main ancestry using local Git.
These primitives and their negative fixtures do not constitute package building,
installed acceptance or completed release workflow migration. No supported local
DEB construction command is introduced.

`check-deb-source-inputs SOURCE_ROOT` observes pinned/locked app and plugin build
commands, maintainer/root policy, absence of the per-user installer and Cargo
installation-guide install/uninstall identity. `check-deb-selection SOURCE_ROOT BINARIES`
copies the Debian recipe into a private evidence directory, stages the complete
payload, runs real `dh_install`, `dh_missing --fail-missing` and desktop validation,
and compares every selected payload path, digest and permission with staging.
It calls no build rule, produces no .deb and installs nothing. Use the pinned
Ubuntu container recipe `containers/Dockerfile.deb-selection` with readonly
project/binary mounts and only the checker's scratch workspace writable. This fixture
image is not the clean installed acceptance image or a release build image.
Candidate digest/source/inventory, runtime package/tool policy and user-state hash
validators have negative Rust tests, but are not yet wired into a migrated
installed runner; the legacy release workflow remains until that work is exercised.

## Native focus, status and desktop checks

`ci-x11 among-tools check-context-menu-launch AMONG_BINARY` installs only private
http/https/mailto desktop associations, replaces XDG_DATA_DIRS to exclude host
fallbacks and records exact URI argument arrays with the native Rust receipt
helper. It exercises supported/unsupported OSC8 and missing-handler failure with
actual menu clicks and continued terminal input. Menu actions wait for two complete
equal samples from the current invocation, never historical global bounds. This
URI checker is observed. Full source clipboard/TUI/light/dark menu coverage uses
the selected application and prebuilt GTK clipboard fixture, built separately so
the headless tools package stays independent of GTK:

```sh
cargo build --release --locked --example check-clipboard
tools/among-tools/target/release/among-tools ci-x11 \
  tools/among-tools/target/release/among-tools check-context-menu \
  target/release/among target/release/examples/check-clipboard light
```

Repeat with `dark`. The native raw PTY command runs inside the selected app's real
shell and captures exact bytes. Existing menu actions, clipboard/selection,
mouse-reporting, OSC8 pointer identity, native pane/zoom/focus behavior, cancel and
asynchronous layout/workspace/exit revocation are checked, not merely logged.
`check-context-menu-snap CLIPBOARD_HELPER THEME [--github-hosted]` preserves the
original guest or explicit GitHub-hosted approval gate and verifies live process
maps contain Snap GTK/VTE. Source light/dark runs and both native installed-Snap
architecture replays passed all 14 scenario groups. This is devel-grade X11
evidence, not stable publication, Wayland or complete source/license acceptance.
The owned display runner forwards only PATH and the two
nonsecret GitHub hosted markers needed for the explicit gate, not credentials.

After building the app, tools and three example plugins, run each command through
`among-tools ci-x11` on a disposable desktop:

```sh
among-tools ci-x11 among-tools check-live-focus /absolute/path/to/among
among-tools ci-x11 among-tools check-live-status /absolute/source /absolute/path/to/among /absolute/observed-installed-profile
among-tools ci-x11 among-tools check-desktop /absolute/source /absolute/path/to/among
```

Focus and status use exactly the selected executable; they do not substitute a
checkout build when an installed executable was selected. Status uses the actual
documented OSC7 shell hook and a profile captured after actual native public
installation, review, mapped output, disable and uninstall (see `docs/testing.md`).
It validates native architecture, receipts, grants and exact executable bytes before
replay; a directory of loose example binaries is not a profile. Desktop checks
validate metadata, invoke the documented installer with the selected binary,
launch its generated entry via GIO on a private bus and check title, WM class,
GTK icon lookup path, then remove the fixture through its native uninstaller.
Its `desktop-session` helper keeps the outer owned bus/group alive after GIO exits;
it is not an independently supported application launch entry point.
SVG lookup requires a working gdk-pixbuf SVG loader (`librsvg2-common` on Debian/
Ubuntu), not merely the SVG file or GTK's `has_icon` result. Metadata validators,
xdotool, xprop, pgrep and ImageMagick are required. These source/source-installer
checks do not replace artifact-bound DEB/Snap checks in release CI.

## Session persistence and failure scenarios

`ci-x11 among-tools check-sessions AMONG_BINARY` exercises the selected native
application, not an internal model substitute. It retains all original pending
consent/close, singleton, sparse pane ID, native layout ratio, pane drag, workspace
rename, background cwd checkpoint, cancel/exit, unchanged snapshot, crash/fresh
shell, Escape/Start new, missing/inaccessible cwd, permission-loss/recovery and
malformed/future-version cases. Real terminal commands report restored cwd;
native buttons and keyboard events drive mutations. Non-root execution, private
X11/D-Bus, xdotool, pgrep, bash, xprop and ImageMagick are required. The checker
refuses an unowned display; failure diagnostics and screenshots stay in its
owned evidence directory. Permission fixtures are restored even on failure.
Passing against a source executable does not close release-CI installed-package
gates. Pair this with `check-preferences-from-cargo` for all ten Preferences phases.

## Terminal scrolling

`ci-x11 among-tools check-scroll-calibration AMONG_BINARY` drives the selected
native application and proves how each kind of scroll moves a terminal. Touch
surface scrolling cannot be synthesised — XTEST sends wheel clicks — so it is
driven through the `scroll:` script step, which reaches the same handler a real
gesture does, and the check requires each delta to move the history by exactly
the distance it reports converted to rows. Wheel clicks are sent as real clicks
and must keep moving the history by the terminal library's own amount, which is a
tenth of the pane's rows rounded up; a modified wheel click must reach the
library too. A delta with no history to move through must be handed back rather
than consumed, each pane's position must be its own, and a program drawing a
full screen must receive the gesture — the check runs one that records the bytes
it is sent and reads them back. Requirements match `check-sessions`: non-root
execution, an owned X11/D-Bus display, xdotool, xprop and ImageMagick.

## Split-inherited terminal directory

`ci-x11 among-tools check-split-directory AMONG_BINARY` drives the selected native
application and proves where a new terminal actually starts. It changes directory in
a pane, makes that pane publish its directory, splits it, and then reads the
self-check report and each shell's own `pwd`: the new terminal must be in the
directory of the pane it was split from, the pane that was split must not move, and
an available directory must not be reported as unavailable. Removing that directory
before a further split must produce the fallback directory and a report naming the
requested location. Requirements match `check-sessions`: non-root execution, an
owned X11/D-Bus display, xdotool, xprop and ImageMagick; evidence stays in its own
directory.

## Quiet window controls and sidebar width

Build the Rust subprocess fixture with
`cargo build --release --locked --manifest-path tools/among-tools/Cargo.toml --example notice-plugin`,
then run `ci-x11 among-tools check-window-controls AMONG_BINARY NOTICE_PLUGIN_BINARY`.
The check sends real pointer drags to the sidebar edge, checks its resize cursor,
reads the saved width, and starts a fresh process against that configuration to
prove the width controls the displayed sidebar. It covers the fixed collapsed
strip, expansion, small-window clamping without rewriting the preferred width,
write failure and recovery, invalid documents, and usable terminal input without
restarting the shell or changing its pane layout. A real notification-producing
plugin proves its message is logged while its explicit footer remains visible.
Live probes must report zero toast widgets, including after zoom and process exit.
Existing Preferences and URI-launch checks cover their error paths. Requirements
match `check-sessions`; the fixture is prebuilt, so the runtime needs no compiler.

## Workspace renaming

`ci-x11 among-tools check-sidebar-rename AMONG_BINARY` drives the selected native
application and proves how a workspace's name is edited. Clicks are aimed at the
bounds the application publishes for each name, and the pointer's real position is
read back, so a click that landed elsewhere cannot be read as an application that
ignores clicks. A single click must switch workspaces and open no field; a
double-click must open a field in the place the name was, holding that name and
holding the keyboard — established by deleting the last character and confirming,
which must leave exactly the name minus that character. Enter applies, Escape keeps,
and clicking elsewhere applies; an empty or whitespace-only name is refused on each
of those endings, and the row shows the name the workspace still holds. Starting a
rename on another row must end the first one, leave exactly one field, and leave the
active marker on the workspace being renamed. A rename must change nothing else in
its workspace — panes, directories, layout and plugin contributions, compared as a
fingerprint taken on both sides of it — and the terminal must hold the keyboard
afterwards, which is shown by typing a command without clicking and reading the file
it wrote. Requirements match `check-sessions`.

## Source and native provenance

`package-native-checker BINARY SOURCE_SHA TAG TARGET ARCH NEW_DIRECTORY` copies a
regular target-native ELF into an exclusive internal directory and writes its
schema-1 identity/digest record. `check-native-checker DIRECTORY SOURCE_SHA TAG
TARGET ARCH` refuses changed source/tag/target/architecture, bytes, mode or extra
members. Both use the existing native-checker format; the directory's executable
is named `among-tools`, including when the input is a separate native acceptance
helper. Keep each helper in its own directory and select it by its recorded role
in the calling workflow. Never add these directories to the application payload.

Validate artifact transport/hashes before executing the transferred validator:
the checker cannot independently establish trust in its own initial binary.
GitHub's trusted job-to-job artifact transport is not an independent signature.
Target build and clean installed acceptance must be separate environments. The
private Snap rehearsal is testing this separation; a workflow definition or a
passing source test does not establish clean installed acceptance.

`source-snapshot ROOT VENDOR NEW_ARCHIVE` includes all three locked Cargo graphs;
vendor with both `--sync examples/plugins/Cargo.toml` and
`--sync tools/among-tools/Cargo.toml`. `check-source-snapshot ARCHIVE` streams the
safe-member inventory and mandatory original-source/vendor checks. Rebuilding
all three packages offline is a separate requirement, not implied by SHA-256.

`inventory-snap-native PRIME EXACT_DEBS NEW_OUTPUT` reads the actual .deb inputs
with dpkg-deb rather than host-installed package metadata. It compares observed
prime files, retains authentic notices and records common-license references.
`check-snap-notices NATIVE_JSON PRIME` requires those notice hashes and nonempty
contained common-license texts. Missing license references are failures, not
permission to invent text or omit the reference.

`check-snap-sources NATIVE_JSON SOURCES` checks exact source versions, inventory
hashes and descriptor archive checksums. `check-native-source-unpack SOURCES
NEW_OUTPUT` requires a disposable container and exercises dpkg-source extraction
with packaging patches; it records build-input hashes, not full native rebuilds.

`check-apt-provenance METADATA NATIVE_JSON SOURCES` invokes gpgv against the pinned
Ubuntu archive signer. `collect-apt-provenance NEW_OUTPUT` preserves metadata in
a fresh root Ubuntu 24.04 audit container and changes only that disposable APT
configuration. Collection is not cryptographic authentication; use the checker.
The checker verifies repository identity/time and both index/input
digest levels, then applies the exact source checker. GNU date, gpgv and preserved
authenticated inputs are runtime prerequisites for this audit command. The
ordinary tooling suite includes a real unsigned-input gpgv refusal and real
synthetic dpkg-source extraction, so these external tools must also be present
in the test container. They are not Among application runtime dependencies.

`package-source-delivery SNAP SNAPSHOT NATIVE SOURCES METADATA PRIME BUILDER
INSTRUCTIONS NEW_OUTPUT` authenticates the inputs and compares actual unsquashfs
binary/plugin/metadata bytes with prime before streaming a deterministic source
bundle. It binds the preserved snapshot receipt, rejects unsafe inputs and checks
its resulting association. Positive end-to-end packaging of a complete audited
Snap remains pending. See [source delivery](../../docs/source-delivery.md) for
scope and prerequisites; no command here establishes legal sign-off or publication.

## Stable promotion admission (partial port)

`check-promotion-admission RECORD_JSON GATES_JSON ARTIFACT_DIRECTORY --approved`
checks exact source/artifact binding, all six gates for both architecture receipts,
stable version/roles, and actual local artifact hashes. Missing approval refuses
before input reads; malformed or extra assets and symlinks fail. Output explicitly
states `published: false`: this command does not reconcile GitHub/Store or
prove that protected environment rules are configured. Offline tests use clearly
synthetic assets and gates, not real approval or compliance evidence. The
publishing command is `release-promotion`; see
[releasing](../../docs/releasing.md) for what it does and which of its paths are
tested. No workflow schedules nightly publication.

## Installed Snap host workflow (partial runtime port)

Within an owned `ci-x11` or disposable guest display, run `check-snap-runtime
[--github-hosted]`. The hosted mode requires explicit approval and both original
GitHub markers. It observes installed executable/maps, icon decoding, an actual
bash startup file, host working directory/environment/library isolation, a Rust
host tool and external JSON-lines plugin fixture, then native fullscreen/split/
keyboard resize/zoom/pointer-divider controls. The fixtures execute the separately
built tools binary outside the Snap payload; they are not shipped sample plugins.

`snap-host-probe OWNED_OUTPUT` requires the synthetic host marker and owned private
evidence directory. `snap-external-plugin` is the fixture protocol helper; it
returns scope-specific messages driven by stdin and exits on shutdown. Neither
helper is a supported product feature or a substitute for observing the plugin
inside the actual application. Positive installed execution of the new runtime
driver is pending. The original modified-VTE/GDB branch remains unported;
`--replace-vte` is rejected, not accepted as a no-op. Retain that legacy audit
until real library replacement and breakpoint evidence exist in Rust.

## Deferred nightly selection

`select-nightly REPOSITORY_DIR OWNER/REPO NEW_JSON [--now ISO]` resolves main once
(`--now` must carry `Z` or a `±HH:MM` offset; the version uses its UTC date),
reads GitHub release records through gh, skips only a matching complete marker
and preserves prepared retry versions. It refuses endpoint errors and never
uploads, publishes or changes Store channels. The CLI uses exclusive output and
private credential-scoped API scratch. Offline tests mock gh and disposable Git;
no nightly schedule or rolling publication is activated.
