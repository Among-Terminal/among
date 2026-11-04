# Terminal bell backend

The host receives VTE's semantic bell signal, retains session-local attention and
acknowledges actual terminal viewing. Pane headers and workspace rows consume
that state in place, including collapsed rows and rename, without rebuilding on
every bell. Stripe opacity pulses briefly; a static `!` and accessible names do
not depend on colour or animation. System reduced-motion changes are read on the
shared tick without discarding unread state or changing workspace hue/terminal size.

[General Preferences](preferences.md#general-notifications) controls accepted
visual/audio/optional-command policies and persistence. One shared application
[audio owner](audio.md) consumes admitted events and explicit preview separately.
The normal bell backend registers/polls the optional [command executor](notification-commands.md)
when configured. The application also explicitly attaches the managed notification
backend to this same core; no second scheduler is introduced. This test-branch
integration remains subject to the separate API/core and causal-contract blockers.
VTE's independent audible bell is disabled; no fallback beep bypasses host policy.

## Live source and focus

`src/app/bell.rs` connects each persistent terminal once, outside header rebuilding.
It stamps events synchronously at the VTE signal through the GTK-free
`among::notifications::Host`. It does not inspect PTY bytes, titles, command lines,
working directories, terminal input, or shell exits for notification content.
An OSC title sequence's BEL terminator is left to VTE's parser, not treated as a
bell by the host.

Each source token contains a process-local session identity, a target generation
and its pane number. The host mints tokens; a pane number alone cannot reconstruct
one. Removal revokes its attention, queued jobs and dequeued-job validity. A late
callback cannot redirect to a reused pane number. Closing the main window revokes
its sources/channels while retaining only asynchronous cleanup ownership through
ordinary reaping. None of this state enters saved terminal sessions.

Viewing means the terminal itself has keyboard focus, is mapped, and belongs to
the active main window. A mapped native popover blocks acknowledgement. Header
focus, remembered selection, model workspace switching and container rebuilding
alone do not acknowledge anything. Rebuilding or switching may subsequently
return **real** terminal focus; that acknowledges only the focused source, never
its hidden siblings. Focus signals and the existing shared tick observe this
without moving focus themselves.

Application foreground includes an active application-owned window or a window
whose transient chain reaches the main window. This includes the current plain
Preferences window, even though it is not registered with `GApplication`.
An unrelated active window is not counted just because it shares the process.

## Core policy and bounded work

`src/notifications.rs` has no GTK types. Visual attention defaults to enabled /
originating-terminal-unfocused. Independently registered delivery channels start
with `Policy::DELIVERY_DEFAULT` (off / application-background); registration does
not start any code. Audio consumes current admission with zero pending playback;
commands retain the bounded core queue. External provider adapters must map their
authoritative identity/channel to an opaque core handle, never a display name.

Every channel independently supports always, originating-terminal-unfocused and
application-background. Eligibility is snapshotted at ingress. An authorized
targetless application event never matches terminal-unfocused policy and never
creates pane or workspace attention. The core does not itself authorize external
producers; callers must establish that authority before calling ingress.

The core retains one attention record per live source. A visual event pulses for
one second, with a two-second source cooldown. Repeated bells do not extend the
pulse. A viewed always-eligible source can pulse without becoming unread.
Disabling visual policy clears existing emphasis; re-enabling does not replay it.
Workspace aggregation reads current surviving members supplied by the workspace
model, never retained list indices, names or colours. No widgets or timers own
attention state.

Delivery scheduling coalesces one pending event per source/channel, including a
single targetless origin. It retains at most 64 jobs globally and 16 per channel.
Replacement retains the job's queue position; dequeue skips sources in their
two-second cooldown, allowing another source to progress. Overflow is dropped,
not retried. If one origin's fan-out monopolizes the global budget, its newest
excess jobs give way to a less represented origin. Diagnostics retain at most
32 fixed-size records, coalescing adjacent
identical reasons, with no arbitrary payload text or diagnostic re-ingress.

`take_ready` consumes a job: it is permission to **attempt** an effect, not a
delivery receipt. An adapter must check `is_current` before acting and cancel its
owned active operations when the source, channel or policy is revoked. Policy
changes clear queued jobs and invalidate already dequeued tokens. Re-enabling a
channel never replays old work. The core cannot undo a completed external effect.

The inherited command PID-truncation finding is closed by the separately authored
framed-selector repair and its independent re-review. That is not broader
application, packaging or release approval.

## Open acceptance boundaries
- Commands retain capacity through cleanup. Ordinary retirement holds GApplication
  and polls without a join; unconfirmed cleanup/quarantine keeps the hold and logs
  uncertainty rather than inventing rollback, retry or an irreversible forced exit.
- Managed provider/producer authorization belongs to the joined plugin backend.
  Active-envelope eviction, targetless origin fairness and the dual-role contract
  are independently open. Managed-origin command attempts are refused rather than
  encoded from the old core kind; the API author owns the typed-core follow-up.
- The supervised audio decoder repair is joined, including both early helper
  dispatches and post-loop shutdown confirmation. Independent safety/platform
  review remains outstanding; the owner separately accepts discarded Busy requests
  and explicit retry after confirmed retirement. No
  rejected Preview is queued or replayed, and accepted sound selection is not a
  playback receipt. New-source native controls/captures are required; older output
  and normal cancellation do not establish this repair's stalled boundary.
- Packaged command self-exec and the full audio platform/release matrix remain
  separate from native General/UI observations.
- Installer fresh-token authority has a separately assigned L1 remove/reinstall
  finding. Current-host/nonmodal approval does not establish that boundary. Await
  the exact reviewed installer repair and actual combined regression; no mutable
  installer checkpoint is incorporated here.

These boundaries require their own tests; core queue/policy tests do not establish
audio output, command supervision, provider delivery, UI acceptance or release
readiness.
