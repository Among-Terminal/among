<p><img src="docs/media/logo.png" width="150" alt="Among Terminal logo"></p>

# Among Terminal

A native GNOME terminal multiplexer built with Rust, GTK4, libadwaita and VTE.
Keep shells running in split panes and named workspaces, with keyboard controls
and optional status plugins.

## Installation

### Release packages (recommended)

Download a ready-made **0.1.0 RC2 preview** package for
[Ubuntu 24.04 or Debian 13](https://among-terminal.github.io/install/),
on amd64 or arm64. For Ubuntu 24.04 on amd64, run in the download folder:

```sh
sudo apt install ./among-terminal_0.1.0.rc.2-1.ubuntu24.04_amd64.deb
```

Launch **Among Terminal** from the applications menu. No Git, Rust or build
scripts are required; `apt` installs the required runtime libraries automatically.
This is a prerelease, not a stable release.
[Release downloads](https://github.com/Among-Terminal/among/releases/tag/v0.1.0-rc.2) ·
[Updates and removal](https://among-terminal.github.io/install/).

### Cargo from Git

Requires Rust **1.98 or newer**, a C compiler, `pkg-config`, GTK **4.12+**,
libadwaita **1.5+** and the **GTK4** build of VTE **0.76+**.
On Ubuntu 24.04 or Debian 13, install the build dependencies:

```sh
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev libvte-2.91-gtk4-dev
cargo install --git https://github.com/Among-Terminal/among --locked among
among
```

This builds the current development version from Git and installs the binary
in Cargo's bin directory (normally `~/.cargo/bin`; add it to `PATH`). It does
not install a desktop launcher, icon or example plugins. To update, repeat
`cargo install` with `--force`; to remove, run `cargo uninstall among`.

[![Among Terminal: split panes, workspaces and live status](docs/media/among-terminal-preview.gif)](https://among-terminal.github.io/#demo)

[Watch the demo](https://among-terminal.github.io/#demo) ·
[Feature guide](https://among-terminal.github.io/features/) ·
[Plugin development](https://among-terminal.github.io/plugin-development/)

## Features

- Split, resize and rearrange panes without restarting their shells.
- Switch between named, colour-coded workspaces with a collapsible sidebar.
- Navigate by keyboard, zoom a pane and customize shortcuts.
- Restore workspace layouts and directories into fresh shells.
- Add clocks, resource meters and per-pane status through separate-process plugins.
  Plugins are optional and disabled by default.

## Contributing

For source builds and contributions, see [building](docs/build.md) and
[CONTRIBUTING.md](CONTRIBUTING.md).

## License

Original code and examples are [Apache-2.0](LICENSE).
Third-party components retain their own terms: see [NOTICE](NOTICE),
[dependency notices](THIRD_PARTY/inventory.json) and [licensing](docs/licensing.md).
