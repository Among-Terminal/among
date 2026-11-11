# Contributing to Among Terminal

## What this project is

A GNOME terminal multiplexer in Rust. Terminal emulation is VTE's job; this
project owns the pane tree, workspace lifecycle, key routing, and the
out-of-process plugin pipeline.

Keep terminal emulation in VTE when changing the host application.

## Where the design lives

This repository documents development, builds and packaging. User guides live on
the [site](https://among-terminal.github.io/overview/). Plugin author documentation
lives in the [plugin template](https://github.com/Among-Terminal/among-plugin-template).
Link to those guides when a contributor needs usage or protocol instructions.

- [Build requirements](docs/build.md), [installer internals](docs/install.md),
  [Debian packaging](docs/debian-packaging.md), [RPM packaging](docs/rpm-packaging.md)
  and [Snap packaging](docs/snap.md).
- [Pane integration](docs/panes.md), [session storage](docs/sessions.md),
  [preference storage](docs/preferences.md) and [shortcut wiring](docs/shortcuts.md).
- [Plugin host and confinement](docs/plugins.md),
  [protocol implementation](docs/plugin-protocol.md),
  [managed notification backend](docs/plugin-notification-backend.md),
  [terminal bells](docs/terminal-bell-backend.md),
  [command execution](docs/notification-commands.md),
  [effect authority](docs/notification-effect-authority.md) and [audio](docs/audio.md).
- [Testing](docs/testing.md), [recording provenance](docs/media-recordings.md)
  and [releasing](docs/releasing.md).
- [Licensing obligations](docs/licensing.md), [native payload audit](docs/native-payload-audit.md)
  and [source delivery](docs/source-delivery.md).

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

- Comments explain why the code has its particular constraints. If a line needs a comment to say
  what it does, it needs a better name or a simpler body. Keep a comment when the
  reason is not visible from the code: a constraint, a trade-off, a behaviour
  that would otherwise look like a mistake.
- The model holds no widget types. `src/tree.rs` and `src/workspace.rs` are
  data. Anything that needs a `gtk::Widget` belongs in `src/app.rs`. This is what
  lets the layout be tested without a display.
- Comments and documentation are in English.
- No panics on a user's input. The runtime code contains no `unwrap`,
  `expect`, or `panic!`; a failure in a plugin or a terminal must not take the
  application down.

## Tests

`cargo test` runs the unit tests. Model tests cover the pane tree, workspace set,
size reconciliation and plugin protocol without a display.

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
checks for both packages; the commands above are what it runs. The two
packages are independent: the application never depends on the tooling package,
and tooling tests do not link GTK or need a display. The official plugins
(clock, system-status, pane-pwd) are built, tested and released from
[`Among-Terminal/among-plugins`](https://github.com/Among-Terminal/among-plugins);
this repository neither builds nor ships them.

On X11 with Openbox, private D-Bus, xdotool, xprop and ImageMagick installed,
build the application, tools and the host test executable, then drive real windows
through the Rust tooling:

```sh
cargo build --release --locked
cargo build --release --locked --manifest-path tools/among-tools/Cargo.toml --bin among-tools
tool=tools/among-tools/target/release/among-tools
export AMONG_CHECK_ROOT="$HOME/.cache/among/checks"
mkdir -p -m 700 "$AMONG_CHECK_ROOT"
AMONG_CHECK_SOURCE_SHA="$(git rev-parse HEAD)" cargo test --bin among --locked --no-run --message-format=json > "$AMONG_CHECK_ROOT/package-host-tests.jsonl"
$tool ci-x11 $tool capture-package-plugin-profile . "$AMONG_CHECK_ROOT/package-host-tests.jsonl" package-plugin-profile
$tool ci-x11 $tool check-live-focus target/release/among
$tool ci-x11 $tool check-live-status . target/release/among "$PWD/package-plugin-profile"
$tool ci-x11 $tool check-sessions target/release/among
$tool ci-x11 $tool check-split-directory target/release/among
$tool ci-x11 $tool check-scroll-calibration target/release/among
$tool ci-x11 $tool check-sidebar-rename target/release/among
$tool ci-x11 $tool check-pane-workspace-move target/release/among
cargo build --release --locked --example check-drag-source
cargo build --release --locked --manifest-path tools/among-tools/Cargo.toml --example summary-plugin
$tool ci-x11 $tool check-pane-drag-guards target/release/among target/release/examples/check-drag-source tools/among-tools/target/release/examples/summary-plugin
$tool ci-x11 $tool check-window-controls target/release/among tools/among-tools/target/release/examples/notice-plugin
$tool ci-x11 $tool check-desktop . target/release/among
```

These use private test data and only stop test-owned processes. Their
profile capture requires a clean source checkout, uses the actual public native
Release through the host UI, and refuses missing or mismatched test/source input.
The captured profile is internal renderer-check input, not packaged installation
acceptance or a release asset. Their
evidence stays in your cache directory (`~/.cache/among/checks` by default,
`AMONG_CHECK_ROOT` overrides it); see [where checks keep their evidence](docs/testing.md#where-checks-keep-their-evidence).
Do not run raw
input automation against your personal application. Native-X11 evidence does
not establish Snap/Wayland support.

Project-owned tooling is Rust only: a Python interpreter must not appear in
contributed sources, workflows, packaging, launchers or executable
documentation. Snapcraft's own internal use of Python is an external tool
detail and not a project entry point.

## Pull requests

Pull requests merge by squash only: the whole branch becomes one commit on
`main`, and that commit takes the pull request title. The release tooling reads
those commits to choose the next version, so the title is not decoration.

The title must be a [Conventional Commit](https://www.conventionalcommits.org/):

```text
type(scope): short summary
type!: summary of a breaking change
```

- Allowed types: `feat`, `fix`, `docs`, `test`, `ci`, `build`, `perf`,
  `refactor`, `chore`. The scope in parentheses is optional; `!` before the
  colon marks a breaking change. The scope may be any non-empty text without
  parentheses.
- GitHub's default `Revert "..."` title is not accepted. Write a revert as a
  normal Conventional Commit, for example `fix: revert the faster redraw`; use
  `fix` or `feat` when users are affected, otherwise `chore`.
- The failing check does not print the title (the runner would interpret
  workflow commands inside it); read the title on the pull request page.
- Titles, descriptions and commit messages are written in English.
- The `pr-title` check validates the title and runs again whenever the title is
  edited. Fix a failing check by editing the title; no new commit is needed.

How the title drives the version (while the major version is `0`):

| Type | Effect on the next application version |
|---|---|
| breaking change (`!`) | raises the minor version |
| `feat` | raises the minor version |
| `fix`, `perf` | raises the patch version |
| `docs`, `test`, `ci`, `build`, `refactor`, `chore` | no release by themselves |

## Reporting a plugin fault

Plugins run as separate processes on purpose: a plugin that crashes takes only
itself down. If a plugin can take the application down, that is a bug in the
host. Please report it with the plugin's source and the
application's stderr.

## Releases

Maintainers release through a pull request that the repository's own tooling
prepares; nobody edits a version by hand.

- `among-tools release-prepare .` reads the squash-merged pull request titles
  since the last stable tag, chooses the next version (while the major version is
  `0`: a breaking change or `feat` raises the minor version, `fix` or `perf`
  raises the patch version), rewrites every version location, adds the release to
  `CHANGELOG.md` and prints the title and body of the release pull request.
- Commits whose title is not a Conventional Commit cannot influence the
  version. The command lists them and fails until a maintainer names each one
  with `--accept-untitled`, so nothing is left out silently. The maintainer also
  adds the hand-written highlights `.github/release-notes/vX.Y.Z.md`; the checks
  fail without it.
- A maintainer opens that pull request with their own credentials and merges it
  by hand. Merging only validates the release commit and creates no tag. A
  release candidate `vX.Y.Z-rc.N` is tagged by a manual run of the Release tag
  workflow, which starts its prerelease draft; the stable tag is made by another
  manual run, only on a commit that already has an accepted candidate.
  Publishing a draft is a separate, manual decision.
- `among-tools check-release-metadata .` runs in CI and fails when a version
  location is missing or differs, so a hand edit of one location is caught.
- Release candidates, the version rules and the release highlights file are in
  the [releasing guide](docs/releasing.md#4a-the-release-cycle).
- Tooling stays Rust only: the release flow uses `among-tools`, `gh` and shell
  steps in workflows, never a Python interpreter.
