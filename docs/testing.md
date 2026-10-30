# Verification aids

These controls are for automated application tests, not normal use.
Use an isolated test session: input automation must never target personal terminals.

`among-tools ci-x11 among-tools check-workspace-reorder <among-binary>` drives
native workspace drags, cancellation, positional keys, and restoration from the
application's actual session file. Use the built tools executable's path in both
positions, as with the other contributor checks. The check leaves logs and
screenshots below `AMONG_CHECK_ROOT`; it never targets personal terminals.

`among-tools ci-x11 among-tools check-pane-workspace-move <among-binary>` drives
a pane header onto workspace entries in both sidebar presentations. It checks the
destination indication, moved shell process, directory, scrollback, header colour,
source closure, rejection and Escape cancellation, then relaunches from the
application's checkpoint and verifies the inserted pane after Restore. A second
window covers drops into and out of an expanded workspace and a dragged pane
whose shell is killed during the drag.

`among-tools ci-x11 among-tools check-pane-drag-guards <among-binary>
<check-drag-source> <summary-plugin>` needs two prebuilt fixtures:
`cargo build --release --locked --example check-drag-source` and
`cargo build --release --locked --manifest-path tools/among-tools/Cargo.toml
--example summary-plugin`. `check-drag-source` is a separate X11 client that
drags terminal text, a file or a number in a private format onto a workspace
entry; it runs only on a display the check owns. Over the entry no pane
destination, pane drag or workspace reorder may be reported, and the release
must leave every workspace unchanged. A private format that no drop target
lists is never delivered to Among by GTK, so that case shows that the payload
changes nothing rather than that a guard refused it. `summary-plugin` reports
every workspace summary from `summary.txt` beside its executable; the check
rewrites that file while a pane header is held over another entry and requires
the shown destination and the old drawn summary to survive until the drop, and
the new summary to be drawn after it. It repeats the refresh during a drag that
is cancelled with Escape: the old summary must stay drawn while the drag lasts
and the new one must appear after the cancel. A later plugin answer may be what
draws it, so this case shows that the hold ends with the gesture, not which
rebuild ends it. The drag source writes
markers when its native drag begins and ends, and the check requires both, so a
drag that never started cannot pass as a rejected one.

A self-check line after the pane drag observations reports `rendered_summaries`
(the first plugin summary label each sidebar row draws, as `row:text`) and
`stored_summaries` (every summary text the slot store holds, as `index:text`).
Texts are not escaped, so the fields suit fixtures with plain one-word
summaries. A held sidebar rebuild shows up as the two disagreeing.

Workspace drag observations include `workspace_order` (runtime references in
display order), `active_reference`, `workspace_revision`, `workspace_drag` (source
reference or `none`), and `workspace_target` (insertion gap in the pre-drop order).
`workspace_indicators` is read from the rows' actual insertion styles.
`workspace_hit_rows` and `workspace_viewport` report native window coordinates;
`workspace_scroll` reports the live vertical adjustment. These are observations,
not an alternate API for injecting a successful drag.
`workspace_native_drag` and `workspace_native_action` report the actual GDK drag
object and selected action, separating native gesture recognition from a prepared
application transaction.

Pane drag observations include `pane_drag` (the source pane id or `none`),
`pane_target` (the workspace reference under the pointer or `none`), and
`pane_indicators` (row indices currently showing the pane-destination style).
They are live observations, not a way to inject a successful pane drop.

With self-checks enabled, `among[session-write]` reports
`checkpoint_submitted=true` when a changed periodic snapshot is queued and
`final_saved=true` when the final write completes successfully before teardown.
The ordering check uses these observations, the on-disk pre-exit snapshot and
normal process exit to distinguish final-save acceptance from an earlier tick.

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

## Cross-repository managed transport

The `managed-transport-interop` Cargo feature only compiles additional internal
tests when `cfg(test)` is also active. It changes no runtime capability or user
preference. Default host builds and tests require no companion checkout. The
dedicated native interoperability workflow enables the feature explicitly;
ordinary package, native and graphical gates remain unchanged and mandatory.

The workflow checks out the exact companion commit
`306839e144d7a4c98432c6c68dad016c412dd656` (tree
`e9d7a98a8162a3b58154b4a9122c70cddaa1db16`), verifies its public repository identity,
and independently builds its `managed-status` **protocol fixture**, production
`clock-managed`, legacy `clock` reference, production `system-status-managed`,
production `pane-pwd-managed` and the labelled `managed-pages` SDK reducer fixture.
It records both sources, native
compiler hosts and all executable hashes. The original five fixture cases still
test real `PluginHost`, inventory inspection, pinned-descriptor spawn, access
review, API 1.2 admission, scoped status and stopping against the external fixture.
They are not relabelled as production clock or authenticated package acceptance.

Feature-enabled tests require `AMONG_MANAGED_TRANSPORT_FIXTURE` (an absolute,
regular executable path), its independently computed SHA-256 in
`AMONG_MANAGED_TRANSPORT_FIXTURE_SHA256`, and an existing absolute owned
`AMONG_CHECK_ROOT`. Missing setup fails; it never skips or silently falls back to
the in-tree fake plugin. Run the compiled tests with private HOME/XDG directories
as the workflow does, without changing process-global environment inside tests.
The copied fixture and its synthetic receipt/manifest stay below the check root.
Those receipts are local protocol setup, not authenticated Release installation.

The separately labelled clock cases also require absolute regular executable
paths `AMONG_MANAGED_CLOCK_BINARY`, `AMONG_LEGACY_CLOCK_BINARY` and their compiled
digests `AMONG_MANAGED_CLOCK_SHA256`, `AMONG_LEGACY_CLOCK_SHA256`. Missing/wrong
setup fails. The managed binary is copied and inspected under synthetic clock
identity metadata before the real host reviews access and spawns it; the legacy
binary is only the unchanged renderer reference, not a managed fallback.

The workflow launches each exact clock case in its own test process with
`AMONG_CLOCK_CASE` and child-only `TZ` setup. UTC/Tokyo output is checked against
actual request-time UTC fences; unset/empty behavior is checked against the real
legacy renderer under the same environment. Both real host footer observations
must advance, carry the 1000-ms refresh and leave the application header empty;
valid pane/workspace targets remain unavailable without their grants. Over-budget
and control-byte cases must refuse before any supervisor is created. No Rust test
changes the parent process environment, and the data cases are not an alternate
serial run of an original failing suite.

Four ordinary host tests additionally observe actual child environment bytes:
unset versus empty, 1024/1025-byte boundaries, every ASCII control refusal and
unchanged POSIX/file/non-UTF-8 values, with the original allowlist retained. These
tests also execute in default host testing without a companion checkout. Dedicated
interop CI explicitly selects those four, the original five fixture cases and
six isolated clock data cases on each native architecture. It checks each exact
name and one real pass per invocation; filtered tests receive no credit.

The stale-target/generation controls hold an unchanged response actually read
from the supervised child's output channel before normal host polling consumes
it, revoke authority, and deliver it through the production handler. Follow-up
live responses must publish; replay must not. Additional isolated wrong-binding
controls modify only the named field of a genuine current response. Captured
replay and synthetic refusal controls are distinguished in the test source.

Missing selection, setup, timeout or assertion is failure. Protocol-fixture results
cannot substitute for the labelled production-clock cases. Even real host clock
status/TZ observations do not establish all-plugin parity, GUI/terminal continuity,
authenticated installation, publisher authenticity, migration, audio behavior or
release readiness. All ordinary full PR workflows remain mandatory; this slice
does not fix or waive a separate main retirement failure. A fixed source pin is
a compatibility checkpoint; update it through
review when changing the client, not through an unresolved latest URL.

The two separately labelled resource cases require absolute regular executable
`AMONG_MANAGED_SYSTEM_BINARY` and digest `AMONG_MANAGED_SYSTEM_SHA256`; missing or
wrong setup fails. The same real inventory/review/admission/supervision path uses
only status authority. It observes initial CPU N/A and then actual numeric
CPU/MEM/root-DISK header fragments, exact text/meter agreement, priorities,
tooltips and 1000-ms refresh. Footer output stays empty; existing pane/workspace
targets remain unavailable without context grants. A withheld genuine retired
reply and a separately labelled isolated stale-generation control cannot consume
current request credit or overwrite a re-enabled header. Both cases explicitly
observe normal stop ownership; they do not repair the separate retirement defect.

These add two invocations after all original fifteen fixture/environment/clock
invocations per native architecture. The companion's own independent measurement,
per-source N/A and sampling-rate assertions remain required in its repository;
resource-host tests neither replace them nor claim a graphical bar was rendered.
Synthetic receipts are still not authenticated release installation. Actual GTK
meters, installed lifecycle, terminal continuity and whole task2.3 remain separate.

Seven additional invocations retain all seventeen above. Three run production
pane-pwd against the actual host's focused/hidden model context, changed focus,
long Unicode cwd, unknown/empty/over-budget fields, required-access refusal and
closed/retired targets. Every real output is checked for exact scope, full tooltip,
480-character truncation and 1000-ms refresh; unrelated slots stay empty. These
are not observations of mapped GTK panes or terminal input.

Four use the external scalar `managed-pages` fixture. Authentic child enumeration
frames pass unchanged to the production host's page handler: application 32+1
count pages with an interleaved real pane reply, and hidden workspace 7+7+3
byte-limited pages with sibling exclusion. Completion depends on later pages.
Closed workspace continuation must fail before the host deadline, publish no
empty-success status and retain ordinary restart/backoff accounting. A genuine
retired enumeration cannot attach to the re-enabled generation. The same host
supervisor must explicitly reach stopped ownership; child exit alone is not proof.
The closed-workspace case observes a timely host fault/backoff, not the numeric
child exit code; direct SDK exit-status observations are a separate boundary.

Setup requires `AMONG_MANAGED_PWD_BINARY`/`AMONG_MANAGED_PWD_SHA256` and
`AMONG_MANAGED_PAGES_FIXTURE`/`AMONG_MANAGED_PAGES_SHA256`. Missing, linked,
non-executable or digest-mismatched setup fails, never falls back to another binary.
CI also checks the exact source tree and native ELF machine. Source-built binaries
and synthetic receipts do not establish authenticated official packages,
installation, package upgrades/rollback or whole tasks2.1/2.4 acceptance.

The separately selected ignored resource-widget case uses the same exact real
binary with explicitly synthetic local metadata, never an authenticated Release.
It drives Cancel/Escape, Accept and enable, then disable through native keyboard
controls; actual terminal input must still reach the same PTY before review,
while running and after disable. Three mapped, allocated GTK ProgressBars must
match the real percentage labels/fractions. A fresh process must restore disabled
state without spawning, rendering bars or rewriting saved bytes; the legacy
document remains byte-identical throughout.
The disabled document must exist, retain the accepted status-only review and
match the post-disable bytes carried across the process boundary; the installed
identity must still be registered as disabled. PTY continuity uses the retained
Pty object, not a reusable numeric descriptor. Labels and bars must be mapped,
allocated and contained within the visible header/window bounds. Input checks
explicitly place window/terminal focus before native typing; they do not establish
automatic focus restoration, complete Tab traversal or absence of focus stealing.

Interop CI runs this one case in light and dark profiles through the existing
owned-display harness, after all24 transport/clock/resource/pane/SDK invocations
(the original17 plus seven pane/SDK cases).
Each profile's controller starts two fresh test processes; these are not three
distinct test definitions or a replacement for earlier tests. The case requires
`AMONG_RESOURCE_GUI_SCREENSHOTS` below `AMONG_CHECK_ROOT` and retains real enabled,
disabled and fresh-disabled window captures. Missing display/tools/source/digests
or screenshots fail rather than silently skip. `GTK_A11Y=test` selects GTK's test
accessibility context so actual progressbar roles and exact stored accessible
labels are checked; this is not AT-SPI traversal or a real screen-reader/whole-
WCAG assessment. Screenshots supplement those property checks, not replace them.
These source-bound local-receipt checks still do not establish repository package
installation, update/uninstall or full packaged/platform acceptance.

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
