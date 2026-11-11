# Installer and desktop integration

Ready-made downloads and end-user installation instructions live in the
[installation guide](https://among-terminal.github.io/install/).
This document covers source installation, package integration and installer
ownership. Native build prerequisites are in [building](build.md).

## Local DEB cards in GNOME Software

Keep licensing, screenshot and release fields in the
[AppStream metadata](../data/com.github.among-terminal.metainfo.xml).
The native package identity check in [deb.rs](../tools/among-tools/src/deb.rs)
requires its current release to match the upstream tag, including an RC suffix.
Preserve earlier release entries when preparing a package.

Validate the local-file card separately from the installed application entry;
valid AppStream XML does not establish how GNOME Software displays a standalone
DEB. Consumer-facing limitations belong in the
[installation guide](https://among-terminal.github.io/install/).

## Desktop notification integration

Desktop bell notifications use `com.github.among-terminal`. GNOME Shell requires
a matching `com.github.among-terminal.desktop` in its application search path.
DEB and RPM install it; the Snap recipe includes it with `confinement: classic`.
For Snap, check the entry exported to the host: the canonical file inside the
Snap alone does not prove registration. See [Snap packaging](snap.md).

The file declares `X-GNOME-UsesNotifications=true` for GNOME settings. It does
not enable D-Bus activation; click actions address the running application.
Strict Snap confinement requires a separate review of the `desktop` plug and
notification-service D-Bus permissions. Flatpak packaging remains separate.

Bare AppImages and extracted archives install no desktop entry. GNOME may discard
notifications even when a session service runs. An `appimagekit_…` entry alone
does not match the notification identity. Among reports a missing service and
sends through the selected GTK, portal or freedesktop D-Bus service. Acceptance
does not prove a banner appeared. Manual registration is documented on the
[site](https://among-terminal.github.io/install/#desktop-notification-integration).

## From source

From a checkout, run as your normal user:

```sh
sh scripts/install.sh --install-deps
```

Rust/Cargo 1.98 or newer is required for the native installer bootstrap and
source build. `--install-deps` uses `sudo apt-get` for Debian/Ubuntu build
dependencies; it neither installs Rust nor changes the toolchain. No project
interpreter is used. Elsewhere, install GTK 4.12+, libadwaita 1.5+, GTK4 VTE
0.76+, a C toolchain, pkg-config and a gdk-pixbuf SVG loader, then run:

```sh
sh scripts/install.sh
```

The installer runs `cargo install --path … --locked` in temporary staging before
replacing installed files. No build runs under sudo. The executable is
`~/.local/bin/among`; PATH determines whether `among` finds it. The installer
does not edit shell startup files. A desktop session may require logout and
login before its applications menu refreshes.

## AppImage process environment

The AppImage bundles GTK, libadwaita, VTE and their dependencies, with the host
library requirements listed on the site. Its launcher exports no library,
GSettings or XDG data search path. Among configures bundled schemas, MIME data
and image loaders inside its own process.

The runtime sets `APPIMAGE`, `APPDIR`, `ARGV0` and, when mounted, `OWD`. Among
does not remove them, so terminal children inherit them. Clean-host checks cover
explicit extraction and `AppRun`; the isolated GNOME integration check uses
`--appimage-extract-and-run`, which extracts to a temporary directory on each start.

## AppImageLauncher integration

AppImageLauncher creates its own `appimagekit_…-Among_Terminal.desktop` entry and
icon name. The host binds an AppImage Wayland surface to that entry when launch
context identifies the same image. Without a valid GIO hint, a unique existing
entry whose executable matches the exact file supplies the association.
Ambiguous duplicates are not guessed and no registration is created automatically.
Global GApplication identity, configuration paths and native entries are unchanged.
`GIO_LAUNCHED_DESKTOP_FILE` hints are removed before terminal children start.
An unmatched launch keeps normal identity.

Images carry 128 px and 256 px PNG variants of the approved mark. The tested
alpha-4 consumer produces transparent thumbnails on the SVG-only path despite
a valid themed SVG. The integration regression must decode generated thumbnails
without clearing unrelated icon or thumbnail caches.

The regression uses Debian 13 / GNOME Shell 48.7 / Wayland and AppImageLauncher
`3.0.0-alpha-4-gha253~36951ec`. It integrates while Shell is running, opens the
file without GIO hints, then checks menu activation, pin activation and a fresh
process launch. This evidence covers the tested image and consumer only.

For the controlled upgrade procedure, use the
[installation guide](https://among-terminal.github.io/install/).
Integration regressions must preserve unrelated desktop entries and native
package pins. Check the desktop entry's executable association separately
from icon decoding and Shell activation.

## Cargo installation (binary only)

Keep these package-identity examples synchronized with the package metadata;
`among-tools` checks them as Debian source inputs. With the native prerequisites
from [building](build.md) installed:

```sh
cargo install --git https://github.com/Among-Terminal/among --locked among
among
```

This builds the moving default branch into Cargo's bin directory, normally
`~/.cargo/bin`. It installs no desktop entry, icons or plugins. Do not run Cargo
as root. Rerun the command to update; remove this binary with:

```sh
cargo uninstall among
```

Settings, sessions and separately installed plugins remain.

## Update and remove

Rerun installation from the updated checkout. Installation never closes running
terminals; relaunch to use the new binary. Remove a source installation with:

```sh
sh scripts/install.sh --uninstall
```

The installed `~/.local/bin/among-uninstall` works without the checkout.
Only recorded files are removed; preferences, sessions and separately installed
plugins remain. Unrelated files, symlinks and locally modified installed files
block replacement and removal. Move conflicts aside before retrying. Cargo's
`~/.cargo/bin/among` is neither updated nor removed; use the menu or absolute path
to distinguish installations.

## Prebuilt executable

Install a native binary with the same desktop assets:

```sh
sh scripts/install.sh --binary ./target/release/among
```

The binary must match the architecture and native libraries. This mode neither
builds Among nor installs dependencies or certifies compatibility. The shell
launcher builds the headless Rust installer if missing. To avoid bootstrap,
supply native `among-tools` and its sibling `among-uninstall` from the same
checked build:

```sh
AMONG_TOOLS=/absolute/path/among-tools sh scripts/install.sh --binary ./among
```

The installed uninstaller requires no Cargo, interpreter or checkout. A valid
version-1 installation made by the old installer is upgraded only after helper
and launcher ownership/hash checks pass. Edited legacy files block the upgrade.

## Installed files

- `~/.local/bin/among`
- `~/.local/bin/among-uninstall`
- `$XDG_DATA_HOME/applications/com.github.among-terminal.desktop`
- `$XDG_DATA_HOME/icons/hicolor/scalable/apps/com.github.among-terminal.svg`
- `$XDG_DATA_HOME/metainfo/com.github.among-terminal.metainfo.xml`
- `$XDG_DATA_HOME/doc/among/`: license, notice and third-party inventory
- `$XDG_STATE_HOME/among-installer/install.json`: ownership and content hashes

Defaults are `~/.local/share` and `~/.local/state`. Installation, update and
removal must use the same HOME and XDG directories. A lock excludes simultaneous
installers. Each replacement is atomic; ordinary write errors roll back installed
files. Power loss or forced termination is not a transaction across all files
and may require recovery. Rollback can fail on full or unwritable storage and
must report that failure. Originals whose bytes and mode match saved state need
no rewrite. Optional plugins remain a separate installation through the official
source; see [plugins](https://among-terminal.github.io/plugins/#installing).
