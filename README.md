<p><img src="docs/media/logo.png" width="150" alt="Among Terminal logo"></p>

# Among Terminal

[![CI](https://img.shields.io/github/actions/workflow/status/Among-Terminal/among/checks.yml?branch=main&label=CI)](https://github.com/Among-Terminal/among/actions/workflows/checks.yml?query=branch%3Amain)
[![Latest release](https://img.shields.io/github/v/release/Among-Terminal/among)](https://github.com/Among-Terminal/among/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/Among-Terminal/among/total?label=downloads)](https://github.com/Among-Terminal/among/releases)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)

A native GNOME terminal multiplexer built with Rust, GTK4, libadwaita and VTE.
Keep shells running in split panes and named workspaces, with keyboard controls
and optional status plugins.

[![Among Terminal: split panes, workspaces and live status](docs/media/among-terminal-preview.gif)](https://among-terminal.github.io/features/)

## Installation

See the [installation guide](https://among-terminal.github.io/install/) for
packages, requirements, verification, updates and removal.
Downloads are on the [releases page](https://github.com/Among-Terminal/among/releases).

| x86-64 (`amd64`) | ARM 64-bit (`arm64`) |
|---|---|
| [![Download Ubuntu 24.04 DEB for amd64](https://img.shields.io/badge/Ubuntu_24.04-DEB_amd64-C7461A?style=for-the-badge&logo=ubuntu&logoColor=white)](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-ubuntu24.04-amd64.deb) | [![Download Ubuntu 24.04 DEB for arm64](https://img.shields.io/badge/Ubuntu_24.04-DEB_arm64-C7461A?style=for-the-badge&logo=ubuntu&logoColor=white)](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-ubuntu24.04-arm64.deb) |
| [![Download Debian 13 DEB for amd64](https://img.shields.io/badge/Debian_13-DEB_amd64-A81D33?style=for-the-badge&logo=debian&logoColor=white)](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-debian13-amd64.deb) | [![Download Debian 13 DEB for arm64](https://img.shields.io/badge/Debian_13-DEB_arm64-A81D33?style=for-the-badge&logo=debian&logoColor=white)](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-debian13-arm64.deb) |
| [![Download Fedora 44 RPM for amd64](https://img.shields.io/badge/Fedora_44-RPM_amd64-294172?style=for-the-badge&logo=fedora&logoColor=white)](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-fedora44-amd64.rpm) | [![Download Fedora 44 RPM for arm64](https://img.shields.io/badge/Fedora_44-RPM_arm64-294172?style=for-the-badge&logo=fedora&logoColor=white)](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-fedora44-arm64.rpm) |
| [![Download Fedora 43 RPM for amd64](https://img.shields.io/badge/Fedora_43-RPM_amd64-294172?style=for-the-badge&logo=fedora&logoColor=white)](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-fedora43-amd64.rpm) | [![Download Fedora 43 RPM for arm64](https://img.shields.io/badge/Fedora_43-RPM_arm64-294172?style=for-the-badge&logo=fedora&logoColor=white)](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-fedora43-arm64.rpm) |
| [![Download Linux AppImage for amd64](https://img.shields.io/badge/Linux-AppImage_amd64-333333?style=for-the-badge&logo=linux&logoColor=white)](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-amd64.AppImage) | [![Download Linux AppImage for arm64](https://img.shields.io/badge/Linux-AppImage_arm64-333333?style=for-the-badge&logo=linux&logoColor=white)](https://github.com/Among-Terminal/among/releases/latest/download/among-terminal-arm64.AppImage) |

AppImages require glibc 2.39 or newer.
Verify downloads with the release's [SHA256SUMS](https://github.com/Among-Terminal/among/releases/latest/download/SHA256SUMS)
as described in the installation guide.

## User guides

- [Overview](https://among-terminal.github.io/overview/) and [features](https://among-terminal.github.io/features/)
- [Panes and workspaces](https://among-terminal.github.io/panes/), [sessions](https://among-terminal.github.io/sessions/) and [shortcuts](https://among-terminal.github.io/shortcuts/)
- [Preferences](https://among-terminal.github.io/preferences/) and [plugins](https://among-terminal.github.io/plugins/)

## Plugin authors

The [plugin template](https://github.com/Among-Terminal/among-plugin-template)
contains the [author guide](https://github.com/Among-Terminal/among-plugin-template/blob/main/docs/guide.md),
[API reference](https://github.com/Among-Terminal/among-plugin-template/blob/main/docs/api.md)
and [release instructions](https://github.com/Among-Terminal/among-plugin-template/blob/main/docs/releasing.md).

## Contributors and packagers

Read [CONTRIBUTING.md](CONTRIBUTING.md) for the development process and documentation
map, and [building from source](docs/build.md) for build requirements.
This repository keeps build, development and packaging documentation.

## License

Original code and examples are [Apache-2.0](LICENSE).
Third-party components retain their own terms: see [NOTICE](NOTICE),
[dependency notices](THIRD_PARTY/inventory.json) and
[licensing](https://among-terminal.github.io/licensing/).
