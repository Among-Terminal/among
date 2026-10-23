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

`scroll:<pane>:<delta>` hands one touch surface delta to the same handler a real
gesture reaches, so a check can exercise touchpad scrolling on hardware that
cannot produce one: XTEST sends wheel clicks and nothing else. The delta is in
surface pixels and negative scrolls towards older output. The step reports
`pane`, `delta`, the row height, whether it moved the history, and the value and
limits of the terminal's scroll adjustment, so a failing scroll check can be
read from the log. There is deliberately no wheel form: wheel clicks are the
terminal library's own, a harness can send a real one, and the library offers no
way to ask for one programmatically.

The self-check line carries `pane_scroll=["<pane>:<lines back>@<row height>", ...]`,
which is zero while a terminal shows its newest output and grows while it shows
older output.

The self-check's `sidebar_states=[<row>:<colour>:<editing>:<panes>, ...]` and
`name_rects=[<row>:x+y+WxH, ...]` follow the name itself rather than the kind of
widget showing it: a row that is not being renamed reports the bounds of its name
label, a row being renamed reports the bounds of its field, and `editing` is true
while the field is there. A pointer test can aim at a name without knowing which of
the two widgets it will find.

Window probes report `toast_widgets` from the live widget tree, `settings_problem`,
the allocated `sidebar_width` and the desired `sidebar_preferred` (`-` before a
width has been chosen). `sidebar_resize_visible` and `sidebar_resize_cursor`
describe the drag target; `sidebar_resize_rect=[edge:x+y+WxH]` and
`sidebar_toggle_rect=[toggle:x+y+WxH]` use native-surface coordinates, including
client-side shadow offsets, so real pointer events can target the narrow edge.
`notices_recorded` counts logged terminal/plugin notices, not visible pop-ups.

Bell backend probes report `bell_events`, `bell_application_active`,
`bell_viewed`, per-pane `bell_panes` (unread/pulse state), current-member
`bell_workspaces` aggregates and bounded `bell_diagnostics`. These are backend
observations, not evidence that notification stripes or audio were presented.
See [the implemented bell boundary and pending adapters](terminal-bell-backend.md).

The ignored application test
`app::bell_tests::live_bells_follow_terminal_focus_lifetime_and_owned_windows`
requires the isolated X11 harness, a window manager and xdotool. It drives actual
PTY bells and OSC terminators, native Preferences/menu focus, hidden terminals,
Restore, unrelated-window activation and stale source callbacks. It checks
backend state and real focus, not notification rendering or audio output.

The command integration test `cargo test --locked --test notification_commands`
compiles its synthetic Rust executable and retains process/stdin/environment
evidence under `AMONG_COMMAND_TEST_ROOT` (or `target/notification-command-tests`).
It uses the production ten-second deadline and 32-KiB combined output budget.
Run it before the ignored native command-input test described in the
[command backend contract](notification-commands.md), so the isolated display
can use the cached fixture without needing a compiler in its stripped environment.

See [contributor checks](../CONTRIBUTING.md#checking-changes) for isolated X11 tests.

## Where checks keep their evidence

`among-tools` checks create private work directories (application home,
configuration, logs, screenshots) below one check root and leave them there
as evidence:

1. `AMONG_CHECK_ROOT`, if set; it must be an absolute path.
2. Otherwise `$XDG_CACHE_HOME/among/checks`.
3. Otherwise `$HOME/.cache/among/checks`.

A missing root is created with mode `0700`. The location is on disk on
purpose: evidence must survive a reboot, so do not point it at a `tmpfs`
such as `/tmp`. Old evidence is not removed automatically; delete runs you no
longer need.

The display harness passes `AMONG_CHECK_ROOT` to every command it runs. The
application's ignored graphical tests refuse to start unless their
configuration directory lies below that root, so they never touch personal
configuration. CI workflows set the root explicitly.

## Installer ENOSPC context

`among-tools check-installer-enospc-from-cargo CARGO_TEST_RECORDS` selects the
unique exact ignored installer ENOSPC test from real Cargo records and executes
its unchanged assertions. Run it only in a fresh disposable Docker container
with a dedicated `/owned-full` tmpfs: writable, `nosuid,nodev,noexec`, at most
128 MiB. The runner checks the actual mount and filesystem capacity before
creating a new user-owned child directory or starting a test. It refuses an
ordinary directory, bind mount, shared `/dev/shm`, missing/ambiguous test or
nonempty fixture mount. Docker presence is an accidental-host-use guard, not an
authentication or sandbox guarantee against deliberate same-user interference.

Checks CI runs this command unconditionally on both native architectures before
the generic graphical suite. Only this exact case is routed out of standalone
graphics; that exclusion is not acceptance credit. The dedicated command fails
if its context or original test fails. Ordinary evidence, TMPDIR, `/tmp`, `/run`
and `/dev/shm` remain disk-backed; only the bounded fixture is intentionally full.
Logs, actual executable hash, mount information and the retained installed record
are written below `AMONG_CHECK_ROOT`. The retained old generation is checked by
inventory inspection, not executed on the `noexec` fixture mount.

## AppImageLauncher on GNOME Wayland

`among-tools check-appimage-gnome APPIMAGE NEW_EVIDENCE_DIRECTORY` runs the real
`ail-cli` integrator and an owned headless GNOME Shell. It needs Debian's GNOME
Shell, GJS/GdkPixbuf, AppImageLauncher, Bubblewrap, D-Bus tools and an available
graphics backend.
It creates a private HOME/XDG profile, PID/network/mount namespaces and two private
buses; it never uses the personal display, session bus or application settings.
The shell's native automation hook enables inspection only in that compositor.

The runner integrates after the owned Shell has started, checks opening the
registered file without GIO launch hints, menu activation, pin activation, normal
close/confirmation and
fresh-process relaunch with session restore and real terminal input. It records
the generated entry, versions, executable paths, window/shell IDs, icon bytes,
child environment and a dock screenshot. It also copies and decodes the actual
128 px and 256 px file thumbnails written by AppImageLauncher, rejecting empty
or fully transparent output and requiring the mark's gold and dark pixels.
This is separate from checking the installed themed SVG. It uses runtime extract-and-run because
setuid FUSE mounting is unavailable in the unprivileged namespace.
`--native-present` additionally requires a configured native DEB and its system
desktop entry, and leaves that entry visible in the namespace; without it, any
system entry is masked.
This tests only the supplied image on the recorded profile, not final DEB/RPM
acceptance. Missing prerequisites and failed observations are failures, not skips.
