# Installing Among Terminal

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
