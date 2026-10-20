# Installing Among Terminal

## Ready-made packages

Every [GitHub Release](https://github.com/Among-Terminal/among/releases) carries
the same files for x86-64 (`amd64`) and ARM 64-bit (`arm64`), under names without
a version. `https://github.com/Among-Terminal/among/releases/latest/download/<file>`
always fetches the file from the newest stable release; the README links each one.
`uname -m` prints `x86_64` for amd64 and `aarch64` for arm64.

| Your system | Files | Details |
|---|---|---|
| Debian 13, Ubuntu 24.04 | `among-terminal-<debian13 or ubuntu24.04>-<arch>.deb` | [Debian packaging](debian-packaging.md#installing-a-built-package) |
| Fedora 43, Fedora 44 | `among-terminal-<fedora43 or fedora44>-<arch>.rpm` | [RPM packaging](rpm-packaging.md#choosing-the-package-for-your-system) |
| Other distributions with glibc 2.39 or newer | `among-terminal-<arch>.AppImage` | [AppImage](#appimage) |
| Debian 13, Ubuntu 24.04 without a package manager | `among-terminal-<debian13 or ubuntu24.04>-<arch>.tar.gz` | [Binary archive](#binary-archive) |

```sh
sudo apt install ./among-terminal-ubuntu24.04-amd64.deb
sudo dnf install ./among-terminal-fedora44-amd64.rpm
```

APT and DNF install the libraries Among needs from the distribution. Each
release also carries `SHA256SUMS`; download it next to your file and check it:

```sh
sha256sum --check --ignore-missing SHA256SUMS
```

The line for your file must end in `OK`. Releases are immutable: once
published, their files and tag cannot change. GitHub records an attestation for
each release; with the GitHub CLI, `gh release verify-asset <tag> <file> --repo
Among-Terminal/among` checks a downloaded file against it.

To update, download the newer release's file and install it the same way. Close
and reopen Among to use the new version.

### Local DEB cards in GNOME Software

On Debian 13 with GNOME Software 48.3 / PackageKit 1.3.1, opening a standalone
DEB shows package fallback information: `among-terminal`, a generic package
icon, `Unknown License`, `No Screenshots` and no release description. Installing
the package makes its desktop name and application icon available in the
Installed list, but reopening the DEB can still show the fallback card.

Among ships Apache-2.0 project licensing, a public UI screenshot and release
notes in AppStream metadata. A separately indexed AppStream component can show
those fields; valid bundled XML alone does not make the local-file path use it.
Native package builds bind the current AppStream release to the package's
upstream version, including its RC suffix, without rewriting release history.

Local installation adds no Among repository or signing key. The consumer's
third-party trust and missing-repository/update warnings remain applicable.
Debian/Ubuntu APT catalogue publication is separate work, not a feature of these
standalone packages. Third-party components keep their own licence terms.

## From source

From a source checkout, run as your normal user:

```sh
sh scripts/install.sh --install-deps
```

Rust/Cargo 1.98 or newer is required for the native installer bootstrap and source
build. No project interpreter is used. `--install-deps` uses
`sudo apt-get` to install Debian/Ubuntu build dependencies. It does not install
Rust or change your toolchain. On other distributions install GTK 4.12+,
libadwaita 1.5+, GTK4 VTE 0.76+, a C toolchain, pkg-config and a gdk-pixbuf SVG
loader first, then run:

```sh
sh scripts/install.sh
```

The installer builds with `cargo install --path … --locked` into a temporary
staging directory before replacing installed files. No build runs under sudo.
Launch **Among Terminal** from the applications menu, or `~/.local/bin/among`.
If `~/.local/bin` is already in PATH, the command is `among`.
The installer does not edit shell startup files. If the menu does not refresh
immediately, log out and back in.

## Binary archive

A binary archive holds the same executables as the DEB of the same release,
without a package manager. It is built for one distribution and architecture
and does not bundle libraries: the distribution must provide them.

| Your system | File |
|---|---|
| Debian 13, x86-64 | `among-terminal-debian13-amd64.tar.gz` |
| Debian 13, ARM 64-bit | `among-terminal-debian13-arm64.tar.gz` |
| Ubuntu 24.04, x86-64 | `among-terminal-ubuntu24.04-amd64.tar.gz` |
| Ubuntu 24.04, ARM 64-bit | `among-terminal-ubuntu24.04-arm64.tar.gz` |

`dpkg --print-architecture` prints `amd64` or `arm64`. Download the archive and
the release's `SHA256SUMS` into one directory and verify the archive:

```sh
sha256sum --check --ignore-missing SHA256SUMS
```

The line for the archive must end in `OK`. Install the runtime libraries.
`INSTALL.txt` inside the archive lists the exact packages and minimum versions
the DEB of the same build depends on; on Debian 13 and Ubuntu 24.04 this
installs them:

```sh
sudo apt-get install --no-install-recommends libgtk-4-1 libadwaita-1-0 libvte-2.91-gtk4-0 librsvg2-common
```

Extract into a new directory and start Among:

```sh
mkdir ~/among
tar -xzf among-terminal-ubuntu24.04-arm64.tar.gz -C ~/among
~/among/bin/among
```

No compiler, Rust toolchain or Among package is needed. The archive adds no
applications-menu entry. The example plugins are in `~/among/plugins/`; they
stay disabled until you install and enable them as described in
[plugins](plugins.md#installing). To remove Among, delete `~/among`. Settings,
sessions and separately installed plugins are preserved.

## AppImage

An AppImage is one executable file that carries Among together with GTK,
libadwaita, VTE and the libraries they need. It installs nothing.

| Your system | File |
|---|---|
| x86-64 | `among-terminal-amd64.AppImage` |
| ARM 64-bit | `among-terminal-arm64.AppImage` |

Supported hosts: Debian 13, Ubuntu 24.04 and Fedora 43 on both architectures;
these are the hosts its release checks start it on. It needs GNU C library
2.39 or newer, because it is built on Ubuntu 24.04. Some libraries are not
bundled and must come from the host: the C and C++ runtime, X11 and XCB,
Wayland client, fontconfig, FreeType, HarfBuzz, FriBidi, Expat, GMP,
libgpg-error, com_err and zlib. The release checks install them on otherwise
clean hosts with:

```sh
sudo apt-get install libwayland-client0 libx11-6 libxcb1 libfontconfig1 libfreetype6 libharfbuzz0b \
  libfribidi0 libexpat1 libgmp10 libgpg-error0 libcom-err2 zlib1g libstdc++6 fonts-dejavu-core
sudo dnf install libwayland-client libX11 libxcb fontconfig freetype harfbuzz fribidi expat gmp \
  libgpg-error libcom_err zlib-ng-compat libstdc++ dejavu-sans-fonts
```

The first line is for Debian and Ubuntu, the second for Fedora.

Download the AppImage and the release's `SHA256SUMS` into one directory, verify
it, make it executable and start it:

```sh
sha256sum --check --ignore-missing SHA256SUMS
chmod +x among-terminal-amd64.AppImage
./among-terminal-amd64.AppImage
```

The AppImage mounts itself with FUSE, which needs `fusermount3` from the
`fuse3` package (`sudo apt-get install fuse3` or `sudo dnf install fuse3`).
Without FUSE it prints `Cannot mount AppImage, please check your FUSE setup`.
Then extract it and start the extracted copy instead:

```sh
./among-terminal-amd64.AppImage --appimage-extract
./squashfs-root/AppRun
```

This creates `squashfs-root/` in the current directory; move or delete it like
any directory. The upstream runtime also offers `--appimage-extract-and-run`,
which extracts to a temporary directory on every start
([AppImage documentation](https://docs.appimage.org/user-guide/troubleshooting/fuse.html));
The package clean-host checks cover the extraction above. The separate isolated
GNOME integration check uses the runtime's extract-and-run mode.

The AppImage's launcher exports no library, GSettings or XDG data search
path; Among sets up its bundled schemas, MIME data and image loaders inside
its own process. The AppImage runtime sets `APPIMAGE`, `APPDIR`, `ARGV0` and,
when mounted, `OWD` before starting Among, and Among does not remove them, so
programs started in its shells inherit them.

The AppImage does not add itself to the applications menu. Its desktop entry
uses the ID `com.github.among-terminal`. To add a menu entry for the file at
`~/Applications/among.AppImage`:

```sh
mkdir -p ~/.local/share/applications ~/.local/share/icons/hicolor/scalable/apps
cd ~/Applications
./among.AppImage --appimage-extract usr/share/icons/hicolor/scalable/apps/com.github.among-terminal.svg
cp squashfs-root/usr/share/icons/hicolor/scalable/apps/com.github.among-terminal.svg \
  ~/.local/share/icons/hicolor/scalable/apps/
rm -r squashfs-root
cat > ~/.local/share/applications/com.github.among-terminal.desktop <<EOF
[Desktop Entry]
Type=Application
Name=Among Terminal
Comment=Organize live terminals in project workspaces
Exec=$HOME/Applications/among.AppImage
Icon=com.github.among-terminal
Terminal=false
Categories=System;TerminalEmulator;
StartupNotify=true
StartupWMClass=among
EOF
```

If you move or rename the AppImage, edit `Exec=` accordingly.

### AppImageLauncher

AppImageLauncher creates its own `appimagekit_…-Among_Terminal.desktop` entry and
icon name. Builds from this source bind an AppImage's Wayland surface to that
entry when its launch context identifies the same AppImage file. If a file-manager
or terminal launch has no valid GIO hint, a unique existing AppImageLauncher entry
whose executable matches that exact file supplies the association. No entry is
created automatically, and ambiguous duplicate registrations are not guessed.
The global
GApplication ID, configuration paths and native desktop entry remain unchanged.
The `GIO_LAUNCHED_DESKTOP_FILE` hints used for this association are removed before
terminal children start. Launches without a matching registration keep the normal identity.

AppImages also include 128 px and 256 px PNG variants of the approved mark for
AppImageLauncher's file thumbnails. On the tested alpha-4 consumer, the SVG-only
path produces fully transparent thumbnails despite a valid themed SVG. The
integration regression decodes the generated thumbnails, not just the installed
icon. Existing RC.1 files do not gain this correction automatically; integrate
the corrected image rather than clearing unrelated thumbnail or icon caches.

The integration regression uses Debian 13 / GNOME Shell 48.7 / Wayland with
AppImageLauncher `3.0.0-alpha-4-gha253~36951ec`. It integrates while Shell is
already running, opens the integrated file without GIO launch hints, then checks
menu activation, pin activation and fresh-process relaunch. Results apply to the
actual image tested, not every integrator or desktop. Immutable 0.1.1 and the
first 0.1.2 RC lack the unhinted-file-launch correction.

For a controlled upgrade, close the old application and remove only its owned
integration before integrating the replacement:

```sh
ail-cli unintegrate "/actual/integrated/old.AppImage"
ail-cli integrate "/path/to/new.AppImage"
```

Use the old entry's actual `Exec` path: AppImageLauncher can move the image to a
hash-suffixed filename. Pin the newly generated entry. Beside a native package,
AppImageLauncher can suffix the display name with `(1)`; native DEB pins remain
separate. Do not delete unrelated desktop entries or reset the entire dock.
Old pins, duplicate manual launchers and icon caches can still refer to older
files; an invisible icon is not proof of which layer failed.

Without a host desktop registration, GNOME Wayland may show a technical
application ID and generic icon for a directly executed AppImage. Its embedded
icon does not register an application with the host shell. Use AppImageLauncher
or the explicit manual desktop entry above. If a native DEB is installed,
prefer AppImageLauncher's distinct entry: a manual entry with the native ID can
override its launcher. Among does not silently create host registration files.

The example plugins are inside the image in `usr/share/among/plugins/`;
`--appimage-extract 'usr/share/among/plugins/*'` copies them out for
installation as described in [plugins](plugins.md#installing). To remove Among,
delete the AppImage file and, if you added them, the desktop entry and icon
above. Settings, sessions and separately installed plugins are preserved.

## Cargo installation (binary only)

With the native prerequisites from [building](build.md) installed:

```sh
cargo install --git https://github.com/Among-Terminal/among --locked among
among
```

This builds the moving default branch and installs only the executable in Cargo's
bin directory (normally `~/.cargo/bin`). It does not install a desktop entry,
icons or plugins. Use the source installer above for desktop integration.
Do not run Cargo as root. Rerun the command to update, and remove this binary with:

```sh
cargo uninstall among
```

Settings, sessions and separately installed plugins are preserved.

## Update and remove

Rerun the installation command from the updated checkout to update.
Close and relaunch the application to use the new binary; installation never
closes running terminals for you.

```sh
sh scripts/install.sh --uninstall
```

The checkout is not required for removal: run `~/.local/bin/among-uninstall`.

Removal preserves application preferences, sessions and separately installed
plugins. Only files recorded by this installer are removed. Existing unrelated
files, symlinks and locally modified installed files block both replacement and
removal; move conflicting files aside before retrying. A binary previously
installed by `cargo install` under `~/.cargo/bin` is not removed or updated;
use the applications-menu entry or the absolute path above to avoid ambiguity.

## Prebuilt executable

To install an already built native binary and the same desktop assets:

```sh
sh scripts/install.sh --binary ./target/release/among
```

The binary must match your architecture and installed native libraries. This
mode does not build the application or install dependencies, nor certify binary
compatibility. The shell launcher builds the headless Rust installer if it is
missing. To avoid that bootstrap, supply prebuilt `among-tools` and its sibling
`among-uninstall` (both native for this machine):

```sh
AMONG_TOOLS=/absolute/path/among-tools sh scripts/install.sh --binary ./among
```

The two tooling binaries must come from the same checked build. Once installed,
`~/.local/bin/among-uninstall` does not need Cargo, an interpreter or the checkout.
A valid version-1 installation made by the old installer is upgraded only after
its helper and launcher pass the existing ownership/hash checks; edited legacy
files block the upgrade instead of being deleted.

## Installed files

- `~/.local/bin/among`
- `~/.local/bin/among-uninstall`
- `$XDG_DATA_HOME/applications/com.github.among-terminal.desktop`
- `$XDG_DATA_HOME/icons/hicolor/scalable/apps/com.github.among-terminal.svg`
- `$XDG_DATA_HOME/metainfo/com.github.among-terminal.metainfo.xml`
- `$XDG_DATA_HOME/doc/among/`: license, notice and third-party license inventory
- `$XDG_STATE_HOME/among-installer/install.json`: ownership and content hashes

The XDG defaults are `~/.local/share` and `~/.local/state`. Use the same HOME
and XDG directories for installation, updates and removal. A lock prevents
simultaneous installers. Each file is replaced atomically; ordinary write
failures roll back installed files. Power loss or forced termination during
installation is not a transaction across all files and may require recovery.
Rollback of changed files can itself fail if storage becomes full or unwritable;
the installer reports those errors rather than claiming recovery. Originals
whose bytes and mode already match the saved state do not need to be rewritten.

Optional plugins remain a separate installation:
`sh scripts/install-plugins.sh --enable-new`; see [plugins](plugins.md#installing).
