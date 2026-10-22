# Optional command backend

`among::notifications::commands` executes real, optional local command attempts.
Its core API is GTK-free. [General](preferences.md#general-notifications) accepts
and stores explicit configuration; the normal VTE adapter owns registration,
shared-tick polling and retirement. No command is configured or enabled by default.

**Security acceptance is blocked:** independent review found that the inherited
procfs child parser can treat a truncated PID suffix as a complete PID. Its
dedicated repair/re-review is pending; ordinary small-tree tests are not approval
of that path. The common UI work does not repair or bypass the parser.

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
  channel **off** with application-background eligibility. Registration and
  configuration inspection never start a process.
- `Runtime::unprepared` supports inert stable registration before directory I/O;
  `prepare` accepts an already validated descriptor. The app requests preparation
  only after a new eligible event, using one bounded process-wide worker. Its
  cwd/HOME is `$XDG_DATA_HOME/among/notification-command-home` (or the standard
  HOME fallback). Explicit later edits can retry failed preparation, never the old
  event. Opening, inspecting, saving or enabling alone does not prepare it.
- `set_policy` explicitly enables/adopts a policy. `poll(&mut Host, Instant)`
  performs nonblocking admission and completion collection. Pending work remains
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

Stdin receives one JSON document, a newline, then **actual EOF**. The 16-KiB bound
includes the newline. Its schema is fixed; there is no arbitrary metadata field:

```json
{"schema":1,"kind":"terminal-bell","event":{"session":1,"id":2},"source":{"session":1,"generation":1,"pane":3}}
```

For an authorized targetless event, `kind` is `application` and `source` is `null`.
Identities are process/session-local host values, not saved addresses. No PTY text,
typed input, title, cwd, process details, terminal command line, executable,
arguments or environment is included in the event document.

The executed command receives exactly `PATH=/usr/bin:/bin`, `LANG=C.UTF-8` and
`HOME` pointing to the actual owned cwd. It does not inherit the caller's tokens,
SSH agent, terminal environment, display/session bus or credentials. Non-stdio
descriptors are marked close-on-exec. Required secrets are unsupported; there is
no invented credential substitute or hidden credential-forwarding API.

The program nevertheless has the user's ordinary filesystem/network authority.
It can deliberately invoke another program or access files allowed to that user.
**Process supervision and environment minimization are not an OS sandbox.**

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
  Exit zero is **not** delivery to a remote service, and nonzero exit is not proof
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
This describes command ownership only. The inherited audio backend has a separate
native-teardown blocking finding; app-wide stalled-media responsiveness is not
established by the command hold or ordinary audio output tests.

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
the dedicated PID safety repair/re-review remain separate acceptance boundaries.

## Evidence boundary

`tests/notification_commands.rs` compiles a synthetic Rust fixture and drives
real local processes. It checks literal metacharacters, exact stdin/EOF/env/cwd,
secret-FD exclusion, no implicit shell on invalid executable format, revoked
generations, targetless policy, concurrency, finite queues/diagnostics, a real
ten-second timeout, combined output overflow and descendant/supervisor reaping.
`AMONG_COMMAND_TEST_ROOT` selects an explicit evidence directory; otherwise files
remain under `target/notification-command-tests`.

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
Neither test establishes the pending PID-parser security boundary or packaged delivery.
