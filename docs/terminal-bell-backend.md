# Terminal bell backend

This is a backend integration boundary, **not a finished notification UI**.
The host receives VTE's semantic bell signal, retains session-local attention,
and acknowledges actual terminal viewing. It does not yet render unread stripes
or play sound, run notification commands, or invoke managed notification providers.
There are no notification Preferences controls or saved notification settings in
this implementation. VTE's independent audible bell is explicitly disabled for
attached terminals; no fallback beep bypasses the future host audio policy.

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
callback cannot redirect to a reused pane number. Closing the main window drops
the entire backend session. None of this state enters saved terminal sessions.

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
not start any code. There are no delivery adapters registered by the application
yet. An adapter must supply real admission and map its authoritative external
channel identity to the opaque core handle.

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

## Pending integration

- Native stripes, accessible/static unread cues and reduced-motion presentation
  must consume `attention` / `aggregate`; none are rendered here.
- Sequential Preferences integration must supply accepted policies, persistence
  and controls. Current app wiring uses only fresh defaults.
- Audio needs validated immutable WAV inputs, a single owned asynchronous player
  and observed packaged output. Explicit preview must be a separate operation,
  usable while event-driven audio is off, not an event sent through this host.
- Commands need actual supervised off-loop execution, bounded JSON stdin/EOF,
  literal argv, allowlisted environment/owned cwd, concurrency, deadline/output
  budgets and group cleanup. There is no command configuration or executor here.
  Process supervision must not be described as an OS sandbox.
- Managed provider/producer authorization and real delivery belong to the separate
  plugin API/runtime integration, not this host core. No provider success is faked.

These boundaries require their own tests; core queue/policy tests do not establish
audio output, command supervision, provider delivery, UI acceptance or release
readiness.
