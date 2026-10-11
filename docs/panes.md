# Panes

A pane is a container holding one terminal together with the application's own
interface around it. This is what the container looks like:

```
+------------------------------------------+
| title, and any plugin content for the pane|   <- the header, and the drag handle
+------------------------------------------+
|                                          |
|               terminal                   |
|                                          |
+------------------------------------------+
| plugin footer content, when there is any |   <- hidden while empty
+------------------------------------------+
```

## Dragging a pane to a new position

A pane can be moved by dragging its header onto another pane. While the drag is
in progress the target shows which half the pane would occupy, and dropping
divides the target's area in half along that edge.

- Drop on the **left** or **right** part of a pane: the two end up side by side.
- Drop on the **top** or **bottom** part: the two end up stacked.
- The indication follows the pointer between edges, so the result is known before
  the pane is dropped.
- A drag that leaves every pane, or ends anywhere else, changes nothing.

The header is the handle rather than the whole pane, because dragging across a
terminal body is text selection — a gesture a terminal user relies on constantly.

A moved pane keeps its identity: the terminal and the process running in it are
the same afterwards, and focus follows the pane to its new position. The space
the pane vacated is reclaimed by whatever shared it, so no gap is left behind. A
drop on a pane's own area does nothing.

## Headers and the terminal's title

Clicking a terminal or its header makes that pane active: keyboard input, pane
actions and the focused highlight follow the same pane. Dragging within terminal
text remains selection; only the header is a pane-movement handle.

A pane's header shows the title the program in that terminal has set, through the
usual OSC escapes, alongside a permanent `#N` identity label supplied by the host.
Workspace terminal counts in both sidebar forms also belong to the host and do
not require a plugin. Programs use titles to advertise what they are doing.
Examples include a file
open in an editor, a host being connected to, a build in progress — so the header
is the natural place to read it.

When a title changes on a pane that is **not** focused, the header is marked and
stays marked until that pane is focused. A title change on the focused pane leaves
no mark, because the user is already looking at it.

A pane whose terminal has never set a title, which is rare because most shells do
at every prompt, falls back to showing the pane's number.

Plugin content for the pane's header appears beside the title; neither replaces
the other. See [plugins.md](plugins.md).

## Terminal context menu

Right-click a terminal, or press **Shift+F10** in the focused terminal. If the
program has enabled mouse reporting, an ordinary right-click goes to that
program; use **Shift+right-click** for Among's menu instead. Arrow keys navigate
the menu, **Enter** activates a row, and **Escape** returns to terminal input.

The menu belongs to the requesting pane, not whichever pane was previously
focused. It uses symbolic icons alongside text labels. Copy uses selected text,
Paste delegates to VTE (including bracketed paste when enabled by the program),
and Select All selects terminal text. Copy is disabled without a selection;
Paste is disabled when the clipboard offers no text.
Both states update while the menu is open. Select All includes available
scrollback and blank terminal rows; Copy preserves the resulting text. Paste
sends text to the receiving program rather than replaying a saved session. A
shell may interpret pasted newlines, so do not paste untrusted commands.

At a pointer target, explicit hyperlinks take precedence over plain absolute
http/https/mailto URLs. Copy Link uses the captured URI even if the pointer later
moves. Other valid explicit schemes are copy-only; Open Link is disabled for them.
Malformed, control-containing and oversized targets are not offered. Keyboard
invocation has no link target and omits link entries. Open Link requests the
desktop's handler asynchronously; failures are reported without shell execution.

Split Pane, Expand/Restore Pane and Close Terminal use the same operations as
their shortcuts. The menu remains available when those shortcuts are unbound;
its hints follow the current bindings. Closing the last terminal still asks for
confirmation rather than silently exiting.

## Closing and exiting

Closing the focused pane focuses the sibling that expands into its space. If
that sibling is split, its first leaf in tree order wins. Closing a background
pane preserves focus; process exit follows the same rule.

A pane is a terminal window and the application is what surrounds it, so closing
cascades upward:

```
Ctrl+Shift+W    the pane and its process go, the layout closes up
  last pane     the workspace closes too
    last one    a confirmation appears before the application exits
```

A pane also closes when the process in it exits — typing `exit`, or a program
that ends — following the same cascade. The exit is reported, naming the status
when it is not zero, so a terminal disappearing is never mistaken for something
the user did. Closing the window with the platform's close control takes the same
path, including the confirmation.

The cascade stops at the last workspace: it asks rather than exiting, because
every terminal's process goes with it.

## Expanding a pane

`Ctrl+Shift+X` expands the focused pane to fill its workspace, and pressing it
again restores the layout.

Only the rendering changes. The layout tree is untouched, so restoring brings
back every pane at exactly the size and position it had, nested dividers
included, and no terminal is restarted. Closing the expanded pane simply returns
the workspace to its normal layout. Expanding in one workspace does not affect
any other.
