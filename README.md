<p><img src="docs/media/logo.png" width="150" alt="Among Terminal logo"></p>

# Among Terminal

[![CI](https://img.shields.io/github/actions/workflow/status/Among-Terminal/among/checks.yml?branch=main&label=CI)](https://github.com/Among-Terminal/among/actions/workflows/checks.yml?query=branch%3Amain)
[![Latest release](https://img.shields.io/github/v/release/Among-Terminal/among)](https://github.com/Among-Terminal/among/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/Among-Terminal/among/total?label=downloads)](https://github.com/Among-Terminal/among/releases)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)

A native GNOME terminal multiplexer built with Rust, GTK4, libadwaita and VTE.
Keep shells running in split panes and named workspaces, with keyboard controls
and optional status plugins.

## Installation

Download the file for your system. The links always fetch the
[latest release](https://github.com/Among-Terminal/among/releases/latest); no
Rust toolchain or compilation is needed.

| x86-64 (`amd64`) | ARM 64-bit (`arm64`) |
|---|---|
| [![Download Ubuntu 24.04 DEB for amd64](https://img.shields.io/badge/Ubuntu_24.04-DEB_amd64-C7461A?style=for-the-badge&logo=ubuntu&logoColor=white)](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-ubuntu24.04-amd64.deb) | [![Download Ubuntu 24.04 DEB for arm64](https://img.shields.io/badge/Ubuntu_24.04-DEB_arm64-C7461A?style=for-the-badge&logo=ubuntu&logoColor=white)](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-ubuntu24.04-arm64.deb) |
| [![Download Debian 13 DEB for amd64](https://img.shields.io/badge/Debian_13-DEB_amd64-A81D33?style=for-the-badge&logo=debian&logoColor=white)](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-debian13-amd64.deb) | [![Download Debian 13 DEB for arm64](https://img.shields.io/badge/Debian_13-DEB_arm64-A81D33?style=for-the-badge&logo=debian&logoColor=white)](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-debian13-arm64.deb) |
| [![Download Fedora 44 RPM for amd64](https://img.shields.io/badge/Fedora_44-RPM_amd64-294172?style=for-the-badge&logo=fedora&logoColor=white)](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-fedora44-amd64.rpm) | [![Download Fedora 44 RPM for arm64](https://img.shields.io/badge/Fedora_44-RPM_arm64-294172?style=for-the-badge&logo=fedora&logoColor=white)](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-fedora44-arm64.rpm) |
| [![Download Fedora 43 RPM for amd64](https://img.shields.io/badge/Fedora_43-RPM_amd64-294172?style=for-the-badge&logo=fedora&logoColor=white)](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-fedora43-amd64.rpm) | [![Download Fedora 43 RPM for arm64](https://img.shields.io/badge/Fedora_43-RPM_arm64-294172?style=for-the-badge&logo=fedora&logoColor=white)](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-fedora43-arm64.rpm) |
| [![Download Linux AppImage for amd64](https://img.shields.io/badge/Linux-AppImage_amd64-333333?style=for-the-badge&logo=linux&logoColor=white)](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-amd64.AppImage) | [![Download Linux AppImage for arm64](https://img.shields.io/badge/Linux-AppImage_arm64-333333?style=for-the-badge&logo=linux&logoColor=white)](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-arm64.AppImage) |

AppImages require glibc 2.39 or newer.

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
