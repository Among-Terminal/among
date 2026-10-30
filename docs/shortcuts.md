# Keyboard shortcuts

The header's left controls are Settings, sidebar toggle, then New workspace.
`Ctrl+Shift+T` (or your configured replacement) also creates a workspace.

Edits in Preferences are saved immediately and restored on the next launch,
including removed bindings and actions displaced by a shortcut reassignment.
Storage is `$XDG_CONFIG_HOME/among/shortcuts.json` (or
`~/.config/among/shortcuts.json`); see [Preferences storage](preferences.md).
If saving fails, the edit remains session-only and Preferences shows a warning.

Bindings live in a single registry (`src/shortcuts.rs`), which is what makes them
inspectable and changeable without touching the code that uses them.

## Workspace order

Drag a workspace entry onto the upper or lower half of another entry to insert it
before or after that workspace. The insertion edge is highlighted before the drop.
This works in both the expanded sidebar and the collapsed strip; holding the drag
near a scrollable sidebar edge reveals entries beyond the viewport. Escape or a
drop outside the sidebar leaves the order unchanged.

Alt+1 through Alt+9 select the current first nine positions, not the original
creation order. Next/previous workspace and custom bindings for positional actions
follow that same order immediately. There is no Alt+0 default for position ten.
Moving an entry does not activate it or restart its terminals. Single-click still
switches workspaces, and double-click still edits an expanded entry's name.

Dragging a pane header onto an entry is a different gesture: it moves that live
terminal into the workspace and activates the destination. It never reorders the
workspace list.

## Defaults

| Action | Binding | Notes |
| --- | --- | --- |
| Focus up | `Alt+Up` | no change without a neighbour or while a pane is expanded |
| Focus down | `Alt+Down` | no change without a neighbour or while a pane is expanded |
| Focus left | `Alt+Left` | no change without a neighbour or while a pane is expanded |
| Focus right | `Alt+Right` | no change without a neighbour or while a pane is expanded |
| Activate workspace 1 | `Alt+1` | beyond the last workspace: no change |
| Activate workspace 2 | `Alt+2` |  |
| Activate workspace 3 | `Alt+3` |  |
| Activate workspace 4 | `Alt+4` |  |
| Activate workspace 5 | `Alt+5` |  |
| Activate workspace 6 | `Alt+6` |  |
| Activate workspace 7 | `Alt+7` |  |
| Activate workspace 8 | `Alt+8` |  |
| Activate workspace 9 | `Alt+9` |  |
| New workspace | `Ctrl+Shift+t` | becomes the active workspace |
| Split pane | `Ctrl+Shift+n` | orientation chosen from the pane's shape; refused while a pane is expanded |
| Move divider up | `Ctrl+Shift+Up` | nearest stacked divider above the pane; one step |
| Move divider down | `Ctrl+Shift+Down` | nearest stacked divider above the pane; one step |
| Move divider left | `Ctrl+Shift+Left` | nearest side-by-side divider above the pane; one step |
| Move divider right | `Ctrl+Shift+Right` | nearest side-by-side divider above the pane; one step |
| Close pane | `Ctrl+Shift+w` | cascades to the workspace, then to exit |
| Open preferences | `Ctrl+,` | shortcuts and plugins |
| Toggle fullscreen | `F11` | returns to the previous size |
| Toggle pane zoom | `Ctrl+Shift+x` | fills the workspace; again restores |
| Next workspace | `Ctrl+PageDown` | wraps around |
| Previous workspace | `Ctrl+PageUp` | wraps around |

This list is generated from the running registry, so it cannot drift from what
the application installs:

```sh
AMONG_LIST_BINDINGS=1 among
```

Several of these are additions beyond the five originally requested. They exist
because the specification requires a keyboard path to each behaviour, and a
required behaviour with no way to reach it is not implemented: divider resizing,
workspace cycling, preferences, fullscreen, and pane expansion.
`Ctrl+Shift+Arrow` is used for resizing so it sits a modifier level away from
`Alt+Arrow` focus movement — adjusting a divider and choosing a direction need
to be distinguishable by touch.

### Why these and not others

The obvious alternative for pane focus is `Ctrl+Shift+Arrow`, as used by several
terminal emulators. `Alt+Arrow` was chosen because it does not collide with
anything on a stock GNOME desktop.

Verified against the live settings schema on the target machine:

- GNOME claims `Ctrl+Alt+Arrow` for switching **its own** workspaces, `Ctrl+Alt+Tab`
  for panels, `Alt+Tab` for applications, and every `Super` sequence for the
  overview.
- None of the defaults above touches those.
- `Ctrl+Shift+T/N/W` are unclaimed by the desktop, and unclaimed by readline and
  zsh.
- `Alt+Arrow` is not bound by the desktop or by GNOME Terminal's own profile.

Binding a sequence the desktop also handles would make the action silently never
fire, which looks like a broken application rather than a conflict. The registry
reports such bindings instead of accepting them (`is_reserved_by_desktop`).

## Splitting

There is one split binding, not separate horizontal and vertical ones. The
orientation comes from the shape of the pane being split:

- width ≥ height → a **vertical** divider, so the two panes sit side by side
- height > width → a **horizontal** divider, so the two panes are stacked

Comparing the two extents directly, rather than their ratio, matches the
intuition that a wide area divides better across. This is the same behaviour as
Terminator's `split_auto`.

The new pane receives focus, so typing continues where the user last was looking.

## Running an action without a key

```sh
AMONG_ACTION=split_pane among
```

Accepted names: `new-workspace`, `split-pane`/`split`, `close-pane`/`close`,
`focus-up`, `focus-down`, `focus-left`, `focus-right`, `next-workspace`,
`previous-workspace`, `workspace-N`.

The environment is used rather than a command-line flag because GApplication
parses and rejects any option it does not recognise, which makes a custom flag
unusable.

Invoking an action that has no binding reports it instead of doing nothing
silently:

```sh
AMONG_ACTION=fly-away among
# among: unknown action name in AMONG_ACTION
```

## Rebinding

Overrides are read at startup and applied to the running process:

```sh
AMONG_REBIND="split-pane=<Control><Shift>d" among
```

Several entries are separated by commas. An unusable accelerator or an unknown
action name is reported and ignored rather than accepted:

```sh
AMONG_REBIND="split-pane=<<bad" among
# among: `<<bad` is not a usable accelerator
```

Binding a sequence that another action already holds transfers it, so the mapping
from sequence to action stays a function — the previous holder becomes unbound.

## Preferences

`Ctrl+,` opens a preferences window with two pages.

**Keyboard** lists every action the application can perform, including ones with
no binding — a list of only the bound actions would hide half of what can be
configured. Select a row to capture a new combination; select it again to cancel,
or press <kbd>Escape</kbd> to remove the binding. A rebind applies immediately,
without restarting.

A sequence the desktop handles first is refused rather than stored, because the
action would then never fire — the same reason the defaults avoid `Super` and
`Ctrl+Alt`.

**Plugins** lists every configured plugin with its state: running, disabled, not
installed, stopped, or speaking an incompatible protocol version. The switch
starts or stops the plugin; stopping one withdraws its content immediately rather
than leaving it frozen on screen. Plugins that are missing or incompatible cannot
be switched on, since there is nothing to start.

## How shortcuts are wired

Actions are registered as `gio::SimpleAction`s on the window and the keys are
attached with `GtkApplication::set_accels_for_action`, under the `win.` prefix.

This is not an arbitrary choice. A `GtkShortcutController` looks like the obvious
tool, but its default `LOCAL` scope is consulted *after* the focused widget — and
a terminal consumes every key it receives. The result is that no application
shortcut fires at all while the terminal has focus, which is the only state that
matters. `GLOBAL` scope fixes the ordering, but the action-and-accelerator route
is the one GTK itself uses for application shortcuts and is what is done here.

Unclaimed sequences still reach the terminal: GTK only consumes what a registered
accelerator matches.

## Global shortcuts

Everything above is *inside* the window, and takes effect only while the
application has focus. A desktop-wide shortcut — one that works while another
application is focused — is a different mechanism: it has to be registered with
the desktop shell.

On GNOME that means a custom keybinding in
`org.gnome.settings-daemon.plugins.media-keys`, whose command invokes the
application. The neighbouring `glagol` project does exactly this in its
`hotkeys.rs`, and that approach is the one to copy if a shortcut has to work from
outside the application.

## Unrecognised keys

Anything the application does not claim is delivered to the focused terminal
unchanged. A shortcut is handled by a `ShortcutController` on the window, and GTK
delivers accelerators before the focused widget, so a claimed sequence never
reaches the terminal while everything else falls through.

This matters for a terminal: an application shortcut that leaked into the shell
would insert stray text into a running command.
