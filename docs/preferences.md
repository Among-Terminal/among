# Preferences storage

The storage module uses `$XDG_CONFIG_HOME/among`, falling back to
`$HOME/.config/among` when XDG_CONFIG_HOME is absent or empty.

## General notifications

General is the first and initially visible page, followed by Keyboard and Plugins.
Page/control automation uses identities (`general`, `keyboard`, `plugins` and
`notification-…` widget names), not the first tab or first switch.

Notification choices use `general.json`, version 1. Missing fields inherit these
in-memory defaults; opening Preferences never writes this document:

```json
{"version":1,"notifications":{"visual":{"enabled":true,"eligibility":"originating-terminal-unfocused"},"audio":{"enabled":false,"eligibility":"application-background","source":"builtin"},"command":{"enabled":false,"eligibility":"application-background"}}}
```

Each channel has independent Always, Originating terminal unfocused and Among in
background conditions. Preferences and native menus count as foreground Among,
but not as viewing a terminal. Visual alerts retain a static `!` and accessible
unread names; system animation changes stop stripe motion without acknowledging
unread terminals. Workspace hue and terminal allocation are not notification state.

Policy edits apply immediately. The command's absolute executable and literal
arguments (a JSON string array) require **Save command**. Empty executable clears
configuration and disables the channel. Saving or enabling does not run a command
or prepare its cwd; an eligible new event triggers bounded off-loop preparation.
See [the command contract](notification-commands.md) for authority and cleanup limits.

**Use local WAV** reads and validates asynchronously; only an accepted immutable
snapshot changes the selected sound and saves its source. Invalid selection does
not play a fallback or overwrite the last accepted selection. A missing/invalid
saved local source disables playback until a source is accepted. **Use built-in
WAV** selects the original bundled sound. **Preview** explicitly plays the accepted
snapshot even with event audio off, without entering notification ingress or
changing policy/unread/command/provider state. Playback requests and decoder end
are not proof of output. See [audio limits and packaging](audio.md).

Provider choices are keyed by full provider identity and channel ID under
`notifications.providers[identity].channels[channel]`. Retained unavailable choices
remain editable and are labelled unavailable, not delivered. A generic registry
supplies typed policy/admission ownership; managed producer/provider attachment
still requires its separate committed API integration. New channels default off.

General edits re-read and merge the relevant fields, preserving unknown keys and
other provider identities. Bounded unique-key JSON and safe owner-controlled paths
are required; fresh files are private, and safe existing directory permissions
are not silently changed. Future versions, invalid known fields, symlinks and malformed documents are reported,
not repaired. Settings are outside packaged application trees. No transient
attention, operation IDs or pending delivery jobs are persisted.

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

Preferences and the repository inspector are nonmodal: switch back to a terminal
without closing them while a repository request is running. Their parented native
window layout is unchanged. Other forms and confirmation dialogs retain their own
interaction rules.

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
Repository inspection belongs to its own Plugin sources section, above and
separate from search. Inspection only previews metadata; a separate Install disabled
action selects one compatible ready-built version without enabling it. Existing
selected identities are refused rather than overwritten. Add source explicitly
saves discovery metadata; Cached reports its retrieval time/offline/stale state,
Refresh resolves the numeric registered repository, and Remove source affects
discovery only. Publisher transfer requires explicit renewed source approval and
cannot grant installed code new runtime access. Updates and uninstall remain
unfinished.
Equal names show separate origins, missing configured files stay inspectable,
and a desired-on unavailable entry remains switchable off. Details opens a native
view; Settings appears only for a declared settings schema. The form's Save action
is above its fields; persistence feedback and runtime status have their own group.
Use Tab/Shift+Tab to move between search, actions and switches, Space to operate
a switch, and the view's close action or Escape to return. The form's Save boundary is separate from
runtime applied/pending/rejected/restart-required acknowledgement.

Background registration or process-state changes update rows by identity rather
than rebuilding focused controls every tick. Opening/searching these views does
not enable code or save defaults. Repository/install operations are separate from
this inventory; no download or uninstall is implied by a static card.
