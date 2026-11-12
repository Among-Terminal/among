# Optional command backend

`among::notifications::commands` executes real, optional local command attempts.
Its core API is GTK-free. [Notifications](https://among-terminal.github.io/preferences/#notifications) accepts
and stores explicit configuration; the normal VTE adapter owns registration,
shared-tick polling and retirement. No command is configured or enabled by default.

The inherited procfs PID-truncation finding is closed by the framed-selector
repair and its independent re-review. That disposition is specific to PID
selection; it does not establish packaged self-exec, every exceptional cleanup
path, combined UI acceptance or release readiness.

## Desktop bell notifications

The [desktop notification controls](https://among-terminal.github.io/preferences/#notifications)
document user-facing defaults and behavior. When an upgraded profile has no
`notifications.desktop` entry, the backend uses the default background policy.

Title and body are capped at 128 and 512 Unicode scalar values respectively
(at most 512 and 2048 UTF-8 bytes). These are payload budgets, not measured display
sizes. Invalid UTF-8 becomes replacement characters, whitespace is collapsed,
other C0/C1 controls and `<`, `>` and `&` are removed. Cuts preserve UTF-8 character
boundaries but may split a combining sequence. No OSC 9/777/99 message parser is
installed. The optional command's JSON event remains free of title and directory.

Repeated bells from one terminal use the same notification id. The existing core
allows one attempt per terminal every two seconds, and the desktop adapter allows
one attempt globally per 100 ms. Pending work stays in the core's bounded queue
with its source coalescing and fairness; there is no separate retry queue. A
confirmed missing service discards pending desktop work, so it does not replay
when the service returns. While initial service discovery is pending, work stays
in that same bounded queue and is revalidated before delivery. A registered
freedesktop service or the selected portal route can be activated when it has no
current bus owner.

Among delivers the three desktop routes with direct named D-Bus senders on its GIO
session connection: `org.gtk.Notifications` for GNOME, the
`org.freedesktop.portal.Notification` portal interface when the portal route is
selected, and `org.freedesktop.Notifications` for the freedesktop daemon. It does
not use GIO's `GNotification`/`GApplication.send_notification` backend, because
that backend loses a withdrawal made before the `Notify`/`AddNotification` reply.
Add and Remove calls are asynchronous: the GTK route addresses the discovered
unique owner, while the portal and freedesktop routes address the well-known name,
and the freedesktop server id stays bound to the unique owner that returned it.
The route is selected before the first delivery and kept for the process. Closing
or viewing the originating terminal, a policy edit and normal exit withdraw its
outstanding notification, including a send whose reply arrives after a
withdrawal. Normal exit drains real freedesktop and named cleanup inside one
absolute 30-second budget, and a held reply does not extend it. A viewed terminal
never notifies, even with Always selected. Desktop delivery does not request sound;
the audio channel retains its own policy, and a daemon may have its own sound
preferences.

The click action binds the backend's random incarnation token and the core's
session, generation and pane identifiers. Closed
or stale targets only raise the current window; they do not select a replacement
terminal. The sidebar's collapsed state is preserved.

Among can report that no session notification service is available, but a
successful D-Bus call cannot confirm display or
report every shell refusal. See [desktop-entry requirements](https://among-terminal.github.io/install/#desktop-notification-integration)
for binary archives and AppImages. Notification clicks are handled by the running
application; restarting it through a notification is not enabled.

API references: the
[freedesktop notification specification](https://specifications.freedesktop.org/notification-spec/latest/)
documents `Notify`, `CloseNotification`, server ids, replacement and the absence
of a display guarantee; the
[portal notification interface](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Notification.html)
documents `AddNotification` and `RemoveNotification`; the
[`org.freedesktop.Application` D-Bus interface](https://docs.gtk.org/gio/class.Application.html#dbus)
documents the `ActivateAction` call that click handling uses.

## Configuration and admission

- `Configuration::new` accepts an absolute UTF-8 executable path and literal
  string arguments. It rejects NUL, relative executables, more than 128 arguments,
  or more than 16 KiB of combined executable/argument text. Inspection performs
  no execution. There is no template expansion, PATH search, implicit interpreter
  or event-to-command-line interpolation.
- `OwnedDirectory::open` opens an existing, owner-only `0700` directory with
  `O_NOFOLLOW`. Perform this filesystem validation off the UI loop. The runtime
  retains its descriptor; renaming/replacing the path cannot redirect command
  cwd into a terminal directory. It does not create or migrate preferences.
- `Runtime::new` takes that directory and the trusted host executable (Among's
  own executable, not the user-configured command). `register` creates a core
  channel off with application-background eligibility. Registration and
  configuration inspection never start a process.
- `Runtime::unprepared` supports inert stable registration before directory I/O;
  `prepare` accepts an already validated descriptor. The app requests preparation
  only after a new eligible event, using one bounded process-wide worker. Its
  cwd/HOME is `$XDG_DATA_HOME/among/notification-command-home` (or the standard
  HOME fallback). Explicit later edits can retry failed preparation, never the old
  event. Opening, inspecting, saving or enabling alone does not prepare it.
- `set_policy` explicitly enables/adopts a policy. `poll(&mut Host, Instant)`
  performs host-only nonblocking admission and completion collection; managed work
  requires `poll_authorized` and an owned `Backend::authorize_delivery` snapshot.
  Pending work remains
  in the core's globally bounded/fair queue; there is no second delivery backlog.
- `replace`, `remove`, `shutdown`, runtime drop and source/session revocation
  invalidate old work. Re-enabling or replacing configuration never replays a
  previous event. A new configuration requires a new eligible event.

Eligibility is snapshotted at host ingress. Source lifetime and policy/configuration
generations are checked again at dispatch, immediately before execution permission
is sent to a ready supervisor, and during the active attempt. Workers use atomic
revocation leases, not GTK widgets or borrowed host state. Targetless events still
never match originating-terminal-unfocused policy; their command payload has a
null source rather than a substituted selected terminal.

## Command input and environment

Stdin receives one JSON document, a newline, then EOF. The 16-KiB bound
includes the newline. Its schema is fixed; there is no arbitrary metadata field:

```json
{"schema":1,"kind":"terminal-bell","event":{"session":1,"id":2},"source":{"session":1,"generation":1,"pane":3}}
```

For a host application event, `kind` is `application` and `source` is `null`.
Managed events use `kind: "managed-notification"` (the local command wire spelling,
not the provider profile's `managed_notification`) and add
`origin: {"identity":"github:12/sample","generation":"producer-generation"}`.
The origin is taken only from current backend delivery authority; missing authority
is a refusal, never a host-origin fallback. Pane-targeted managed events retain
their source; targetless managed events retain `source: null`.
Identities are process/session-local host values, not saved addresses. No PTY text,
typed input, title, cwd, process details, terminal command line, executable,
arguments or environment is included in the event document.

The executed command receives `PATH=/usr/bin:/bin`, `LANG=C.UTF-8` and `HOME`
pointing to the actual owned cwd, and the session locations a notifier needs:
`XDG_RUNTIME_DIR`, `DBUS_SESSION_BUS_ADDRESS`, `DISPLAY`, `WAYLAND_DISPLAY` and
`XAUTHORITY`. Each of those is passed through from Among's own environment only
when it is set there, non-empty, at most 4 KiB and free of a NUL byte; an absent
one is absent for the command too. Nothing else is passed. It does not inherit
the caller's tokens, SSH agent, terminal environment or credentials. Non-stdio
descriptors are marked close-on-exec. Required secrets are unsupported; there is
no invented credential substitute or hidden credential-forwarding API.

The program nevertheless has the user's ordinary filesystem/network authority.
It can deliberately invoke another program or access files allowed to that user.
The session variables above are themselves authority: with the session bus and the
display, the command acts with the user's session rights, for example it can ask
the keyring or a user service over D-Bus, or talk to the display server. "Does not
inherit credentials" means no token variable or agent socket is copied, not that
the command is cut off from what the session can reach.
Process supervision and environment minimization are not an OS sandbox.

## Process ownership and budgets

A single fixed off-main-loop I/O service is shared by runtimes in one host
process. It reserves at most two attempts globally and one per channel, including
queued admission and cleanup. No per-command host worker thread is spawned or
abandoned. The service's nonblocking attempt state machines handle pipes,
cancellation, deadlines and supervisor reaping; GUI callers never join/wait.

Each attempt execs a fresh copy of the trusted host in an internal supervisor
mode, before GTK/GApplication initialization. Only that process becomes a Linux
subreaper; plugin children and application-wide child reaping are untouched.
The supervisor waits for a ready/permission handshake before starting the actual
configured executable in its own process group. It writes JSON stdin to EOF and
drains both output pipes without retaining or logging child output.

| Boundary | Enforced limit |
|---|---|
| Pending jobs | Core global 64 / per-channel 16, coalesced per source |
| Active attempts | Global two / per-channel one, capacity held through reap |
| Attempt deadline | Ten seconds, including supervisor admission/startup |
| Combined stdout + stderr | 32 KiB, not 32 KiB for each stream |
| Command stdin event | 16 KiB |
| Diagnostic history | Core's shared 32 fixed-size records, including command completions |
| Stop grace / cleanup observation | 250 ms TERM grace, KILL, two-second cleanup observation |

The supervisor keeps the leader PID reserved through its final group signal,
then reaps the leader and adopted descendants. Inherited pipes and TERM-ignoring
descendants do not turn local completion/cancellation into an endless pipe wait.
Adopted direct children are also stopped within that isolated supervisor; it does
not scan or signal application/plugin/unrelated processes.

Each child-list pass reads at most 64 KiB, using a 4-KiB scratch buffer and bounded
read attempts. A PID is selected only after ASCII whitespace or an actually
observed EOF completes its positive decimal record. A byte cap, exhausted read
budget or read error never completes a numeric suffix; an exact-cap undelimited
token is conservatively discarded, even if the underlying file ends there.
Short reads and interruptions retain the token only within that pass. It is never
carried across reaping or joined to a newly opened child list. Each pass signals
before its at-most-256 nonblocking reaps; subsequent fresh scans cover remaining
reapable children as the prefix shrinks, without increasing the byte cap or
signalling a guessed PID prefix.

`Host::command_completions` reads results from the shared bounded diagnostic ring:

- `Exited(code)` / `Signaled(signal)` describes only known local process exit.
  Exit zero is not delivery to a remote service, and nonzero exit is not proof
  that no external effect occurred.
- `NotStarted(reason)` describes an attempt denied or failed before execution
  permission/launch.
- `Uncertain(reason)` covers timeout, output overflow, active cancellation,
  lost supervisor or incomplete cleanup. An external effect may already exist.

Reasons are fixed codes. No child output or command settings are dumped. Results
never re-enter bell ingress. There is no automatic retry, replay, rollback claim
or empty-success delivery path.

## Cancellation and integration limits

`shutdown` removes pending channels and requests active cancellation immediately.
Runtime drop also revokes channel admission and active leases without joining;
the core removes revoked registrations on its next housekeeping operation.
The app's removal handler retains a GApplication hold and polls until ordinary
`active_count()==0` and any owned directory preparation has finished, alongside
the existing plugin retirement gate. GTK never waits for a child or joins a
thread. Replacement windows do not abandon old reaping ownership.
This describes command ownership only. The supervised audio teardown repair is
joined: the original GTK-stall repair passed independent recheck, but a separate
helper pre-play cancellation race remains open. The owner accepts discarded Busy
requests with explicit retry after confirmed retirement. Command retirement and old ordinary audio captures do not
establish app-wide stalled-media acceptance.

No userspace supervisor can promise that SIGKILL instantly ends a kernel
uninterruptible task. Such cleanup retains the isolated supervisor/global slot
and reports uncertainty instead of replacing it with more commands or blocking
GTK. If a supervisor is lost after execution permission without trustworthy reap
confirmation, its capacity is quarantined for the process lifetime. A deliberately
hostile same-user program can attack its supervisor; this is not a security
boundary against that authority. Unknown effects are never retried. Unconfirmed
retirement keeps the hold, logs that cleanup is unconfirmed, and does not silently
choose an irreversible forced-exit policy.

This implementation requires Linux subreaping, procfs and `close_range` with
close-on-exec support. Native execution is tested. AppImage loader paths under
the deliberately cleared environment and Snap confinement permission for these
operations still need concrete packaging validation; no packaging changes or
packaged-command delivery claims are made here. Managed provider delivery and
broader lifecycle/platform acceptance remain separate boundaries.

The ordinary application's command adapter consumes owned backend delivery
authorization without holding a mutable core borrow. Missing/expired/mismatched
authority is refused and consumed, not retained for replay. The asynchronous
service retains the snapshot, reads its live OriginLease and expiry alongside the
existing source/channel/session flags before spawn and supervisor grant, and
repeats those checks during active work. This wiring does not establish independent
combined review, packaged execution or release acceptance.

## Evidence boundary

`tests/notification_commands.rs` compiles a synthetic Rust fixture and drives
real local processes. It checks literal metacharacters, exact stdin/EOF/env/cwd,
secret-FD exclusion, no implicit shell on invalid executable format, revoked
generations, targetless policy, concurrency, finite queues/diagnostics, a real
ten-second timeout, combined output overflow and descendant/supervisor reaping.
`AMONG_COMMAND_TEST_ROOT` selects an explicit evidence directory; otherwise files
remain under `target/notification-command-tests`.

`tests/notification_origin_commands.rs` exercises actual managed pane/targetless
stdin kind/origin, missing/expired authority and independent worker-side revocation.
It is a separate executable so its attempts cannot distort the original suite's
process-wide capacity assertions; the original suite remains parallel and unchanged.

Pure cleanup regressions cover the reviewer's 9,005 mixed-width synthetic child
identities across 64 KiB, framing/EOF/read-error boundaries and malformed tokens.
The production `Cleanup::poll` body is also driven through bounded passes with
synthetic I/O: every positive signal target must belong to that child set, all
9,005 reapable identities must progress, and the reserved leader's final group
signal must precede reaping and never recur. Those tests send no real signals
and do not create a large process tree; the separate hostile-process suite above
observes actual owned-process behavior with unchanged production limits.

The ignored native test
`app::bell_tests::hostile_command_backend_preserves_real_terminal_input_and_nonblocking_shutdown`
uses the cached Rust fixture built by the ordinary integration test. Under an
isolated X11 harness it configures a test channel on the real bell host and polls
this API with a GTK timer. A real semantic VTE bell dispatches the command while
keyboard input continues in a sibling terminal during a stubborn command tree.
Host access/configuration in that older test is test-only. The separate ignored
`app::notification_ui_tests::general_controls_drive_production_commands_and_application_retirement`
drives actual General keyboard controls, normal VTE registration/polling, lazy cwd
preparation and a real no-window GApplication run held through small-tree cleanup.
The PID selector has separate repair/re-review evidence; neither native test
establishes packaged delivery or every exceptional cleanup path.
