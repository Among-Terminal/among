<p><img src="docs/media/logo.png" width="150" alt="Among Terminal logo"></p>

# Among Terminal

[![Latest release](https://img.shields.io/github/v/release/Among-Terminal/among)](https://github.com/Among-Terminal/among/releases/latest)

A native GNOME terminal multiplexer built with Rust, GTK4, libadwaita and VTE.
Keep shells running in split panes and named workspaces, with keyboard controls
and optional status plugins.

## Installation

Download the file for your system. The links always fetch the
[latest release](https://github.com/Among-Terminal/among/releases/latest); no
Rust toolchain or compilation is needed.

| System | x86-64 (`amd64`) | ARM 64-bit (`arm64`) |
|---|---|---|
| Ubuntu 24.04 — DEB | [among-terminal-ubuntu24.04-amd64.deb](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-ubuntu24.04-amd64.deb) | [among-terminal-ubuntu24.04-arm64.deb](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-ubuntu24.04-arm64.deb) |
| Debian 13 — DEB | [among-terminal-debian13-amd64.deb](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-debian13-amd64.deb) | [among-terminal-debian13-arm64.deb](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-debian13-arm64.deb) |
| Fedora 44 — RPM | [among-terminal-fedora44-amd64.rpm](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-fedora44-amd64.rpm) | [among-terminal-fedora44-arm64.rpm](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-fedora44-arm64.rpm) |
| Fedora 43 — RPM | [among-terminal-fedora43-amd64.rpm](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-fedora43-amd64.rpm) | [among-terminal-fedora43-arm64.rpm](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-fedora43-arm64.rpm) |
| Other distributions with glibc 2.39+ — AppImage | [among-terminal-amd64.AppImage](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-amd64.AppImage) | [among-terminal-arm64.AppImage](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-arm64.AppImage) |
| Ubuntu 24.04 — binary archive | [among-terminal-ubuntu24.04-amd64.tar.gz](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-ubuntu24.04-amd64.tar.gz) | [among-terminal-ubuntu24.04-arm64.tar.gz](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-ubuntu24.04-arm64.tar.gz) |
| Debian 13 — binary archive | [among-terminal-debian13-amd64.tar.gz](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-debian13-amd64.tar.gz) | [among-terminal-debian13-arm64.tar.gz](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-debian13-arm64.tar.gz) |

`uname -m` prints `x86_64` for amd64 and `aarch64` for arm64. Install from the
download directory, using the file name you downloaded:

```sh
sudo apt install ./among-terminal-ubuntu24.04-amd64.deb   # Debian, Ubuntu
sudo dnf install ./among-terminal-fedora44-amd64.rpm      # Fedora
chmod +x among-terminal-amd64.AppImage && ./among-terminal-amd64.AppImage
```

Start **Among Terminal** from the applications menu, or run `among`. To verify
downloads, put [SHA256SUMS](https://github.com/Among-Terminal/among/releases/latest/download/SHA256SUMS)
next to them and run `sha256sum --check --ignore-missing SHA256SUMS`.

The [installation guide](docs/install.md) covers AppImage requirements, binary
archives, updates and removal. Earlier versions and their notes are on the
[releases page](https://github.com/Among-Terminal/among/releases). For the
optional `cargo install --git` source-build route, see
[building from source](docs/build.md#cargo-install-from-git).

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
