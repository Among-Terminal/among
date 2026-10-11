# Among Terminal

<p align="center">
  <video src="docs/media/among-terminal-action-v5.mp4"
         poster="docs/media/among-terminal-action-v5.png"
         width="880" autoplay loop muted playsinline
         title="Among Terminal: panes, workspaces, themes and plugins"></video>
</p>

<p align="center"><sub>23 seconds · <a href="docs/media/among-terminal-action-v5.mp4">download</a> · <a href="https://among-terminal.github.io/">documentation</a></sub></p>

A terminal multiplexer for GNOME: split panes, workspaces in a collapsible
sidebar, and a pluggable pipeline that turns shell data into interface content.

The binary is `among`. Documentation is at
[among-terminal.github.io](https://among-terminal.github.io).

Terminal emulation is delegated to VTE (`libvte-2.91-gtk4`) through the official
GNOME World `vte4` crate — the same widget GNOME Terminal is built on. This
project owns the pane tree, workspace lifecycle, key routing, and the plugin
pipeline.

## Features

- **Split panes** in any nesting: split the focused pane side by side or top to
  bottom. The orientation is chosen from the pane's own shape, so a wide pane
  divides across and a tall one divides down.
- **Resizable dividers.** Drag a divider, or move it with the keyboard.
- **Workspaces**, each with a name you can rename in place and a colour of its own.
  The sidebar collapses to a narrow strip that still shows every workspace, so
  none is ever out of reach. Every terminal in every workspace stays running, so
  switching is instantaneous and background processes keep going.
- **Keyboard driven.** Move between panes, workspaces, split, and close without
  touching the pointer.
- **Session restoration.** Save workspace names, colours, nested layouts and
  known directories; choose whether to restore fresh shells on the next launch.
- **Persistent preferences.** Plugin switches and shortcut changes survive
  restarts, including explicitly unbound actions.
- **Plugins** as separate processes that collect shell data and push content into
  the application, workspace, pane, and footer areas. A faulty plugin cannot take
  the application or its terminals down.

## Installation status

Version 0.1.0 is under release preparation. Private CI has exercised installed
DEBs on Debian 13 and Ubuntu 24.04 for amd64 and arm64, and devel-grade core24
Snaps on both architectures. These rehearsals are not stable release acceptance.
Wayland, GPU and Flatpak remain untested; no package is published in the Snap Store.

Install from this source checkout (Rust/Cargo 1.98+ required):

```sh
sh scripts/install.sh --install-deps
```

`--install-deps` installs Debian/Ubuntu native dependencies via sudo; Cargo and
the application install run as your normal user. On other distributions install
the [native prerequisites](docs/build.md), then run `sh scripts/install.sh`.
The installer adds the binary, applications-menu entry, icon, AppStream metadata
and license notices. Rerun it to update; `sh scripts/install.sh --uninstall`
removes its files without deleting settings, sessions or plugins.
See [installation](docs/install.md) for locations and prebuilt-binary installs.

### Cargo installation from GitHub (binary only)

After the public source repository is published, with Rust 1.98+ and the
[native build prerequisites](docs/build.md) installed:

```sh
cargo install --git https://github.com/Among-Terminal/among --locked among
among
```

This builds the repository's default branch and installs only the executable
into Cargo's bin directory (normally `~/.cargo/bin`, which must be in PATH).
It does **not** register a desktop entry, icon or AppStream metadata, install
plugins, or provide a `.deb` package. Use the source installer above for desktop
integration. `publish = false` prevents crates.io publication, not Git installation.
No published stable tag is assumed here; once one exists, `--tag vX.Y.Z` selects
that version instead of the moving default branch.

Rerun the Cargo command to update. Remove this installation with
`cargo uninstall among`; settings, sessions and external plugins are separate.
Do not use sudo for Cargo installation. Cargo and system/source installations
can coexist, but PATH chooses the binary; `/usr/bin/among` explicitly selects
the system package. This Git installation command has not yet been exercised
against the remote repository during release preparation.

Optional plugins are built separately and disabled by default:
`sh scripts/install-plugins.sh --enable-new` builds, backs up selected installed
copies and registers missing disabled entries. Enable wanted plugins in Settings.
The helper never closes terminals or edits shell startup files.

## Documentation

[Overview and full-length videos](docs/overview.md) ·
[Feature guide with demonstrations](docs/features.md) ·

If GitHub does not play an MP4 inline, open or download the linked video.
Controls are documented in
[panes](docs/panes.md), [sessions](docs/sessions.md),
[preferences](docs/preferences.md) and [plugins](docs/plugins.md).

## License

Original project code and examples are [Apache-2.0](LICENSE). Third-party Rust
and native libraries retain their own licenses; see [licensing](docs/licensing.md),
[NOTICE](NOTICE) and [dependency notices](THIRD_PARTY/inventory.json).

## Layout of the source

```
src/main.rs       startup: application, window, process spawning
src/app.rs        the window, and the projection of model into widgets
src/workspace.rs  the workspace list: names, colours, and which one is active
src/tree.rs       pane layout as plain data, no widgets, and the drop geometry
src/size.rs       pixels, cells, and ratios, reconciled in one place
src/pane.rs       a terminal plus the interface around it
src/slots.rs      declarative fragments and the slots they appear in
src/shortcuts.rs  the binding registry
src/prefs.rs      the preferences window
src/plugin/       the versioned protocol and the out-of-process host
data/among.css    application styles
docs/panes.md     pane behaviour
docs/plugins.md   the plugin protocol
docs/shortcuts.md the binding list and how it is wired
docs/build.md     building against the GTK 4 generation of the stack
examples/         working plugins
```

The model is deliberately free of widget types: `src/tree.rs` and
`src/workspace.rs` hold layout and identity as data, and `src/app.rs` is the only
place that turns them into widgets. That split is what lets the layout be tested
without a display.

## Building

### System packages

```sh
sudo apt install libgtk-4-dev libvte-2.91-gtk4-dev libadwaita-1-dev
```

`libvte-2.91-dev` is **not** the package you need. It installs the GTK 3 build of
VTE; the bindings used here are generated against the GTK 4 build, whose
pkg-config name is `vte-2.91-gtk4`.

Verify what is installed:

```sh
pkg-config --exists gtk4 libadwaita-1 vte-2.91-gtk4 && echo ok
```

### Build

```sh
cargo build --release --locked
cargo test --locked
```

The example plugins are a separate Cargo package. To build/install them with
backups and merge missing configuration entries, run
`sh scripts/install-plugins.sh --enable-new`; see [plugin installation](docs/plugins.md#installing).
Rebuilding the application alone neither upgrades installed plugins nor restarts
an already running application instance.

Two dependency details are not discoverable from the `vte4` README and will
otherwise cost you a failed build:

- `gtk4` must be built with the `v4_10` feature. `vte4`'s generated code
  references `gtk::Accessible`, which sits behind that feature; without it the
  dependency tree does not compile.
- `glib` and `gio` must be direct dependencies. `vte4` does not re-export them,
  and `PtyFlags`, `SpawnFlags`, and `Cancellable` need them.

The gtk-rs crates move together: `libadwaita 0.7` pulls `gtk4-sys 0.9` while
`gtk4 0.11` needs `gtk4-sys 0.11`, and two crates cannot both declare
`links = "gtk-4"`. Bump one and you must bump the other.

## Recommended shell setup

Workspace names, pane layouts and known directories can be restored on startup;
see [session saving and restoration](docs/sessions.md) for choices and limits.

We recommend using Among Terminal with **zsh + [Starship](https://starship.rs/)**
for a customizable prompt. Both are optional: the application uses your configured
shell and does not install or modify shell configuration. Prompt customization
does not by itself enable the OSC 7 directory reporting required by `pane-pwd`;
see [shell directory integration](docs/plugins.md#live-status-examples).

## Keyboard shortcuts

| Action | Binding |
| --- | --- |
| Focus up | `Alt+Up` |
| Focus down | `Alt+Down` |
| Focus left | `Alt+Left` |
| Focus right | `Alt+Right` |
| Activate workspace 1 | `Alt+1` |
| Activate workspace 2 | `Alt+2` |
| Activate workspace 3 | `Alt+3` |
| Activate workspace 4 | `Alt+4` |
| Activate workspace 5 | `Alt+5` |
| Activate workspace 6 | `Alt+6` |
| Activate workspace 7 | `Alt+7` |
| Activate workspace 8 | `Alt+8` |
| Activate workspace 9 | `Alt+9` |
| New workspace | `Ctrl+Shift+t` |
| Split pane | `Ctrl+Shift+n` |
| Grow pane | `Ctrl+Shift+Right` |
| Shrink pane | `Ctrl+Shift+Left` |
| Close pane | `Ctrl+Shift+w` |
| Open preferences | `Ctrl+,` |
| Toggle fullscreen | `F11` |
| Toggle pane zoom | `Ctrl+Shift+x` |
| Next workspace | `Ctrl+PageDown` |
| Previous workspace | `Ctrl+PageUp` |

None of these use `Super` or `Ctrl+Alt`, which GNOME reserves for its own window
and workspace management; binding them would make the action silently never fire.

Divider resizing and workspace cycling are beyond the five bindings originally
asked for. They are here because the specification requires a keyboard action for
both — resizing without a pointer, and moving between workspaces — and a
requirement cannot be met by an action nobody can reach. Both are rebindable and
can be removed by rebinding them to nothing.

Divider resizing uses `Ctrl+Shift+Arrow` rather than `Alt+Arrow` so it sits at a
different modifier level from focus movement; conflating the two would make the
layout hard to adjust by feel.

Actions can also be run without a key press, which is how an action that has no
binding becomes reportable rather than unreachable:

```sh
AMONG_ACTION=split_pane among
```

`AMONG_ACTION=fly-away` reports an unknown action name.

### Rebinding

Bindings live in one registry, so a changed binding applies without a restart.
At startup they can be overridden from the environment, where `~` is not special:

```sh
AMONG_REBIND="split-pane=<Control><Shift>d" among
```

An unusable accelerator or an unknown action name is reported and ignored rather
than accepted.

## Closing panes

Closing cascades upward, because a pane is a terminal window and the application
is what surrounds it:

```
close pane      -> the pane and its process go, the layout closes up
  last pane     -> the workspace is closed too
    last one    -> a confirmation appears before the application exits
```

The cascade stops at the last workspace. The confirmation names how many
terminals are still running, because closing one idle terminal and closing three
panes running a deployment are not the same decision.

## Workspaces

Every terminal in every workspace stays running, so switching is instantaneous
and background processes keep going.

Each workspace has a **name** and a **colour**. Double-click a workspace's entry
in the sidebar to rename it in place — `Enter` applies, `Escape` puts the old
name back, and an empty name is refused so a workspace cannot silently lose its
identity. Renaming touches nothing else: the panes, their processes, and the
layout are exactly as they were.

### Collapsing to the strip

The sidebar's toggle switches between the full list and a narrow strip. It does
not hide the list: every workspace stays reachable, which is the point of
collapsing it. Each strip entry shows a terminal glyph, that workspace's terminal
count, and its colour:

```
|▌ $  1 |   the active workspace: one terminal
|▌ $  4 |   another: four terminals, still running
```

The count comes from the pane tree, not from a plugin, so it is right whether or
not any plugin is enabled. It follows the layout as panes are split and closed,
and a workspace disappears from the strip when its last terminal does.

### Colour

A workspace's colour comes from a fixed palette of eight and appears in three
places at once, so a workspace is recognisable without reading its name:

- a stripe in its sidebar entry,
- the same stripe on the header of each of its panes,
- and its entry in the collapsed strip.

The colours are applied as markers, never as a tint behind terminal output.
Terminal programs use colour to mean something, and a colour behind their text
would both reduce legibility and read as application state.

A new workspace takes the colour used by the fewest open workspaces, so colours
stay distinct as long as the palette has room; past eight, colours are reused
evenly rather than the workspace being refused or left uncoloured. A colour
belongs to the workspace for as long as it exists — renaming it, switching away
and back, or rearranging its panes leaves it unchanged — and closing a workspace
returns its colour for the next one.

Names, colours, focus, layouts and known directories are checkpointed to the
session snapshot and can be restored into fresh shells after explicit startup
consent. Commands, processes and scrollback are not resumed. See
[sessions](docs/sessions.md) for storage/error handling and crash limits.

## Plugins

See [docs/plugins.md](docs/plugins.md) for the protocol, and
[examples/plugins/src/bin/clock.rs](examples/plugins/src/bin/clock.rs) for
a compiled example. Build and installation instructions are in [docs/plugins.md](docs/plugins.md).

## Verification aids

The application reads a handful of environment variables that exist only for
automated runs. None of them has any effect in normal use, and each is documented
here because an undocumented switch is indistinguishable from a broken one.

| Variable | Effect |
| --- | --- |
| `AMONG_SELFCHECK` | Prints the live widget state, plugin states, and slot contents to stderr, once a second. |
| `AMONG_SELFCHECK_TICKS` | How many reports to print before stopping them. Default `120`. |
| `AMONG_SCRIPT` | A file of actions to run at startup, one per line. The application keeps running afterwards. |
| `AMONG_ACTION` | A single action to run at startup instead of a script file. |
| `AMONG_REBIND` | Rebinds keys before startup, as a comma-separated `action=accel` list. |
| `AMONG_ENABLE` / `AMONG_DISABLE` | Comma-separated plugin names to switch on or off for this run. |
| `AMONG_COLOR_SCHEME` | `light` or `dark`, to force the scheme instead of following the platform. |
| `AMONG_LIST_BINDINGS` | Prints every action and its binding, then exits. No window is shown. |

`AMONG_LIST_BINDINGS` is the reason the shortcut table above can be generated
rather than maintained by hand:

```sh
AMONG_LIST_BINDINGS=1 among
```

### Script actions

Each line of `AMONG_SCRIPT` is one action. Anything that is a key binding can be
named, using the names in the table above with `_` in place of spaces —
`split_pane`, `close_pane`, `new_workspace`, `focus_left`, `resize_grow`,
`zoom`, `fullscreen`, `preferences`, `next_workspace`. Two families take an
argument:

- `workspace_N` — activate workspace *N*, counting from 1.
- `type:<command>` — type into the focused terminal; `~` stands for a space.
  `feed:<pane>:<command>` does the same for a numbered pane, which is how a
  title is set on a pane that is not focused.

Three further steps exist for states that have no key binding:
`collapse-sidebar` and `expand-sidebar` switch the sidebar between the list and
the strip, and `request-close` does what the window's close control does.

### Driving the application from outside

GTK delivers accelerators only while the window holds focus. The executed live
tests use an isolated native-X11 session and explicitly activate its test window:

```sh
GDK_BACKEND=x11 AMONG_SELFCHECK=1 AMONG_SCRIPT=steps.txt among &
# once the window exists, give it focus:
xdotool search --name 'Among Terminal' | while read -r id; do xdotool windowfocus "$id"; done
xdotool key ctrl+shift+n
```

`GDK_BACKEND=x11` is what makes the window reachable through X at all; the
`AMONG_*` variables report state and script the actions; `xdotool` supplies the
real key events. This is the combination the project's own test scripts use.

## The window

The window opens **maximized**, not fullscreen. `F11` toggles fullscreen;
leaving it restores the previous window state. The standard window controls — close,
minimise, restore — are present whenever the window is not fullscreen, and
closing from them takes the same path as closing the last workspace, including
the confirmation.

## Panes

What a pane looks like, how a pane is dragged onto another pane, how pane headers
follow the terminal's title, what happens when a pane's process exits, and how a
pane is expanded are described in [docs/panes.md](docs/panes.md).

## Preferences

`Ctrl+,` opens a window with the keyboard shortcuts and the plugin list. Shortcuts
can be rebound by selecting a row and pressing a combination, or removed with
<kbd>Escape</kbd>; the change applies without a restart. Plugins can be started and
stopped, and stopping one removes its content from the interface immediately.

See [docs/shortcuts.md](docs/shortcuts.md) for details.

## Not included

Named layout presets, pane groups, broadcast input to all panes,
detaching a pane into its own window, distribution packaging, and a GNOME Shell
extension. Public packaging and the reviewed demo are still in preparation.

## Project status

Version 0.1.0. Native-X11 behavior has executed evidence; release tooling is being
prepared locally, with no publication yet. Crash reporting is not implemented.
No compatibility or untested-platform support is promised. See
[Snap preparation](docs/snap.md) for remaining distribution gates.
The [first-publication guide](docs/releasing.md) separates local checks from
owner-only account, approval and publication steps.

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).
