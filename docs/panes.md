# Pane integration

User controls are documented in [panes and workspaces](https://among-terminal.github.io/panes/).

## Layout and process ownership

A pane move preserves the terminal object, process, scrollback, directory,
plugin content and title-change marker. Reparenting must preserve independently
managed header content. Moving to another workspace changes the header colour
and inserts the terminal after its last pane in layout order. The last pane's
shape determines the split orientation. An empty source workspace is removed.

A pane drag cannot reorder workspaces or activate one on hover. Invalid payloads,
or a source or destination that disappears during the gesture, leave the layout
unchanged. Only the header handles pane movement; the terminal body retains text
selection.

Splitting uses the source pane's known directory without changing its process or
directory. Initial directories and later OSC 7 reports are separate inputs.
An inaccessible inherited directory falls back to an accessible home or startup
directory and is reported. A workspace's first pane keeps the startup directory.
See [session storage](sessions.md) for recording the actual spawn directory.

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

The context menu captures its originating pane and link target. Copy Link uses
that captured URI even after the pointer moves. Keyboard invocation has no link
target. Copy, Paste and Select All delegate to VTE; Select All includes blank
terminal rows, and Copy preserves the resulting text. Symbolic icons accompany
text labels. Availability changes while the menu is open.

Explicit hyperlinks take precedence over displayed URLs. Only absolute http,
https and mailto targets can launch. Other valid explicit schemes are copy-only;
malformed, control-containing or oversized targets are refused. Desktop handlers
are requested asynchronously without shell execution. A refused menu launch
reports an error while the terminal remains usable; a refused Ctrl+click launch
reports on stderr without a dialog. The menu and shortcut paths share pane
operations, but unbinding a shortcut must leave the menu operation available.

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
