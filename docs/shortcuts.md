# Shortcut integration

The [keyboard guide](https://among-terminal.github.io/shortcuts/) owns default
bindings, reassignment, startup overrides and action names.

Bindings live in the single registry in `src/shortcuts.rs`. Keep that registry
inspectable and editable independently of action implementations.

## How shortcuts are wired

Actions are registered as `gio::SimpleAction`s on the window. Keys are attached
with `GtkApplication::set_accels_for_action`, under the `win.` prefix.

GTK handles a registered window action's accelerator before the focused widget.
A claimed sequence must not also reach the terminal and insert text into a
running command. Unclaimed sequences pass through unchanged. Among registers
no desktop-wide shortcuts.

## Startup diagnostics

`AMONG_ACTION` and `AMONG_REBIND` are startup inputs; they do not forward work to
an existing application process. Keep their diagnostics useful for test harnesses:

```sh
AMONG_ACTION=fly-away among
# among: unknown action name in AMONG_ACTION
```

```sh
AMONG_REBIND="split-pane=<<bad" among
# among: `<<bad` is not a usable accelerator
```

The startup action path requires a binding. An unbound action reports that it
has no key binding and does not run. Reassigning a sequence unbinds its previous
holder so each sequence maps to one action. Preference writes preserve that
explicit unbinding; see [stored settings](preferences.md#stored-settings).
