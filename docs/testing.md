# Verification aids

These controls are for automated application tests, not normal use.
Use an isolated test session: input automation must never target personal terminals.

## Fixtures to build before `cargo test`

Some tests start the example programs as real processes: `notification-plugin`
(the managed notification backend tests and the notification UI tests),
`effect-authority-helper` (`tests/command_effect_authority.rs`) and `fake-plugin`
(the plugin host tests). `notification-plugin` keeps its control files and
transcripts in the directory it starts in, or in `AMONG_PLUGIN_DATA_DIR` when the
host sets it, which is the case only under the real confinement; the backend test
`producer_ui_and_delivery_provider_work_under_real_confinement` uses that to run a
producer and a delivery provider confined (it returns early, printing NOT VERIFIED,
on a kernel without full protection). A full `cargo test --locked` builds the
examples first.
A run limited to one target (`cargo test --bin among`, `--lib`, `--test <name>`)
builds only that target, and the tests that need an example then fail at once
with "build --example notification-plugin first" or a similar message. Before
such a run, execute `cargo build --locked --examples`.

## Desktop notification contract

Build the application test executable with `cargo test --locked --bin among
--no-run --message-format=json`, then use its absolute executable path:

```sh
among-tools ci-x11 among-tools check-desktop-notifications /absolute/path/to/among-test-executable
```

The full-window GTK test owns `org.gtk.Notifications` on the runner's private
session bus; it refuses to replace an existing owner. It rings a real VTE terminal
and checks the actual `AddNotification` body and field names, application id,
normal priority, icon, complete default-action target, stable replacement id and
withdrawal on focus, close and normal shutdown. It also checks viewed/off
suppression, hostile OSC-title sanitisation, the settings row and missing-service
reporting. Activation calls `org.freedesktop.Application.ActivateAction` over
D-Bus for another workspace, a hidden sibling of an expanded pane, a closed pane
and a previous instance token.

The named-sender private-bus tests
`app::bell::desktop::named::tests::gtk_named_private_bus` and
`app::bell::desktop::named::tests::portal_named_private_bus` drive the production
`GioSender` against fake services. The GTK case checks the real
`AddNotification`/`RemoveNotification` payload, removal from the old unique owner
when the same id is added to a new owner, and raw error and malformed-reply
reporting. The portal case checks the forced-GTK fallback, autostart, an early
`RemoveNotification` before the held `AddNotification` reply, held Add/Remove
replies and the shared drain budget. Both prove that a named route never falls
back to freedesktop delivery.

A separate private-bus FDO fixture holds the `Notify` reply and checks the raw
freedesktop payload, automatic `CloseNotification` after a viewed/off withdrawal,
coalescing while a reply is pending and the real application action handler. It
binds each server id to the reply's unique owner; the scenarios cover the first
owner, an A-to-B owner swap, a retired owner whose late activation token and
action still select the old target while its Close reply is held, a foreign owner
that must not inherit that identity, and removal of the identity when the Close
replies, fails or the drain budget expires. A direct GIO control keeps the
original early-withdrawal loss as a measured comparison.
Failure to observe the held reply or the daemon-side close is a test failure, not
a product pass.
The checker first copies a bounded regular executable into its private evidence
directory, then enumerates and runs that same read-only snapshot. `result.json`
keeps the original `executable` path, the `executed_snapshot` path and the snapshot's
SHA-256, so replacing a Cargo output cannot change which image the report names.
Logs include the recorded calls. A Rust test that runs zero cases is a failure. Evidence stays below
`AMONG_CHECK_ROOT`.

The required native CI jobs on Ubuntu 24.04 amd64 and arm64 run this exact ignored
application case through `run-ignored-tests`. Each case owns its display and
session bus, and its recorded D-Bus calls and test result appear in the job log.
The standalone command above also writes the executable hash and a result file.
The ignored-test runner requires the exact desktop case in the selected
application binary's actual listing. Tools and example binaries keep their own
test sets; removing or renaming the application case fails the native job.

This checks application integration on an owned X11 session. Use the separate
GNOME checker to observe real banners and window activation:

```sh
among-tools check-gnome-notifications /absolute/source/root /absolute/native/among wayland /absolute/new/evidence-wayland
among-tools check-gnome-notifications /absolute/source/root /absolute/native/among x11 /absolute/new/evidence-x11 --xvfb /absolute/path/to/Xvfb
```

The runner needs Bubblewrap with unprivileged user namespaces, GNOME Shell,
`gdbus`, `dbus-daemon` and their system runtime dependencies. X11 also needs
Xvfb; its default path is `/usr/bin/Xvfb`. The Shell `Eval` implementation must
await returned Promises, as upstream GNOME Shell 46 and later do. A capability
probe runs before virtual input is created; unsupported synchronous Eval is a
prerequisite failure, with the observed reply in `eval-capability.txt`.
It copies the native application,
checker, optional Xvfb and matching desktop entry into a new private evidence
directory. Run it directly: it creates its own display and session bus.
The headless seat owns virtual pointer and keyboard devices; without keyboard
capability, Wayland cannot deliver keyboard focus to GTK even when Mutter has
selected the window. `seat-ready.json` records the observed input devices.
The other-project scenario requires a fresh Among self-check showing workspace 1
active before ringing the source in workspace 0. Its initial state is retained in
`project-before-app.log`; a no-op workspace action cannot pass as cross-project
activation. Timeouts from the outer namespace process include the evidence path.
The mount namespace exposes `/usr` read-only, selected system configuration,
minimal `/dev`, private runtime directories and the fixture directory. It has
no host HOME, desktop sockets, session bus, network or device mounts.
The fixture uses Cairo for the application, disables its accessibility backend
and limits Mesa software rendering to two threads. These settings apply only
to the owned test processes.

The checker inspects the actual Shell notification source, complete action
target, banner allocation and window focus.
The selected mode sets `GDK_BACKEND` for Among. Every window snapshot also records
Mutter's actual `get_client_type()` as `client_type` and `client_type_value`;
the checker rejects an Among window whose client type differs from that mode.
This distinguishes a Wayland client from an Xwayland client in a Wayland Shell.
A virtual pointer clicks the banner body for a minimised window, another GNOME
desktop, another Among project and
a terminal hidden by an expanded sibling. The last case must restore the
layout. It also closes a source, observes withdrawal and dispatches its captured
action from a real pointer release on an owned Shell test control with that
event's timestamp. This models a late action race; the remaining terminal must
stay selected. The control has its own Shell input region on both X11 and
Wayland; the pointer clicks its measured allocation. The result records the
input device, button, timestamp and action target. It does not claim a
click on an already withdrawn banner.
`inputs.json` identifies the copied executables by SHA-256. Shell and application
logs, JSON observations and PNG screenshots remain in the evidence directory;
`observations.jsonl` preserves each poll, including Shell notification policy,
queue, presence and banner state, so a timed-out final snapshot cannot hide an
earlier banner. The owned pointer moves before each BEL to register user activity.
Before the first BEL, the pointer closes any visible Shell startup banners using
their real close buttons; `startup-dismissals.json` records the banner identity,
button allocation and observed removal.
`result.json` appears only after every scenario passes. A missing prerequisite,
failed compositor or blocked activation returns an error with the evidence path.

This runner exercises its owned virtual GNOME session. A packaged application
on a physical desktop and distribution-specific integration still need their
own observations. GNOME's focus-prevention behaviour must be reported as
observed; the application does not bypass it. Repeat the private-bus check on
Ubuntu 24.04 and Fedora before accepting those target environments.

## Additional graphical fixtures

The ignored graphical tests also use `target/debug/notification-command-fixture`.
Cargo does not build it: the integration tests (`notification_commands`,
`command_effect_authority`) compile `tests/fixtures/notification-command.rs` with
`rustc` and copy the result there. It exists after a full `cargo test --locked` or
after those two integration tests have run, not after `cargo build`.

`among-tools ci-x11 among-tools check-plugin-development <among-binary>` starts
real Among processes on the one session bus of that run. It checks that a
secondary `--plugin-development` exits 2 with its diagnostic while an ordinary
primary stays ordinary, that an ordinary secondary does not change a
development primary, that a later ordinary start is ordinary, and four
simultaneous starts in both orders. The mode is read from the
`plugin_development=` self-check line; logs stay below `AMONG_CHECK_ROOT`.

Local catalogue installation is checked twice. The unit tests in
`plugin::install::local` run in an ordinary process and check that the public
actions refuse, that a package installs into `development/<plugin-id>` with
no repository or release fields, the hostile package, directory and catalogue
cases, a catalogue changed after review, replacement only after confirmation
with a new consent token, and that a development install does not start
without the flag. The ignored native test
`local_catalogue_installs_and_replaces_only_with_explicit_review_in_development_mode`
fixes development mode in its own process and drives Preferences, the review
dialog and the replacement confirmation with real keyboard input; it runs with
the other ignored graphical tests.

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
changes nothing rather than that a guard refused it. `summary-plugin` is a
managed fixture and reports
every workspace summary from `summary.txt` in its `AMONG_PLUGIN_DATA_DIR` (the one
place a confined plugin can read that the check can also write); the check
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
In these commands, `~` represents a space. `activate:<widget-name>` presses a
named widget in any open window, such as `notification-audio-preview`. Only the
application executable can do this for audio: a test binary cannot host the
decoder helper, so its Preview produces no sound. `focus:<widget-name>` hands a
named control to the keyboard. Both first show the Preferences page that holds the
control, because Preferences opens on Plugins and a control on a hidden tab cannot
take focus. `choose-wav:<absolute path>` gives the Sound row the path its file
dialog would return, because a native dialog cannot be driven from a script; the
file then goes through the same read, validation and save. `await:<mark>` holds the
script until the file
`<script>.<mark>` exists, so a harness can send real keys or type text at that
point.
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

`among-tools ci-x11 among-tools check-terminal-colours --source-build target/release/among tools/among-tools/target/release/examples/terminal-colour-fixture tests/fixtures/gnome-terminal-3.56.2-colours.json tests/fixtures/terminal-gsettings/base` (the last argument is the GSettings schema directory the source phase compiles in a private home) starts the real executable on the owned display with the Rust colour fixture as its shell. Build the fixture with `cargo build --release --locked --manifest-path tools/among-tools/Cargo.toml --example terminal-colour-fixture`. The fixture paints a calibration block and fixed SGR grid, records its exact emitted bytes and PID, then execs an interactive bash on the same PTY. The check opens Preferences, drives the two named controls with real keys/pointer events, captures the actual window with ImageMagick `import`, converts the PNG to bounded 8-bit straight `rgba:` bytes with `convert`, and samples 3x3 centres of solid background and reverse-video spaces. It records the PNG, raw bytes, format, coordinates, measured and oracle values. The independent catalogue values come only from the retained GNOME 3.56.2 JSON fixture; API colour values are not rendered evidence. `identify`, `convert` and `import` are prerequisites and their absence is a setup failure. Use `--installed /usr/bin/among` (or the actual installed launcher path) for package evidence; the result keeps the boundary, executed `/proc/<pid>/exe`, SHA-256, fixture/oracle hashes and source identity when supplied. A packaged AppImage/Snap keeps its own schema source; its missing or available GNOME inheritance path is reported as observed, not replaced with a source-build matrix. A result that still has any mandatory phase `not-run` is written for diagnosis but the command exits non-zero; only a complete report is accepted. The run also captures the owner baseline Gray on black + Tango (ANSI 4/12 and observed contrast) and, after that, all 11+6 catalogue choices in one binary run; the owner baseline retains before/after screenshots with readable normal-34/bright-94 text beside the exact pixel swatches, without a universal contrast guarantee. The full acceptance report expects eleven unique passed phases (`vertical-slice`, `owner-baseline`, `catalogue`, `source`, `no-write`, `solarized-unavailable`, `restart`, `continuity`, `osc`, `bold`, `layout`); Preferences uses the production default width 820, and layout checks the measured toolbar minimum against its allocated width plus the app's `prefs_tabs` JSON observer for five mapped, homogeneous, non-ellipsized labels in light/dark/high-contrast rather than resizing a test widget. When a phase fails, every phase that did not run is recorded as `not-run` and the command exits non-zero.

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

## Notification accessibility consumers

Checks selects the unique application test executable from real Cargo records
and runs four exact notification UI cases on individual owned displays: the
positive/negative accessibility guard, provider controls/actions, explicit role
review/restoration and visual attention/geometry. The ordinary package tests
prepare the command fixture; the workflow also builds the notification plugin.
Missing fixtures, absent/ambiguous test selection, a failed process or anything
other than one passing test per invocation fails the step.

These tests set `GTK_A11Y=test` before GTK initialization and require an actual
AT context and property before comparing accessible values. An outer environment
variable is not sufficient because the generic display runner disables a11y by
default. The ordinary ignored-test suite is still mandatory and unchanged;
these targeted cases do not replace its other tests or establish audio output,
packaged behavior or whole-release acceptance.

## Published-plugin native checks

`managed-transport.yml` runs on native amd64 and arm64 runners. Production clock,
system-status and pane-pwd executables come from the exact immutable official
prerelease pinned in `.github/plugin-acceptance.json`. The same required input
producer serves Checks, DEB, RPM, AppImage and Snap. None of these acceptance paths
builds replacement official plugin executables.

`among-tools prepare-official-plugin-inputs <new-directory>` validates the numeric
repository/publisher, complete bounded release inventory and full tag chain. It
downloads the schema2 catalogue through its observed asset ID and checks its size
and digest before parsing; package names, sizes and digests must match that same
release. Native archive members, ELF architecture and the common source delivery
are checked without replacing them with host-built executables. Repository,
latest, assets and the complete tag chain are re-observed after transfer; drift
refuses preparation. It retains downloads and emits schema2 `inputs.json` with
the observed numeric IDs, snapshot and binding, not installed receipts or grants.
Only the protocol fixtures are built from the exact companion source. The
supervised admission, stale-reply, paging, timezone, resource and keyboard controls
remain mandatory. Transport fixture receipts remain synthetic and do not prove
repository installation.

`published-plugin-inputs.yml` runs
`among-tools fetch-published-plugin-assets <pin> <new-directory>`. It observes the
exact numeric release ID, refuses drafts, mutable releases and non-prereleases,
checks the tag's complete binding against the pinned source, and verifies the
catalogue digest and all downloaded assets. It repeats the repository, release,
inventory and tag observation before uploading the bytes and original API bodies.
This live final observation occurs in the producer, before native consumers run.

`among-tools prepare-published-plugin-inputs <pin> <assets> <new-directory>` checks
both stored API observations and the producer receipt, then reconstructs native
paths offline with the existing catalogue, archive-member, ELF and source-delivery
checks. Its receipt keeps `candidate:false` and `prerelease:true`.
`among-tools export-plugin-catalogue <inputs> <new-directory>` exports only the
unchanged catalogue and verified native archives under their original release
names, for a separate local development catalogue test.

`among-tools check-published-development-install <among-test-executable> <inputs>
<local-catalogue>` runs the release-selection and local-install tests in separate
processes. `among-tools check-published-prerelease-install <among-test-executable>
<inputs>` runs the full UI lifecycle with the explicit test-only development
adapter. Neither changes the product's ordinary stable-only policy. Native tests
with `candidate:false` use the production GitHub HTTPS client for source refresh
and installation. They compare its live snapshot, tag binding and installed bytes
with the independently prepared inputs. The local catalogue case reads the
verified archive bytes locally; synthetic replacement uses its owned HTTPS fixture.
The aggregate preferences phases use `check-published-preferences-from-cargo`
with the same verified inputs and explicit test-only mode. Ordinary preferences
commands retain their existing mode and do not inherit this flag from the caller.

Checks runs the complete published selection/local wrapper in the required
`published-development` job on amd64 and arm64. The full published lifecycle stays
in `native`, together with the separate synthetic replacement case. Counting the
API calls in code, for a one-page release list and a tag directly targeting a
commit, the selection test issues 23 requests and the lifecycle issues 39; putting both in one
job would exceed the anonymous 60-request budget. The local case, preferences
persistence fixture and subsequent profile replay make no GitHub requests.
Separate jobs keep these API budgets separate, but do not guarantee independent
public IP quotas or a fresh GitHub allowance. Prior traffic, another job sharing
an address or additional release-list pages can still exhaust the available
budget; rate refusal remains a failing acceptance result.

The mandatory `check-synthetic-plugin-replacement` case retains the separate
replacement/rebinding fixture over those archive bytes. It requires explicitly
synthetic candidate inputs from `prepare-candidate-plugin-inputs` and invents its
release documents. Its result proves replacement behaviour, not publication.

`among-tools check-plugin-install <among-test-executable> <input-directory>` requires
stable inputs, an owned X11 display and an isolated check root. Both lifecycle
wrappers run the exact ignored host test
`app::plugin_tests::installation::real_published_official_packages_install_review_enable_disable_uninstall`.
An absent test, failed download, changed source/asset, ignored/zero-test result or
missing lifecycle assertion is a failure, never a fallback to the old standalone
plugin installer. Native UI, explicit access/enablement and terminal continuity
must be demonstrated by that host test before this check can pass. Preparing the
inputs alone establishes none of those behaviours or application-RC readiness.

`among-tools check-resource-measurements <profile>` takes the profile that
the applicable lifecycle wrapper observed. It accepts the installed `system-status`
executable only when the receipt, manifest, artifact and executable digests and size
match the published catalogue of the same run, then drives it over the managed handshake and compares
CPU, MEM and root-DISK with independent `/proc` and `df -P /` readings. No plugin
is built from this repository. The only plugin executable the host still builds is
`examples/fake-plugin.rs`, a labelled protocol and failure-control fixture; it
never stands in for an official plugin.

Package workflows compile exactly the `among` binary test target with
`AMONG_CHECK_SOURCE_SHA` set to the checkout's full HEAD. The compile-time value is
exported only with the observed runtime profile. `capture-package-plugin-profile`
receives `--published-inputs <prepared-directory>` from every package producer and
requires a clean checkout before and after the actual UI lifecycle, one Cargo test
artifact whose manifest and `src/main.rs` belong to that checkout, an unchanged
test-executable digest, and a matching compiled-in source SHA. Its bounded internal
transfer includes exact file hashes, the completed lifecycle observation, the
publication pin and original initial/final API bodies in `publisher-observation.json`.
Its `release-inputs.json` preserves the prepared release observation so replay
checks every selected receipt's full snapshot, catalogue and tag binding against
the same source, without embedded historical release/asset constants.
`check-package-plugin-profile` checks the expected host source, native architecture,
all declared bytes and missing/duplicate/unrecorded/link inputs before a package
renderer consumes it. The complete transfer remains within 8 MiB/64 entries.

This deliberate internal input may contain receipt/lease files; only its dedicated
package-checker artifact includes them. Hidden runtime capture directories remain
excluded from screenshot/failure uploads. No profile is a public application asset.
On compiler-free hosts the selected AppImage/Snap/DEB/RPM/archive executable renders
that observed profile. This is packaged **renderer replay**, not observation of
installation or access review through that packaged executable.
For the pinned published prerelease, replay first checks the raw publication
observations and keeps the profile's original prerelease receipts. It starts the
production executable without a development flag and requires all three plugins
to stay unavailable. A separate fresh replay then explicitly starts the same
executable with `--plugin-development` and checks the existing renderer, terminal
and fault-containment behaviour. Stable profiles keep the ordinary launch. Missing
publication evidence or a synthetic prerelease refuses the development replay.

## Cross-repository managed transport

The `managed-transport-interop` Cargo feature only compiles additional internal
tests when `cfg(test)` is also active. It changes no runtime capability or user
preference. Default host builds and tests require no companion checkout. The
dedicated native interoperability workflow enables the feature explicitly;
ordinary package, native and graphical gates remain unchanged and mandatory.

The workflow checks out the exact companion commit
`a7aefca1a36125986c8ecc662400d4293c39b3cd` (tree
`01cfecb8c2f20be46b644aebb14957a4b9fbb3db`), the source of the official plugins
`v0.2.0-rc.1` release candidate, verifies its public repository identity,
and builds only its `managed-status` **protocol fixture** and labelled
`managed-pages` SDK reducer fixture. Production `clock-managed`,
`system-status-managed` and `pane-pwd-managed` instead come from the separately
verified public Release packages described above; no legacy clock is required.
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

The separately labelled clock cases also require an absolute regular executable
path `AMONG_MANAGED_CLOCK_BINARY` and its verified package-member digest in
`AMONG_MANAGED_CLOCK_SHA256`. Missing/wrong setup fails. The published managed
binary is copied and inspected under explicitly synthetic clock identity metadata
before the real host reviews access and spawns it. That fixture setup is not an
installed Release receipt.

The workflow launches each exact clock case in its own test process with
`AMONG_CLOCK_CASE` and child-only `TZ` setup. UTC/Tokyo output is checked against
actual request-time UTC fences; unset/empty behavior is checked against an
independent system `localtime_r` oracle under the same environment. Both host footer observations
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
CI also checks the exact source tree and native ELF machine. Source-built protocol
fixtures and synthetic receipts do not establish repository installation, current
managed updates/recovery or whole tasks2.1/2.4 acceptance. Published package-byte
verification is a separate boundary from installation and runtime consent.

The separately selected ignored resource-widget case uses the same published
binary with explicitly synthetic local metadata, not an installed Release receipt.
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

## Preferences screenshots

The images in `docs/media/preferences/` come from one ignored graphical test,
`app::plugin_tests::installation::doc_screenshots::every_preferences_tab_is_captured_for_the_documentation`.
It builds a scene from sample official plugins plus one example third-party
source (the catalogue sizes and the example source are synthetic), opens
Preferences in the dark scheme, asserts the content each picture is meant to
show, and writes raw window captures below `AMONG_CHECK_ROOT/preferences`. Run it
like any ignored graphical test, for example
`among-tools ci-x11 <among-test-executable> <test name> --exact --ignored --nocapture`.

`among-tools prepare-preferences-screenshots RAW_DIR NEW_OUTPUT_DIR` then removes
the window border, strips all PNG metadata and fails when a capture is missing,
an output file already exists, an image is 300,000 bytes or larger, or the
widths differ. It never overwrites, so prepare into a new directory; a failed
attempt can leave output files behind, so after fixing the capture retry with
another new directory. When preparation succeeds, copy the ten prepared PNG
files into `docs/media/preferences/`, replacing the existing ones. This is the
only step between the capture and the published files; the host's real user,
paths and desktop never appear because the test runs in the owned display with
its own `HOME`. The tool runs `/usr/bin/convert` with a cleared environment, the
same fixed path the other graphical checks use.

### Site and README media

The differently framed stills and the notification recording used by the
public site and README come from `among-tools media`; see
[documentation media](media.md). That pipeline starts its own owned X11 and
GNOME displays, writes a manifest and checks the published dimensions. The
Preferences captures above remain the in-repository table until its remaining
states are ported to the same scene.

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

## Plugin sandbox (hostile tests)

Managed plugins run under Landlock and a seccomp filter (see
[plugins.md](plugins.md#confinement)). The tests below run on a real kernel; there is
no mock of the kernel. `examples/sandbox-hostile.rs` is a managed-plugin fixture that
attempts, on request, everything a confined plugin must not be able to do. Build it
first: `cargo build --locked --examples`.

```sh
AMONG_REQUIRE_SANDBOX=1 AMONG_HOSTILE_REPORT=<directory> \
  cargo test --locked --bin among -- plugin::sandbox plugin::host::confinement_tests
```

Every action runs twice: once without confinement, the positive control, which must
succeed in the same environment, and once through the launch path every managed
plugin uses. A denial counts as evidence only when its control succeeded. A control
that cannot run (an `io_uring` disabled by sysctl, a file system without user
extended attributes, a kernel without 32-bit entry points) marks that row
`NOT VERIFIED` instead of passing it. `AMONG_REQUIRE_SANDBOX=1` makes a kernel that
cannot give full protection (Landlock ABI below 3) fail the tests instead of skipping
them with a `NOT VERIFIED` line. `AMONG_HOSTILE_REPORT` names a directory that
receives one table per run (`matrix-<arch>.md`, `metrics-<arch>.md`) with the kernel,
architecture, Landlock ABI and the declared outer confinement
(`AMONG_OUTER_CONFINEMENT`, for example `docker-default-seccomp`).

Two actions change priority for the whole process group or user (`setpriority` with
`PRIO_PGRP` and `PRIO_USER` and `who == 0`). Their controls run only when
`AMONG_HOSTILE_CONTAINER=1` is set, which is allowed only inside a disposable
container as a throwaway user, never in a real session:

```sh
docker run --rm --user 4242:4242 -e AMONG_HOSTILE_CONTAINER=1 \
  -e AMONG_HOSTILE_REPORT=/report <image> <test-binary> plugin::sandbox::hostile
```

Without the variable those two rows are `NOT VERIFIED (container-only)`; the filter
semantics for them are also covered by `plugin::sandbox::bpf` unit tests that run the
generated program against chosen system call data.

Other checks in the same area: `plugin::sandbox::launch_tests` (every failing
confinement step, a child stuck before `exec`, a missing or short report, an
allocation between `fork` and `exec`), `plugin::sandbox::data_dir` (links at every
level, foreign owner, permissions, a component swapped during opening),
`plugin::sandbox::level` (the decision table), and `plugin::host::confinement_tests`
(the fixture as a managed plugin: environment, data directory, records, consent,
`system.metrics`). Kernels that Among cannot reach natively (Landlock ABI 1 or 2,
no Landlock) are exercised by injecting the probe result; their real behaviour is
not verified.

`plugin::host::confinement_tests::the_released_official_plugins_still_produce_their_fragments_under_confinement`
runs the released `clock`, `pane-pwd` and `system-status` binaries as managed plugins. It needs
`AMONG_OFFICIAL_PLUGINS=<directory>` holding one extracted release package per plugin identifier
(`clock/`, `pane-pwd/`, `system-status/`). Without the variable it prints `NOT VERIFIED` and passes, except
under `AMONG_REQUIRE_SANDBOX=1`, where it fails; the `native` CI job extracts the published packages of the
runner's architecture and sets both. Only the `native` job of `checks.yml` runs these tests; the package
workflows only build the test executables.

When a kernel or `linux-libc-dev` update adds a system call with a name that touches sockets, processes, signals,
file metadata, mounts, namespaces or privileges, `plugin::sandbox::policy::review_tests` fails until the call is
either added to the filter in `policy.rs` or listed with its reason in `REVIEWED_ALLOWED`. That is the update step
for the filter's call list. The RPM `%check` and the Debian `dh_auto_test` run the whole suite against the headers
of the build machine, so a newer Fedora header set (it carried `open_tree_attr`, now denied with the rest of the
mount family) fails a package build until this step is done.

The same-user IPC rows (`SysV shared memory`, `SysV message queue`, `SysV semaphore set`, `POSIX message
queue`) create the objects in the test process, then show that the unconfined fixture reads them (the control)
and that the confined one is refused with `EPERM`. A control that cannot reach the object marks the row
`NOT VERIFIED`.
