# Pane integration

User controls are documented in [panes and workspaces](https://among-terminal.github.io/panes/).

## Layout and process ownership

Keep terminal instances in the pane map when `rebuild_panes` in
[app.rs](../src/app.rs) reparents widgets. Rebuilding a container must not respawn
the shell or discard independently managed header content in
[pane.rs](../src/pane.rs).

`WorkspaceSet::move_pane_between` in [workspace.rs](../src/workspace.rs) prepares
the destination and surviving source on copies before committing either.
Preserve this refusal boundary for stale drag targets, and resolve the destination
by its stable reference after removing an empty source workspace.

Directory inheritance during splitting must use the source pane's known cwd;
spawn confirmation and later OSC 7 updates follow the
[session storage contract](sessions.md#saving-and-restoring).

## Header and expansion invariants

The host owns the permanent `#N` identity label and workspace terminal counts.
Programs set the terminal title through OSC escapes. Plugin content shares the
header with that title and cannot replace it.
An empty plugin footer is hidden. An absent title displays `Terminal N`.
Title-change markers and bell unread state have separate lifecycles.

Expansion changes rendering without changing the layout tree or restarting
terminals. Restore reconstructs the same nested divider positions. Directional
focus stays on the expanded pane, and splitting is refused until restoration.
Other workspaces keep their expansion state independently.

The [expansion controls](https://among-terminal.github.io/panes/#expanding-a-pane)
define Restore button visibility, header geometry and focus behavior.
Restoring a hidden sibling for desktop notification activation must
not acknowledge other terminals' indicators. An activation for a closed terminal
or previous process instance only raises the window.

## Menu and link ownership

Keep the pane and URI captured by [context_menu.rs](../src/context_menu.rs)
bound to one invocation. Invalidate that invocation before layout changes in
[app.rs](../src/app.rs), so stale callbacks cannot act on a replacement pane.
Menu actions must call shared pane operations independently of accelerator
bindings and delegate clipboard and selection operations to VTE.

[links.rs](../src/links.rs) owns URI parsing and launch eligibility for both the
menu and Ctrl+click. Preserve explicit-hyperlink precedence and the scheme,
length and control-character checks in `Link::parse`. Keep desktop dispatch
asynchronous through `GtkUriLauncher`; never pass a terminal-supplied URI to a
shell.

## Scroll calibration

The application leaves mouse-wheel amounts to VTE, about a tenth of the pane's
rows per click. Its touchpad controller
converts surface distance to text rows independently of pane height; VTE's
unadjusted surface handling scaled the distance with the pane's rows. Keep
wheel input and terminal-owned full-screen scrolling outside this correction.
Each pane owns its scroll position. See `src/pane.rs`, `surface_scroll_lines`
and `scroll_terminal`, and the scroll-calibration checks in [testing](testing.md).

## Interface diagnostics

Actions and plugin notifications do not create bottom pop-up messages.
Diagnostics go to stderr; Settings also reports storage warnings. Explicit
plugin header, footer and sidebar content remains independent of these messages.
