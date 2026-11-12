# Session storage

The [session guide](https://among-terminal.github.io/sessions/) owns restoration
controls and shell directory reporting. [session.rs](../src/session.rs) owns the
snapshot format.

`session::path` uses `XDG_DATA_HOME` whenever it is set, including empty or
relative values. Only an unset value falls back to `$HOME/.local/share`.
The resulting base is joined with `among/session.json`; if both variables are
unset, no path is available. Preserve this path-resolution contract independently
of the stricter XDG path handling used by plugin installation.

## Saving and restoring

`install_session_checkpoints` in [app.rs](../src/app.rs) submits changed snapshots
to `session::Writer` on a two-second timer. Preserve the saved/submitted snapshot
comparison to avoid redundant writes and the ordered worker to keep disk I/O off
the GTK loop. Final-save completion must precede terminal teardown; cancelling
close must resume checkpoints.

Keep directory selection in `session::initial_directory` separate from spawn
confirmation in [pane.rs](../src/pane.rs): VTE may fall back during spawning, so
record the actual `/proc/<pid>/cwd`, not the requested path. The cwd callback in
[app.rs](../src/app.rs) must update inactive workspaces too and remain independent
of plugin enablement.

Keep write failures retryable through the checkpoint controller. A failed or
unsupported load must not authorize replacement of the stored document. Recovery
controls belong in the [session guide](https://among-terminal.github.io/sessions/#storage-and-recovery).

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
actually started in. Snapshot capture must preserve `null` for a pane whose
directory is unknown, without borrowing another pane's location.

Keep `Snapshot::validate` and the bounded reader in [session.rs](../src/session.rs)
within 8 MiB, 64 nested splits, 4096 panes and 256 workspaces. Retain the JSON
parser's recursion safety limit and validate existing bytes before replacement.

`save_with_hooks` must create a same-directory temporary file with mode 0600 or
the existing stricter mode, sync its contents, compare the destination with the
original bytes, rename, then sync the parent directory. Keep pre-replacement
failure distinct from post-rename durability uncertainty. Refuse symlink and
non-regular destinations and return a retryable error for a detected concurrent
change.
