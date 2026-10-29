# Contributing to Among Terminal

## What this project is

A GNOME terminal multiplexer in Rust. Terminal emulation is VTE's job; this
project owns the pane tree, workspace lifecycle, key routing, and the
out-of-process plugin pipeline.

That division matters when contributing: a change that reaches into
`libvte` is fighting the design, not extending it.

## Where the design lives

The behaviour each subsystem promises is documented next to the code it
describes: `docs/panes.md` for the pane tree, `docs/sessions.md` for workspace
state, `docs/plugins.md` for the plugin protocol, and `docs/shortcuts.md` for
the key bindings and how they are wired. `docs/features.md` summarises what the
application does from a user's point of view.

Read the relevant document before changing behaviour, and change it in the same
commit as the code: a document that no longer matches the code is a bug in the
document.

## Building

See [docs/build.md](docs/build.md). In short:

```sh
sudo apt install libgtk-4-dev libvte-2.91-gtk4-dev libadwaita-1-dev
cargo build --locked
cargo test --locked
```

## Conventions

Original code and examples are Apache-2.0. By intentionally submitting a
contribution, you offer it under Apache-2.0 unless separately agreed otherwise.
Keep third-party attribution and license terms intact; identify copied/adapted
code explicitly. See [licensing and distribution obligations](docs/licensing.md).

- **Comments explain why, not what.** If a line of code needs a comment to say
  what it does, it needs a better name or a simpler body. Keep a comment when the
  reason is not visible from the code: a constraint, a trade-off, a behaviour
  that would otherwise look like a mistake.
- **The model holds no widget types.** `src/tree.rs` and `src/workspace.rs` are
  data. Anything that needs a `gtk::Widget` belongs in `src/app.rs`. This is what
  lets the layout be tested without a display.
- **Comments and documentation are in English.**
- **No panics on a user's input.** The runtime code contains no `unwrap`,
  `expect`, or `panic!`; a failure in a plugin or a terminal must not take the
  application down.

## Tests

`cargo test` runs the unit tests. They cover the model — the pane tree, the
workspace set, size reconciliation, the plugin protocol — and need no display.

Behaviour that only exists in a running window is verified differently: the
application is driven with scripted actions (`AMONG_SCRIPT`) and real key events
(`xdotool`), and its state is read back from `AMONG_SELFCHECK`. See
[Verification aids](docs/testing.md) for the full list of switches.

A change to layout, focus, or the plugin pipeline is not finished until it has
been observed at that boundary. A test that only proves a function returns a
value of the right shape has not proved the behaviour works.

## Checking changes

### Local quality gates

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
cargo fmt --check --manifest-path examples/plugins/Cargo.toml
cargo clippy --locked --all-targets --manifest-path examples/plugins/Cargo.toml -- -D warnings
cargo test --locked --manifest-path examples/plugins/Cargo.toml
cargo build --release --locked --manifest-path examples/plugins/Cargo.toml
cargo build --release --locked --manifest-path tools/among-tools/Cargo.toml --example notice-plugin
cargo fmt --check --manifest-path tools/among-tools/Cargo.toml
cargo clippy --locked --all-targets --manifest-path tools/among-tools/Cargo.toml -- -D warnings
cargo test --locked --manifest-path tools/among-tools/Cargo.toml
cargo build --release --locked --manifest-path tools/among-tools/Cargo.toml --bin among-tools
tools/among-tools/target/release/among-tools third-party-notices . THIRD_PARTY check
cargo test --locked --manifest-path tools/among-tools/Cargo.toml --test release_policy \
  --test stable_build --test release_metadata --test promotion --test stable_source \
  --test source_delivery --test source_snapshot --test delivery_package --test notices
desktop-file-validate data/com.github.among-terminal.desktop
appstreamcli validate --no-net --strict data/com.github.among-terminal.metainfo.xml
```

`scripts/check-packages.sh` performs the format, Clippy, test and release-build
checks for all three packages; the commands above are what it runs. The three
packages are independent: the application and its example plugins never depend
on the tooling package, and tooling tests do not link GTK or need a display.

On X11 with Openbox, private D-Bus, xdotool, xprop and ImageMagick installed,
build the application and plugin examples first and then drive real windows
through the Rust tooling:

```sh
cargo build --release --locked
cargo build --release --locked --manifest-path examples/plugins/Cargo.toml
tool=tools/among-tools/target/release/among-tools
$tool ci-x11 $tool check-live-focus target/release/among
$tool ci-x11 $tool check-live-status . target/release/among examples/plugins/target/release
$tool ci-x11 $tool check-sessions target/release/among
$tool ci-x11 $tool check-split-directory target/release/among
$tool ci-x11 $tool check-scroll-calibration target/release/among
$tool ci-x11 $tool check-sidebar-rename target/release/among
$tool ci-x11 $tool check-pane-workspace-move target/release/among
$tool ci-x11 $tool check-window-controls target/release/among tools/among-tools/target/release/examples/notice-plugin
$tool ci-x11 $tool check-desktop . target/release/among
```

These use private test data and only stop test-owned processes. Their
evidence stays in your cache directory (`~/.cache/among/checks` by default,
`AMONG_CHECK_ROOT` overrides it); see [where checks keep their evidence](docs/testing.md#where-checks-keep-their-evidence).
Do not run raw
input automation against your personal application. Native-X11 evidence does
not establish Snap/Wayland support.

Project-owned tooling is Rust only: a Python interpreter must not appear in
contributed sources, workflows, packaging, launchers or executable
documentation. Snapcraft's own internal use of Python is an external tool
detail and not a project entry point.

## Reporting a plugin fault

Plugins run as separate processes on purpose: a plugin that crashes takes only
itself down. If a plugin can take the application down, that is a bug in the
host, not the plugin — please report it with the plugin's source and the
application's stderr.
