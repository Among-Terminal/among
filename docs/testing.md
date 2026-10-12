# Verification aids

These controls are for automated application tests, not normal use.
Use an isolated test session: input automation must never target personal terminals.

| Variable | Effect |
| --- | --- |
| `AMONG_SELFCHECK` | Reports live widget, plugin and slot state to stderr. |
| `AMONG_SELFCHECK_TICKS` | Limits reports; default `120`. |
| `AMONG_SCRIPT` | Reads startup actions from a file. |
| `AMONG_ACTION` | Runs one startup action. |
| `AMONG_REBIND` | Overrides bindings for this launch. |
| `AMONG_ENABLE` / `AMONG_DISABLE` | Overrides selected plugin switches for this launch. |
| `AMONG_COLOR_SCHEME` | Selects `light` or `dark`. |
| `AMONG_LIST_BINDINGS` | Prints bindings and exits without opening a window. |

Script actions include `split_pane`, `close_pane`, `new_workspace`, `focus_left`,
`resize_right` (also `resize_left`, `resize_up`, `resize_down`), `zoom`, `fullscreen`,
`preferences` and `next_workspace`.
`workspace_N` activates workspace N, counting from 1. `type:<command>` sends text
to the focused terminal; `feed:<pane>:<command>` targets a numbered pane.
In these commands, `~` represents a space.
`collapse-sidebar`, `expand-sidebar` and `request-close` exercise window controls.

See [contributor checks](../CONTRIBUTING.md#checking-changes) for isolated X11 tests.
