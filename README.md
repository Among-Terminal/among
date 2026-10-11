<p><img src="docs/media/logo.png" width="150" alt="Among Terminal logo"></p>

# Among Terminal

A native GNOME terminal multiplexer built with Rust, GTK4, libadwaita and VTE.
Keep shells running in split panes and named workspaces, with keyboard controls
and optional status plugins.

## Quick install

On Debian/Ubuntu, with Git and Rust/Cargo 1.98+ installed:

```sh
git clone https://github.com/Among-Terminal/among.git && cd among && sh scripts/install.sh --install-deps
```

Launch **Among Terminal** from the applications menu. The installer uses sudo
only for system dependencies; build and application installation run as your user.
[Other distributions, updates and removal](https://among-terminal.github.io/install/).

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
