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
| Open preferences | `Ctrl+,` | plugins, notifications, keyboard, terminal appearance and About |
| Toggle fullscreen | `F11` | returns to the previous size |
| Toggle pane zoom | `Ctrl+Shift+x` | fills the workspace; again restores |
| Next workspace | `Ctrl+PageDown` | wraps around |
| Previous workspace | `Ctrl+PageUp` | wraps around |

Print the built-in defaults to stdout from a new application process, then exit:

```sh
AMONG_LIST_BINDINGS=1 among
```

`Ctrl+Shift+Arrow` resizes dividers; `Alt+Arrow` moves focus. Desktop shortcuts can
intercept a binding before Among receives it. Check your desktop's bindings when
choosing replacements, especially combinations using `Super` or `Ctrl+Alt`.

## Splitting

There is one split binding, not separate horizontal and vertical ones. The
orientation comes from the shape of the pane being split:

- width ≥ height → a **vertical** divider, so the two panes sit side by side
- height > width → a **horizontal** divider, so the two panes are stacked

The new pane receives focus, so typing continues where the user last was looking.

## Running an action without a key

```sh
AMONG_ACTION=split_pane among
```

Accepted names include `new-workspace`, `split-pane`/`split`, `close-pane`/`close`,
`focus-up`, `focus-down`, `focus-left`, `focus-right`, `resize-up`, `resize-down`,
`resize-left`, `resize-right`, `next-workspace`, `previous-workspace`,
`workspace-N` (a positive position), `preferences`/`open-preferences`,
`fullscreen`/`toggle-fullscreen`, and `zoom`/`toggle-zoom`.
Hyphens and underscores are interchangeable; names are case-insensitive.

This startup setting does not forward a new action to an already running
instance. An unknown name is reported:

```sh
AMONG_ACTION=fly-away among
# among: unknown action name in AMONG_ACTION
```

An action whose binding has been removed reports that it has no key binding and
does not run through this startup setting.

## Rebinding

Overrides are read by a new application process at startup:

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

`Ctrl+,` opens Preferences with Plugins, Notifications, Keyboard, Terminal and
About pages. See [Preferences](preferences.md) for the other controls.

**Keyboard** lists every action the application can perform, including ones with
no binding — a list of only the bound actions would hide half of what can be
configured. Select a row to capture a new combination; select it again to cancel,
or press <kbd>Escape</kbd> to remove the binding. A rebind applies immediately,
without restarting.

Changing a binding does not check your desktop's configured shortcuts. A
combination intercepted by the desktop will not reach Among.

The [Plugins page](plugins.md) handles installation, access review and plugin
state separately from keyboard bindings.

## How shortcuts are wired

Actions are registered as `gio::SimpleAction`s on the window and the keys are
attached with `GtkApplication::set_accels_for_action`, under the `win.` prefix.

Unclaimed sequences still reach the terminal: GTK only consumes what a registered
accelerator matches.

## Global shortcuts

These bindings work while Among has focus. Among does not register desktop-wide
shortcuts. Use your desktop's keyboard settings if you want a shortcut to launch
the application from another window.

## Unrecognised keys

Anything the application does not claim is delivered to the focused terminal
unchanged. GTK handles the registered window action's accelerator before the
focused widget, so a claimed sequence never reaches the terminal while everything
else falls through.

This matters for a terminal: an application shortcut that leaked into the shell
would insert stray text into a running command.
