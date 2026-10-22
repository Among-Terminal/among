# Managed notification backend — explicit integration only

This is an implemented backend boundary, **not an enabled ordinary-app feature**.
Normal `PluginHost::new` / `with_start_gate` construction offers no notification
roles. A required notification role refuses execution there; optional roles are
omitted. General controls, durable provider policy, contribution widgets and normal
application wiring remain separate integration work. Compilation or serialization
does not establish native notification acceptance.

`src/plugin_notifications.rs` is GTK-free state and shared-core scheduling.
`src/plugin/host/notification_backend.rs` connects it to actual supervised managed
processes. The unshipped `examples/notification-plugin.rs` fixture records both
directions of real stdin/stdout, including jobs and results.

## Explicit host contract

The internal constructor is:

```rust,ignore
let bridge = Rc::new(RefCell::new(Backend::new(core.clone(), actual_focus)));
let plugins = PluginHost::with_notification_backend(start_gate, Some(bridge.clone()));
```

`core` is the **same** `Rc<RefCell<among::notifications::Host>>` used by semantic
bell ingress and other delivery adapters, not another event queue. Supply actual
application-owned-window/terminal focus and update it with `Backend::set_focus`.
Register/detach real source lifetimes on that core and update the plugin host's
existing stable `World` context. Never derive keyboard focus from remembered
selection or inspect PTY output. Do not hold bridge/core borrows across host calls.
The bridge binds to one plugin-host session and refuses a different session.

Only a bound, usable bridge expands the supported feature set. Managed admission
still requires a valid receipt, publisher/access review, explicit desired
enablement, declared features, matching session/process generation and runtime
negotiation. Grants do not sandbox ordinary user-level plugin processes. Required
secret settings remain unsupported; no credential storage or environment expansion
is introduced here.

Static channel declarations populate `Backend::inventory()` without starting code.
Each `ChannelKey` contains authoritative `identity` (`github:<repository-id>/<id>`)
and channel ID; names are not keys. `set_policy(key, Policy)` changes effective
channel policy only, never plugin enablement. New channels are off with background
eligibility. Views distinguish saved policy, installation, desired plugin switch
and current admitted owner. Disabled/unavailable choices remain in memory.
Persistence is the future General/storage consumer's responsibility, not a claimed
operation of this backend.

Disable/fault/host destruction revoke actions, contributions, provider registration
and active jobs. Installer integration must also call `Backend::uninstall(identity)`
when authoritative registration is removed, after stopping/revoking the host's
registration. That operation retains channel choices but removes admission
authority: an old registration's later restart is refused before executing code.
Only an explicit new static registration restores installation authority.
This does not implement the installer's update/uninstall transaction.
The process owns an independent revocable admission lease: stopping clears it even
when a renderer temporarily holds a bridge snapshot borrow. Current snapshots and
action routing check that lease, rather than waiting for mutable UI bookkeeping.

## Negotiated producer operation

After `notifications.produce` admission, a plugin may send:

```json
{"type":"notification_event","session":"host-session","generation":"current-process-generation","event":{"sequence":1,"target":{"kind":"application"},"message":"Synthetic application event"}}
```

Targetless application events never invent a pane/workspace. Always policy can
match, background requires no active application-owned window, and originating-
terminal-unfocused never matches. Existing core acknowledgement rules are unchanged.
Legacy `notify` is still a generic payload-omitted diagnostic, not this ingress.

Pane events additionally require `context.terminals`, a live canonical string pane
reference, and `source_generation` matching the current core source. Authorized
status/context pages include each attached terminal's `notification_generation`
when production or UI is negotiated. Closed/reused sources and stale plugin
session/generation cannot redirect events. A workspace alone is not a terminal
source and is refused for event production. The host resolves workspace membership;
plugins cannot supply origin, causal/event IDs or source ownership.

`sequence` is a positive, increasing per-process `u64`. Authenticated fresh
attempts have a two-second producer cooldown, independently of per-source core
coalescing. Text is nonempty, at most 240 characters/1024 UTF-8 bytes, with no
control characters or bidi overrides/isolates. Diagnostics contain fixed reasons,
not event text, configuration payloads or plugin free-form result text.

The bridge stamps an envelope with event/root causal ID, core session, authoritative
plugin identity/process generation, optional source-generation/workspace identity,
and kind `managed_notification`. Delivery-linked production (`causal_job`) is
rejected, not recursively routed. **Fail-closed dual-role rule:** after a process
generation has been assigned a delivery attempt, further production from that generation
is rejected even if it omits `causal_job`. It can still deliver and contribute UI.
This conservative restriction prevents disguised feedback; it is a contract-review
point, not a claim of arbitrary dual-role workflow support. Assignment is conservative
even if enqueue later fails; it is not a claim that a provider received bytes.
Self-delivery is omitted.
Results never become notification events. No automatic retry or reconnect replay.

## Contribution and action state

With `notifications.ui`, send `notification_contribution` with session/generation
and a `contribution` object containing:

- `id`, increasing positive `revision`, `scope` in existing plugin areas;
- nonempty `text`, optional `tooltip`, `icon`, closed `style` and up to four actions;
- for pane scopes, the current `source_generation` from authorized context.

Scopes use the same `slot`/`target` shape as managed status. Application scopes are
`app_header`/`app_footer`; workspace scopes are `workspace_header` and
`workspace_summary`; pane scopes are `pane_header`/`pane_footer`. Non-application
scopes require terminal context and current host resolution. There is no title,
Restore-control, workspace-hue, arbitrary host-action, HTML/widget/script/CSS scope.
Text is plain data, not markup. Each text/tooltip fits 512 characters/2048 UTF-8
bytes. Tooltip alone permits newline/tab. Styles are `default`, `success`,
`warning`, `error`, `accent`, `dim` and affect contribution data only.

Icons are `theme:<bounded ASCII theme name>` or `package`, which selects the
statically inspected package artwork snapshot. No wire-selected filesystem path,
URL, SVG or executable image loader is accepted. Package PNGs reuse the existing
1-MiB/2048x2048 static/chunk/decode validation; malformed optional artwork falls
back to no icon. The bridge does not decode arbitrary images from a wire message.

Action IDs are bounded local IDs and labels fit 64 characters. Snapshot
`Contribution::actions` supplies **opaque** `ActionHandle`s. Explicit host activation
calls `Backend::activate(handle, now)`; the next plugin-host poll sends
`notification_action` with contribution ID/revision/action only to that current
owning process. No shell, terminal-input or host-command routing exists.
Replaced/withdrawn/expired/disabled/restarted actions are inert, including already
queued activations. Activation has a 250-ms per-contribution cooldown.

`withdraw_notification` with session/generation/`id` withdraws one contribution.
Current applied settings acknowledgements also invalidate old contribution/action
state without releasing old status/configuration/delivery reply credits.
`contributions(now)` is a renderer snapshot, **not a widget renderer**. Its consumer
must preserve existing title/Restore/stripes and implement native accessibility;
those presentation gates remain open.

## Actual provider jobs and outcomes

An admitted `notifications.deliver` process receives only declared, explicitly
enabled eligible channels. A job is:

```json
{"type":"deliver","session":"host-session","generation":"provider-generation","delivery":{"job_id":7,"channel":"first","event":{"event_id":"12","causal_id":"12","core_session":"1","kind":"managed_notification","origin":{"identity":"github:12/sample","generation":"producer-generation"},"source":null,"message":"Synthetic application event"}}}
```

`source` stays null without provider terminal-context consent; it never contains
PTY output, input, cwd, title, process command line, inherited environment or TTY
metadata. A permitted source contains pane, source generation and workspace string
references. Provider identity is determined by the real process receiving the job,
not by a reply claim.

Reply with `delivery_result`, matching session/generation and `job_id`, and outcome
`accepted`, `refused`, `failed` or `uncertain`. Accepted is the provider's local
claim, not proof of remote receipt or exactly-once delivery. Wrong, duplicate, late
and revoked results cannot replace recorded outcomes. Policy/source/provider
revocation cancels active authority and sends bounded `cancel_delivery` messages
where the same process remains running; external side effects are not reversible.
Timeout/cancellation is recorded as uncertain, never retried. A provider that
retains an unanswered reply beyond five seconds is faulted/stopped, including a
cancelled job whose credit is still retained.

## Resource and lifetime limits

| Boundary | Implemented limit |
| --- | --- |
| Shared core delivery queue | 64 globally / 16 per channel, coalesced by source |
| Active provider attempts | Two per bridge / one per channel, no second pending-job queue |
| Delivery request payload | 16 KiB including host envelope |
| Provider response deadline | Five seconds; existing off-loop two-second write deadline |
| Process wire/output | Existing 64-KiB frames, depth 32, 16 queued frames/1 MiB, rotating bounded drains |
| Replies | Status + configurations + deliveries share **15 retained credits**; enumeration retains its reserved slot |
| Event metadata | 64 envelopes / ten seconds; evicted/unobserved jobs are dropped, never reattributed |
| Contributions | 128 globally / eight distinct IDs per process, including revision tombstones; ten-second lifetime |
| Actions | Four per contribution; 16 pending activations |
| Provider inventory | At most 128 installed identities / 4096 retained channel rows |
| Backend diagnostic records | 16 completions + 16 fixed rejections; existing core diagnostics remain separate |

Provider disable removes its core channels/queued work immediately. Producer
revocation deletes its envelopes; any already coalesced core entries lose authority
and are discarded on bounded dequeue, not delivered under a fabricated origin.
Timers, envelopes/jobs and action handles are session-only, never saved sessions.

The frozen core's channel-full policy drops an incoming distinct source once that
channel already has sixteen pending sources. Single-source floods coalesce and a
quiet sibling progresses; this is **not** a guarantee that every new distinct
source survives an already saturated channel. Core suppression diagnostics expose
that overload. Changing the core's admission fairness is a separate review point.

## Sequential integration requirements and local checks

1. Supply real focus/source lifetimes and the same core, then construct the host
   with the explicit bridge. Update the ordinary-app feature set only then.
2. On actual core bell/application ingress call `observe_host_event(event,
   current_workspace)` immediately. Never recover missing envelopes by guessing.
3. General must consume typed inventory and call `set_policy` after accepted edits,
   with durable identity/channel-scoped storage and unknown-data preservation.
4. Render contribution snapshots only in the existing plugin areas and activate
   opaque handles. No native contribution/action widget acceptance is claimed here.
5. The frozen core names its source-bearing ingress `bell`/`Kind::TerminalBell`.
   Managed provider envelopes correctly say `managed_notification`; later command/
   app adapters **must use `Backend::envelope(event)`**, or extend the core's typed
   origin contract, rather than relabel plugin events from that old core enum.
   The command backend is not duplicated or changed by this slice.
6. Join installer registration removal/replacement with host stop/reap and bridge
   uninstall/re-registration. Do not resurrect consent from retained settings.

Run real process/model tests after building the fixture:

```sh
CARGO_BUILD_JOBS=2 cargo build --locked --example notification-plugin
CARGO_BUILD_JOBS=2 cargo test --locked plugin::host::notification_backend -- --nocapture
CARGO_BUILD_JOBS=2 cargo test --locked --test plugin_notification_model
```

The ignored `native_pty_bell_real_provider_and_keyboard_remain_responsive_during_flood`
test requires `among-tools ci-x11` with its owned display/private HOME/XDG profile.
It tests an explicit backend plus real VTE PTYs/provider/keyboard, **not** ordinary
app/General/rendering integration. Full three-package gates, notices, strict specs,
independent review and latest-head GitHub CI are separate acceptance requirements.
