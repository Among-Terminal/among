# Session storage

The [session guide](https://among-terminal.github.io/sessions/) owns restoration
controls and shell directory reporting. `src/session.rs` owns the snapshot format.

`session::path` uses `XDG_DATA_HOME` whenever it is set, including empty or
relative values. Only an unset value falls back to `$HOME/.local/share`.
The resulting base is joined with `among/session.json`; if both variables are
unset, no path is available. Session writes are independent of preference files,
plugin installation and shell configuration.

## Saving and restoring

Changed session state is checkpointed every two seconds by a separate ordered
writer. Disk completion depends on storage availability. Unchanged state does not
cause disk writes. Confirmed application exit saves the latest model before
terminal teardown; canceling exit keeps the live session running. A crash can
lose changes newer than the last completed checkpoint. Power loss or a storage
error can lose more: there is no unconditional durability guarantee.

If a saved directory is deleted or inaccessible, the shell uses an accessible
home or startup directory. Settings shows affected panes. VTE can silently fall
back during spawning; the host reads the new process's actual `/proc/<pid>/cwd`
rather than assuming the requested path was used. Only the actual known location
is recorded. A split records the directory the new shell actually started in.
OSC 7 reports update known directories even for inactive workspaces and do not
depend on enabling plugins. See [directory reporting](https://among-terminal.github.io/sessions/#directory-reporting).

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
