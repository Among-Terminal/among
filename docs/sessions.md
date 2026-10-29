# Sessions

Session snapshots live at `${XDG_DATA_HOME:-$HOME/.local/share}/among/session.json`.
This is independent of `plugins.json` and `shortcuts.json`: saving a session does
not rewrite preferences, install plugins or modify shell configuration.

## Saving and restoring

A new application process with a valid snapshot asks **Restore** or **Start new**.
Restore recreates each terminal as a fresh shell in its saved directory, including
inactive workspaces, and restores the active workspace and its focused pane.
The saved sidebar sequence includes manual drag-and-drop ordering. Restore brings
back that order, with the same workspace active at its new position; Alt+1 through
Alt+9 then use those restored positions. Ordering is part of the session, not a
separate preference, and does not bypass the Restore/Start new decision.
It does not resume old processes, repeat commands, restore scrollback or retain
temporary pane zoom. The window opens maximized as usual.

Start new (also Escape) opens the usual single-terminal workspace. Its next
checkpoint replaces the previous session. Closing the window while the prompt is
still open exits without launching terminals or altering the old snapshot.
Activating an already running application does not ask again.

Changed session state is checkpointed every two seconds by a separate ordered
writer, normally reaching disk within three seconds. Unchanged state does not
cause disk writes. Confirmed application exit saves the latest model before
terminal teardown; canceling exit keeps the live session running. A crash can
lose changes newer than the last completed checkpoint. Power loss or a storage
error can lose more: there is no unconditional durability guarantee.

If a saved directory is deleted or inaccessible, the shell uses an accessible
home or startup directory. Settings shows affected panes. VTE can silently fall
back during spawning; the host reads the new process's actual `/proc/<pid>/cwd`
rather than assuming the requested path was used. Only the actual known location
is recorded. A workspace's first terminal keeps the normal startup directory; a
terminal created by [splitting a pane](panes.md#splitting-a-pane) starts in that
pane's directory instead, and records it because it really started there.

For directory changes after launch, the shell must publish OSC 7. See the tested
[Bash integration](plugins.md#live-status-examples). This does not require enabling
pane-pwd or any other plugin, and Among Terminal never edits your shell files.
For example, after enabling the optional Bash hook, `cd /tmp` is recorded on the
next checkpoint even when the workspace is inactive.

Storage failures are reported in Settings without blocking terminal input. Later
checkpoints retry after the problem is fixed. Invalid or future-version documents
are preserved until externally corrected or removed. A failed final save asks
whether to keep working or exit without saving; keeping the window open resumes
checkpoint retries. Routine checkpoints do not show success notifications.

## Snapshot format

Version 1 stores workspace order, names, colours, the active workspace index,
per-workspace focus, nested split orientations/ratios and a directory entry for
each pane. `vertical` means a vertical divider (panes side by side), while
`horizontal` means a horizontal divider (panes stacked). Ratios describe the
first child's fraction, not pixels, and must lie between 0.05 and 0.95.

```json
{
  "version": 1,
  "active": 0,
  "workspaces": [
    {
      "name": "project",
      "colour": 0,
      "focused": 1,
      "layout": {"type": "pane", "id": 1},
      "directories": [{"pane": 1, "cwd": null}]
    }
  ]
}
```

Pane IDs are nonzero and unique across all workspaces. Sparse IDs retain their
numbers without reserving an array up to that number; exhausted IDs prevent new
pane creation instead of wrapping. Each focused ID must belong to that workspace.
Directory entries must exactly match its leaves. Paths are absolute or `null`
when unknown; they come from terminal-reported cwd or the directory the shell
actually started in. A pane with no directory of its own records `null` rather
than borrowing another pane's location; a pane created by splitting records the
directory it was started in, which is the pane it was split from. Changes after
launch need shell OSC 7 reporting to be known reliably. No plugin is required to
collect these reports.

The file contains no commands, process IDs, shell environment or scrollback.
Input is bounded to 8 MiB, 64 nested splits, 4096 panes and 256 workspaces;
unreadable, malformed, unsupported or unsafe files are not silently overwritten.
The loader also retains the JSON parser's recursion safety limit.

New snapshot files are private (mode 0600). Existing stricter permissions are
preserved. Saving writes and syncs a same-directory temporary file, then renames
it atomically and syncs its directory. A failed replacement retains the previous
complete file; a directory-sync error after replacement means the new file was
written but its power-loss durability is uncertain. Symlink and non-regular
destinations are refused. Manually editing a snapshot while it is being saved
can produce a retryable concurrent-change warning.
