# Debian packaging development

Every release tag builds DEBs for Debian 13 and Ubuntu 24.04 on amd64 and
arm64, and the [latest release](https://github.com/Among-Terminal/among/releases/latest)
carries them. The installed checks described below are a scoped subset, not
full release acceptance. Normal user installation is documented on the
[installation page](https://among-terminal.github.io/install/).

## GitHub release builds only

`.github/workflows/deb-release.yml` builds candidates exclusively for `v*` tag
pushes, accepting stable `vX.Y.Z` and preview `vX.Y.Z-rc.N` tags (positive `N`).
Its only trigger is `workflow_call`: `release-draft.yml` starts it for the tag
push, and the called run sees that push event. The base version must match
Cargo/AppStream versions, checked before package construction. It has no
branch, pull-request, schedule or manual dispatch build trigger; GitHub reruns
retain the original tag event. Native
`ubuntu-24.04` and `ubuntu-24.04-arm` runners use digest-pinned Debian 13 or Ubuntu
24.04 containers, a pinned Rust 1.98.1 toolchain and debhelper dependency detection.
Container identities, installed build-package versions, source commit, package
version, architecture and SHA-256 are recorded with each Actions artifact.

The builder refuses ordinary local execution. Local work uses the fixture tests
below; do not register host QEMU/binfmt or build release packages locally.
Actions artifacts remain internal candidates, not public download attachments.
This build workflow has no publication permission and does not itself publish.
`release-draft.yml` attaches the checked DEBs and binary archives, under
version-free names, to an unpublished release draft
([releasing guide, section 5](releasing.md#5-release-drafts-from-a-tag)); the
owner publishes the draft. The version inside each DEB keeps the Debian
`~rc.N` ordering.

## Installed checks in release CI

The separate `installed` job downloads the exact target artifact into a fresh
container on a native runner. `among-tools ci-deb-install` checks tag/source/target/digest
binding before apt installation and rejects compilers or development packages
both before and after it. A control-only negative dependency fixture must fail
APT simulation. The real package must be fully configured and have no unresolved
libraries according to `ldd`.

A synthetic unprivileged user runs a private Xvfb/Openbox/D-Bus session with only
`/usr/bin:/bin` in PATH. `among-tools check-deb-desktop` launches the installed system entry
through GIO, observes a visible `/usr/bin/among` window and its application ID,
resolves the icon through GTK, and types a shell command that writes an actual
marker file. The existing live-focus regression then runs against `/usr/bin/among`
using its explicit executable argument, without building anything. The headless
native checker is transferred separately from the package. SHA-256 is checked
before execution; its receipt must then match the tag, source, OS target and
architecture, and orchestration refuses a different executable. The receipt is
an internal artifact-integrity contract, not a cryptographic signature or a
substitute for trusting the build job and artifact transport.

Logs and `runtime-record.json` identify the package SHA-256, source, target and
executed checks. The record explicitly says `release_acceptance: false`: this
initial subset is not the full acceptance gate. The live-status regression now
copies the installed example executables into a synthetic external-plugin
directory and explicitly enables them. The session regression uses the installed
binary to exercise consent, restored directories and divider ratios, a divider
drag, pane moves and background activity, without Cargo. Private hosted execution passed these scoped
GUI/focus/status/Sessions checks on all four native targets. Full package and
release acceptance remains incomplete.

The Rust lifecycle runner runs only in the disposable release-CI container. It repacks the
candidate as a synthetic older-version fixture (`~ciolder`, not a historical
release), adds an older-only file, and upgrades back to the exact candidate.
It verifies version ordering, installed candidate bytes and removal of the older
file, then runs remove and purge. Because the local-only package has no conffiles,
remove can erase its APT identity completely. The runner reinstalls the same checked
candidate, verifies its exact version and payload again, then purges the installed
package independently; it does not treat an absent-package no-op as purge evidence.

It then checks the upgrade users actually perform. Before any installation, the
runner downloads the published package of the same target from the release pinned
in `packaging/ci/deb-upgrade/previous-release.json` and refuses it unless its
SHA-256, package name, version and architecture equal the pin and its version sorts
before the candidate. After the purge above it installs that published package on
the clean system, upgrades it to the candidate, verifies the candidate version and
bytes and that no file owned only by the published package remains, then purges.
Synthetic preferences, sessions, external
plugin, shell startup files and per-user Cargo/source launcher fixtures must keep
their hashes throughout. A separate user process must survive; this does not yet
prove survival of a running GUI terminal through package operations.

Finally the runner replays the user commands of "Installing a built package"
below word for word, without `sudo` because it already runs as root: install
the published package, upgrade to the candidate, roll back with
`--allow-downgrades`, upgrade again, remove and purge, each from a directory
holding this target's package under its release name, with the installed
version checked after every step. apt's confirmation prompt is answered through
`APT_CONFIG`, as a user answers it, so the documented words stay unchanged. The
commands are read from this file; an unknown or changed command stops the run
before anything is installed.

The installed binary also runs the keyboard divider regression
(`check-live-resize`, `Ctrl+Shift`+arrows). Actual Snap coexistence and
installed preference editing remain outstanding: the existing Preferences
check drives a Cargo test executable, not `/usr/bin/among`, and the
Preferences window is being reworked for 0.2.0. Private hosted acceptance passed the independent
upgrade/remove/reinstall/purge sequence on all four targets, with user-state hashes
and the synthetic survivor asserted after each operation. Receipts retain partial
runtime/lifecycle scope, not full release acceptance. X11/Openbox evidence does not
certify GNOME or Wayland.

The supported native runner labels were checked against GitHub's
[hosted-runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).
Runner availability for the actual organization remains a hosted-execution gate.

The Debian inputs live under `packaging/debian/`. The CI builder copies them into
`debian/` inside a fresh isolated public source build tree and makes its rules
executable; it never merges them into an existing package tree. The build environment
requires debhelper 13, pkg-config, GTK4/adwaita/GTK4 VTE development
packages and Rust 1.98.1 available through rustup. The rules build and test all
three independent locked Cargo packages and use a dedicated `target/debian`
directory. Rust staging replaces the direct interpreter build dependency.

The system package installs `/usr/bin/among`, the standard desktop/icon/AppStream
assets with ID `com.github.among-terminal`, and license materials under
`/usr/share/doc/among-terminal/`. Three example binaries are staged under
`/usr/share/among/plugins/`; staging does not enable them or edit user files.
Maintainer contact: Among Terminal <hello+amongterminal@highload.zone>.

## Software-centre presentation

The package supplies its desktop launcher, square SVG and AppStream metainfo,
including the project licence, release details and a public application screenshot.
Those inputs do not guarantee a rich card when a standalone DEB is opened:
GNOME Software 48.3's PackageKit local-file path begins with package details and
desktop-file discovery, not extraction of the DEB's metainfo. Its later AppStream
refinement and the installed-app view are separate consumers to test.

XML/desktop validation therefore does not certify the displayed name, icon,
licence or screenshots in either view. The corrected candidate's actual
pre-install and installed GNOME Software views remain acceptance gates. No Among
APT repository or signing key is installed by these presentation changes; Debian
and Ubuntu repository/catalogue publication is separate future work. Local-file
trust and update-source notices are not suppressed.

## Staging checks

```sh
cargo test --locked --manifest-path tools/among-tools/Cargo.toml --test deb --test deb_workflow --test native_checker
tools/among-tools/target/release/among-tools check-deb-source-inputs .
tools/among-tools/target/release/among-tools check-deb-workflow .
```

This checks staging contents, permissions, unchanged license inputs, absolute
desktop launch paths, unsafe/missing input rejection and absence of HOME writes.
Unit tests use executable fixtures, not evidence of packaged application behavior.

## Installing a built package

The four supported targets are named exactly as the release job names them.
Each package carries its own distribution and architecture in the filename, so
the right one is chosen by reading the name, never by guessing. A GitHub Release
attaches each package under a version-free name, so the same name in
`releases/latest/download/` always fetches the newest stable package; the build
job writes the Debian file name, which the package metadata repeats:

| Target | Release file | Build output |
|---|---|---|
| `debian13` / `amd64` | `among-terminal-debian13-amd64.deb` | `among-terminal_<version>-1~debian13_amd64.deb` |
| `debian13` / `arm64` | `among-terminal-debian13-arm64.deb` | `among-terminal_<version>-1~debian13_arm64.deb` |
| `ubuntu24.04` / `amd64` | `among-terminal-ubuntu24.04-amd64.deb` | `among-terminal_<version>-1~ubuntu24.04_amd64.deb` |
| `ubuntu24.04` / `arm64` | `among-terminal-ubuntu24.04-arm64.deb` | `among-terminal_<version>-1~ubuntu24.04_arm64.deb` |

`dpkg-deb --field among-terminal-debian13-amd64.deb Version` prints the version
of a downloaded file. For `v0.1.0-rc.1`, `<version>` is `0.1.0~rc.1`. Debian version ordering puts
successive RCs before `0.1.0`; stable promotion still refuses all RC tags.
RC tags are cut from `main` ancestry and are previews of a version not yet
published as stable. Do not replace an installed stable version with an RC of
the same base: APT correctly treats that operation as a downgrade. The embedded
AppStream version remains the base application version; the package version and
release label distinguish the RC.

Install the package built for your own distribution. A `debian13` package is not
a substitute on Ubuntu 24.04 or the reverse: the dependency versions differ.

```sh
sudo apt install ./among-terminal-debian13-amd64.deb
```

`apt install ./file.deb` resolves and installs dependencies from the configured
repositories. There is no APT repository for Among Terminal, so `apt upgrade`
will never move to a new version by itself; a release is upgraded by installing
the newer local package.

### Upgrade, remove, purge and rollback

```sh
sudo apt install ./among-terminal-debian13-amd64.deb   # upgrade with a newer release's file
sudo apt remove among-terminal                         # remove
sudo apt purge among-terminal                          # purge
```

Rollback is installing an older release's package over a newer one. `apt`
refuses a downgrade by default; the explicit form is:

```sh
sudo apt install --allow-downgrades ./among-terminal-debian13-amd64.deb
```

None of these commands touch your shell startup files, sessions, preferences or
installed plugins. They live under your own home directory and are owned by the
application, not by the package.

### Coexisting with other copies

The package installs `/usr/bin/among` and its desktop entry. If you also have a
Cargo-built `among` on your `PATH`, or a Snap, or a copy installed by the
per-user source installer, they coexist: each is a separate file. To launch the
system package specifically rather than whatever your `PATH` resolves first:

```sh
/usr/bin/among
```

To remove only the Snap channel:

```sh
sudo snap remove among
```

To remove only a per-user source installation, remove the directory your
installer used. Removing one copy never removes another.

### What has actually been tested

Package construction and installed acceptance run only on GitHub release
runners, once per target and architecture. The display backend exercised there is
X11 under Xvfb; Wayland sessions are not covered by that evidence. Nothing here
is a claim about distributions, versions or architectures outside the table
above, and no package is offered through an APT repository.

With the four actual built binaries in one directory and debhelper installed:

```sh
tools/among-tools/target/release/among-tools check-deb-selection . /path/to/release-binaries
```

The second check runs real `dh_install` and `dh_missing --fail-missing` in a
temporary package tree and compares every installed file and its mode with the
staged input. It does not install into the host system or certify target ABI,
dependency resolution, GUI operation, upgrades or release readiness.

Pure policy and staging commands, after building the tooling package:

```sh
tools/among-tools/target/release/among-tools deb-release-inputs . v0.1.0 debian13 amd64
tools/among-tools/target/release/among-tools stage-deb . /path/to/release-binaries /path/to/new-staging
```

The stable tag must agree with both product manifests and AppStream. Staging
requires four regular executable ELF inputs and complete attribution materials;
it refuses existing output and writes only distribution-owned paths. No command
in this section constructs or installs a DEB. Run project scripts and checks in
an isolated container, never against a personal installation.
