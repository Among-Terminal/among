# Preferences storage

The storage module uses `$XDG_CONFIG_HOME/among`, falling back to
`$HOME/.config/among` when XDG_CONFIG_HOME is absent or empty.

Plugin choices keep the existing `plugins.json` format:

```json
{"plugins": [{"name": "clock", "enabled": false}]}
```

Shortcut overrides use `shortcuts.json`, version 1:

```json
{"version": 1, "overrides": {"split-pane": "<Control><Shift>b", "toggle-zoom": null}}
```

An absent override inherits the current default; null explicitly removes a
binding.

The expanded sidebar's width uses `window.json`, version 1:

```json
{"version": 1, "sidebar_width": 420}
```

Drag the expanded sidebar's right edge to resize it. The width is measured in
logical pixels and saved when the drag finishes, without a Save button. It returns
after collapse/expand and after restarting, even when you start a new terminal
session. The collapsed strip stays fixed-width. A smaller window temporarily
constrains the displayed width without replacing the saved preference. Without a
saved width, the existing adaptive default is used; zero, negative and non-numeric
widths are rejected.

Unknown fields and entries are retained when saving other preferences.
Malformed files and unsupported versions are not automatically overwritten.

Writes use a private temporary file in the destination directory and atomic
replacement. Failures before replacement preserve the prior document; failures
confirming directory durability after replacement report uncertainty. Symlink
destinations are refused. Independent external edits are merged when detected;
simultaneous writes by external editors are not fully serialized.

## Changing preferences

Plugin switches and shortcut edits apply immediately and are saved without a Save
button. Plugin-specific forms instead require an explicit Save after validation;
closing without Save discards unfinished edits. Fresh launches restore plugin
choices and shortcut overrides.
Reassigning an occupied shortcut also saves the displaced action as unbound.
Cancelled capture and rejected/no-op edits do not write new overrides.

A failed write leaves the edit active for this session and shows a warning in
Preferences and stderr, without a pop-up. A later edit in the same settings
category retries pending changes; successful saving clears that category's warning.
Repair malformed files or directory permissions before retrying. Startup errors
are visible too.
Plugin faults do not change the saved desired enabled state.

Plugin forms show their own persistence result: **Saved locally**, **Accepted for
this session; not saved locally**, or **Written locally; durability not confirmed**.
That result is independent of pending/applied/rejected/restart-required process
configuration. A plugin restart requires its own confirmation; opening a form or
saving a restart-mode edit never restarts code automatically.

Clock, resources and pane PWD remain optional and disabled by default. Only explicit
user changes enable them. No terminal contents or workspace layouts are saved here.

Developer checks are described in [the contributing guide](../CONTRIBUTING.md#checking-changes).

## Plugin inventory navigation

The Plugins page has a labelled native search field. Match by name, description,
identity or origin; clear it to return to the full installed/configured list.
Equal names show separate origins, missing configured files stay inspectable,
and a desired-on unavailable entry remains switchable off. Details opens a native
dialog; Settings appears only for a declared settings schema. Use Tab/Shift+Tab
to move between search, actions and switches, Space to operate a switch, and the
dialog's dismiss action to return. The form's Save boundary is separate from
runtime applied/pending/rejected/restart-required acknowledgement.

Background registration or process-state changes update rows by identity rather
than rebuilding focused controls every tick. Opening/searching these views does
not enable code or save defaults. Repository/install operations are separate from
this inventory; no download or uninstall is implied by a static card.
