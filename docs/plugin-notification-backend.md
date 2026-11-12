# Managed notification backend

This is the backend boundary between the shared notification core and managed
plugin processes, and its integration into the application. Plain
`PluginHost::new` / `with_start_gate` construction offers no notification roles;
the application attaches the backend with `with_notification_backend` and its VTE
core at startup. General reads the declared full-identity/channel inventory and
persists choices independently of plugin enablement, retaining unavailable choices
and unknown data. A plugin holds a notification role only through the explicit
role review described below. Plugins installed from a local catalogue under the
development flag (`development/<plugin>`) register like release installations.

Implemented and covered by tests that run in CI against real plugin processes:

- active provider envelopes are retained under bounded fresh-source traffic
  (`active_provider_envelope_is_retained_under_bounded_fresh_source_traffic`);
- targetless producers coalesce and cool down by their exact registered origin and
  share the core's bounded, fair queue
  (`targetless_origin_fanout_uses_the_same_bounded_fair_global_queue`);
- the producer and delivery roles are strictly separated at review, preflight and
  admission, and a plugin that requires both is refused without execution;
- the ordinary application, General and the renderer are exercised together by
  `ordinary_app_provider_controls_and_declarative_actions_use_one_live_core` and
  `combined::every_channel_keeps_its_own_policy_failure_and_restart`.

Recorded decisions of 2026-11-04: the audio helper's 250 ms exit limit after the
end of its protocol stays as it is (a helper that does not leave in time is killed
and the sound reported failed; it fails closed), and `package-source-delivery` is
not part of the 0.2.0 release candidate. Installed DEB and RPM output is observed
only at the release-candidate tag.

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

`core` is the same `Rc<RefCell<among::notifications::Host>>` used by semantic
bell ingress and other delivery adapters, not another event queue. Supply actual
application-owned-window/terminal focus and update it with `Backend::set_focus`.
Register/detach real source lifetimes on that core and update the plugin host's
existing stable `World` context. Never derive keyboard focus from remembered
selection or inspect PTY output. Do not hold bridge/core borrows across host calls.
The bridge binds to one plugin-host session and refuses a different session.

Only a bound, usable bridge expands the supported feature set. Managed admission
still requires a valid receipt, publisher/access review, explicit desired
enablement, declared features, matching session/process generation and runtime
negotiation. Notification grants authorize protocol operations only; the sandbox
policy has no notification rule, so a grant never widens what the process may do
at the operating-system level. Every managed plugin runs under the confinement
described in [Confinement](plugins.md#confinement), which also bounds these
roles: a delivery provider cannot open a socket, so it cannot reach a network
service or the session bus itself, and a provider that needs one needs an access
grant that does not exist yet. The confinement does not limit what a plugin sends
over the protocol, so the limits in this document still apply to every message.
Required secret settings remain unsupported; no credential storage or environment
expansion is introduced here.

### Explicit production/delivery role choice

A process may hold either `notifications.produce` or
`notifications.deliver`, never both. `notifications.ui` remains independent and may
coexist with either. A manifest requiring both conflicting roles is refused before
executing its entrypoint. When both are optional, the existing identity/publisher-
scoped access review must explicitly grant one or neither in `review.features`;
selecting both refuses admission, without silently preferring or removing a grant.
Ordinary unsupported optional-feature omission still applies.

General can validate a proposed grant set with
`managed::notification_role(&BTreeSet<String>) -> Result<Option<NotificationRole>,
NotificationRoleConflict>`, and required declarations with
`required_notification_role(&Manifest)`. Conflicts distinguish `RequiredRoles`
from `ReviewChoice` and provide a finite `problem()` message. The existing
`review_access` / preflight / runtime feature checks consume this same validation;
the actual hello contains only the explicitly chosen supported role. Static
overview reports impossible required-role declarations as unavailable.
`PluginHost::reviewed_notification_role(name)` reports the validated retained
choice; `admitted_notification_role(name)` reports only current admitted authority,
which may omit an optional reviewed feature. Review does not start code, enable
channels or save preferences; General still owns its native controls/persistence.

Static channel declarations populate `Backend::inventory()` without starting code.
Each `ChannelKey` contains authoritative `identity` (`github:<repository-id>/<id>`)
and channel ID; names are not keys. `set_policy(key, Policy)` changes effective
channel policy only, never plugin enablement. New channels are off with background
eligibility. Views distinguish saved policy, installation, desired plugin switch
and current admitted owner. Disabled/unavailable choices remain in memory.
Persistence belongs to the General/storage consumer, not this backend. Opening
the page or editing channel consent never starts a provider.

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

The [producer wire contract](https://github.com/Among-Terminal/among-plugin-template/blob/main/docs/api.md#producing-an-event)
defines message fields, text limits, source-generation requirements and the
per-producer cooldown.

Targetless application events never invent a pane/workspace. Always policy can
match, background requires no active application-owned window, and originating-
terminal-unfocused never matches. Existing core acknowledgement rules are unchanged.
The host resolves workspace membership and source ownership. Closed/reused
sources and stale plugin session/generation cannot redirect events. Producer
cooldown is independent of per-source core coalescing. Diagnostics contain fixed
reasons, not event text, configuration payloads or plugin free-form result text.

The bridge stamps an envelope with event/root causal ID, core session, authoritative
plugin identity/process generation, optional source-generation/workspace identity,
and kind `managed_notification`. The shared core itself retains
`Kind::ManagedNotification` for both pane and targetless ingress, not a terminal
bell/application substitute. Its opaque `Event::origin()` resolves through
`Host::origin(token)` to the host-registered `OriginIdentity { identity, generation }`.
Only trusted admission mints that token and its irrevocable lifetime lease;
neither kind nor origin is accepted from the plugin's event body.
Delivery-linked production (`causal_job`) is rejected, not recursively routed.
Production/delivery authority is separated at review/preflight/admission, not
silently retired after the first assigned or non-enqueued job. A delivery-only
process cannot produce even if it omits `causal_job`; omission is not proof of
independent intent. Results remain separate, self-delivery is omitted, and work is
rate/budget bounded. This is API-mediated feedback prevention: it covers
relationships that pass through this notification API. The
[confinement](plugins.md#confinement) denies the direct channels between two
plugins that it lists, but it is a separate mechanism and no guarantee that an
out-of-band exchange cannot happen: reduced protection that the user accepted
leaves more file access, and a kernel vulnerability defeats it. The host does not
detect a fresh-looking event that such an exchange caused.
Results never become notification events. No automatic retry or reconnect replay.

## Contribution and action state

The [contribution wire contract](https://github.com/Among-Terminal/among-plugin-template/blob/main/docs/api.md#visible-content-and-actions)
defines fields, scopes, styles, icon selectors, actions and withdrawal.
The renderer must preserve titles, Restore controls and workspace hues. It treats
text as plain data and never routes content to arbitrary host actions or markup.
Package artwork uses the statically inspected snapshot; malformed optional
artwork falls back to no icon. The bridge does not decode arbitrary images from
a wire message or open a wire-selected filesystem path or URL.

Snapshot `Contribution::actions` supplies opaque `ActionHandle`s. Explicit host
activation calls `Backend::activate(handle, now)`; the next plugin-host poll sends
`notification_action` with contribution ID/revision/action only to that current
owning process. No shell, terminal-input or host-command routing exists.
Replaced/withdrawn/expired/disabled/restarted actions are inert, including already
queued activations.

Current applied settings acknowledgements also invalidate old contribution/action
state without releasing old status/configuration/delivery reply credits.
`contributions(now)` is a renderer snapshot, not a widget renderer. The application's
renderer (`src/app/notification_ui.rs`) consumes it for the allowed areas, keeps the
existing title, Restore and stripes, and gives each action a native accessible name.

## Actual provider jobs and outcomes

The [delivery wire contract](https://github.com/Among-Terminal/among-plugin-template/blob/main/docs/api.md#delivery-providers)
defines jobs, results and cancellation. Admission must enforce declared,
explicitly enabled eligible channels and terminal-context consent. Source data
never contains PTY output, input, cwd, title, process command line, inherited environment or TTY
metadata. A permitted source contains pane, source generation and workspace string
references. Provider identity is determined by the real process receiving the job,
not by a reply claim.

Accepted is the provider's local claim, not proof of remote receipt or
exactly-once delivery. Wrong, duplicate, late
and revoked results cannot replace recorded outcomes. Policy/source/provider
revocation cancels active authority and sends bounded `cancel_delivery` messages
where the same process remains running; external side effects are not reversible.
Timeout/cancellation is recorded as uncertain, never retried. A provider that
retains an unanswered reply beyond five seconds is faulted/stopped, including a
cancelled job whose credit is still retained.

## Resource and lifetime limits

| Boundary | Implemented limit |
| --- | --- |
| Shared core delivery queue | 64 globally / 16 per channel; source keys for targeted work, distinct origin keys for targetless managed work |
| Active provider attempts | Two per bridge / one per channel, no second pending-job queue |
| Delivery request payload | 16 KiB including host envelope |
| Provider response deadline | Five seconds; existing off-loop two-second write deadline |
| Process wire/output | Existing 64-KiB frames, depth 32, 16 queued frames/1 MiB, rotating bounded drains |
| Replies | Status + configurations + deliveries share 15 retained credits; enumeration retains its reserved slot |
| Event metadata | 64 envelopes / ten seconds, coalesced by source/origin/ingress eligibility while retaining active IDs; evicted/unobserved jobs are dropped, never reattributed |
| Core producer registrations | 128 live origins; identity <=256 UTF-8 bytes, generation <=128; revoked metadata is reclaimed, not retained as history |
| Contributions | 128 globally / eight distinct IDs per process, including revision tombstones; ten-second lifetime |
| Actions | Four per contribution; 16 pending activations |
| Provider inventory | At most 128 installed identities / 4096 retained channel rows |
| Backend diagnostic records | 16 completions + 16 fixed rejections; existing core diagnostics remain separate |

At most two active event IDs are pinned inside the same 64-envelope budget.
Cap eviction selects only non-active records (or refuses new metadata if none is
available); fresh source traffic alone cannot revoke a valid active attempt.
Expiry, origin/source/channel revocation and deadlines still invalidate authority.

Provider disable removes its core channels/queued work immediately. Producer
revocation invalidates the same generic core-issued lease held by the backend,
process and active adapters. `Host::is_current(job)` and `is_event_current(event)`
reject that origin immediately, even before mutable cleanup; scheduling prunes its
queued jobs and bounded registry/cooldown entries. An old generation cannot become
a new origin, and a managed event cannot re-enter via `observe_host_event` as a host
bell after its envelope disappears. `envelope(event, now)` also checks exact core
kind/origin/source attribution and expiry before returning data.

All asynchronous adapters must consume
`Backend::authorize_delivery(job, now) -> Option<DeliveryAuthorization>` after
dequeue. This joined snapshot supplies `envelope() -> &Envelope`, `job() -> Job`,
`origin_lease() -> Option<OriginLease>` and
`is_current(&notifications::Host, now) -> bool`. Recheck it before and during
an attempt; it combines core source/channel/origin authority with the ten-second
metadata lifetime. A managed snapshot always retains its lease; a host snapshot
has none. Off-thread work must retain/check the cloneable thread-safe lease as
well as the adapter's existing source/channel cancellation, without carrying GTK
or RefCell borrows into the worker. Revocation cannot reverse external effects.
Provider attempts consume this same accessor and an owned exact-job effect
capability. For off-thread/exec consumers, use the
[owned effect authority](notification-effect-authority.md), including its
post-operation-claim validation ordering and finite read-only descriptor contract.
Missing metadata is checked at original private snapshot issuance: bounded cache
eviction does not revoke an already issued owned snapshot. The compatible
`plugin_notifications::OriginLease` path re-exports the core's single lease type;
no duplicate lifetime authority or pending queue is introduced.
Timers, envelopes/jobs and action handles are session-only, never saved sessions.

The core's channel-full policy drops an incoming distinct scheduling key once that
channel already has sixteen pending keys. Single-source/origin floods coalesce and a
quiet sibling progresses; this is not a guarantee that every new distinct
key survives an already saturated channel. Core suppression diagnostics expose
that overload. Changing the core's admission fairness is a separate review point.
Targetless managed events coalesce and cool down by their exact registered origin
(including generation), separately from targetless host application work. A quiet
origin entering after noisy production has begun can bypass that origin's cooldown.
Global fanout balancing uses those same keys within the existing 64-job queue.
Target-bearing delivery cooldown/unread/focus semantics remain source-based.

## Sequential integration requirements and local checks

1. Supply real focus/source lifetimes and the same core, then construct the host
   with the explicit bridge. Update the ordinary-app feature set only then.
2. On actual core bell/application ingress call `observe_host_event(event,
   current_workspace, ingress_focus)` immediately, using the same focus snapshot
   passed to the core. Never recover missing envelopes or eligibility by guessing.
3. General must consume typed inventory and call `set_policy` after accepted edits,
   with durable identity/channel-scoped storage and unknown-data preservation.
4. Render contribution snapshots only in the existing plugin areas and activate
   opaque handles. The current app renderer adds plain text, inspected icons,
   named styles and native action buttons inside those areas; it never replaces
   the title, Restore control or workspace identity. Focused widgets are retained
   across unchanged snapshots/legacy slot refresh; bounded scrolling contains wide
   content. Native contribution/action acceptance remains evidence-dependent.
5. Command/audio/app adapters must explicitly map `Kind::ManagedNotification` and
   consume `authorize_delivery(job, now)`, retaining/rechecking its origin authority
   alongside their source/channel cancellation. The ordinary command consumer now
   retains this snapshot through the I/O service and maps the managed kind/origin
   explicitly. The audio owner retains/rechecks it before submission and on the
   shared tick; helper-side off-thread revocation still needs the audio author's
   compatible admission seam. Joining these consumers does not accept the combined
   feature, helper cancellation or platform behavior.
6. Join installer registration removal/replacement with host stop/reap and bridge
   uninstall/re-registration. Do not resurrect consent from retained settings.

Run real process/model tests after building the fixture:

```sh
CARGO_BUILD_JOBS=1 cargo build --locked --example notification-plugin
CARGO_BUILD_JOBS=1 cargo test --locked plugin::host::notification_backend -- --nocapture
CARGO_BUILD_JOBS=1 cargo test --locked --test plugin_notification_model
```

The ignored `native_pty_bell_real_provider_and_keyboard_remain_responsive_during_flood`
test requires `among-tools ci-x11` with its owned display/private HOME/XDG profile.
It tests an explicit backend plus real VTE PTYs/provider/keyboard; the ordinary
application, General and rendering are covered by the notification UI tests named
above.
