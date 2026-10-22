# Managed notification backend — explicit integration under review

This is an implemented backend boundary plus an ordinary-app integration on the
current test branch, **not accepted notification enablement**. Plain
`PluginHost::new` / `with_start_gate` construction still offers no notification
roles; the application explicitly uses `with_notification_backend` with its VTE
core. General reads declared full-identity/channel inventory and persists choices
independently of plugin enablement, retaining unavailable choices and unknown data.
Compilation or serialization does not establish native notification acceptance.

Independent review of `ca73e6e` found three open P2 blockers: active provider
envelopes can be evicted at the 64-record bound, targetless producers share one
coalescing/cooldown key, and the permanent producer retirement after any delivery
assignment (including not-enqueued) lacks an approved dual-role contract. The API
author owns the first two/core-kind repairs and the owner-chosen strict role split.
The common UI does not waive or duplicate those repairs.

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

### Explicit production/delivery role choice

A process may hold **either** `notifications.produce` **or**
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
rate/budget bounded. This is API-mediated feedback prevention, **not** a sandbox or
a guarantee against out-of-band communication between unsandboxed processes.
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
| Shared core delivery queue | 64 globally / 16 per channel; source keys for targeted work, distinct origin keys for targetless managed work |
| Active provider attempts | Two per bridge / one per channel, no second pending-job queue |
| Delivery request payload | 16 KiB including host envelope |
| Provider response deadline | Five seconds; existing off-loop two-second write deadline |
| Process wire/output | Existing 64-KiB frames, depth 32, 16 queued frames/1 MiB, rotating bounded drains |
| Replies | Status + configurations + deliveries share **15 retained credits**; enumeration retains its reserved slot |
| Event metadata | 64 envelopes / ten seconds, coalesced by source/origin/ingress eligibility while retaining active IDs; evicted/unobserved jobs are dropped, never reattributed |
| Core producer registrations | 128 live origins; identity <=256 UTF-8 bytes, generation <=128; revoked metadata is reclaimed, not retained as history |
| Contributions | 128 globally / eight distinct IDs per process, including revision tombstones; ten-second lifetime |
| Actions | Four per contribution; 16 pending activations |
| Provider inventory | At most 128 installed identities / 4096 retained channel rows |
| Backend diagnostic records | 16 completions + 16 fixed rejections; existing core diagnostics remain separate |

At most two active event IDs are pinned **inside** the same 64-envelope budget.
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

**All asynchronous adapters** must consume
`Backend::authorize_delivery(job, now) -> Option<DeliveryAuthorization>` after
dequeue. This joined snapshot supplies `envelope() -> &Envelope`, `job() -> Job`,
`origin_lease() -> Option<OriginLease>` and
`is_current(&notifications::Host, now) -> bool`. Recheck it before **and during**
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
quiet sibling progresses; this is **not** a guarantee that every new distinct
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
It tests an explicit backend plus real VTE PTYs/provider/keyboard, **not** ordinary
app/General/rendering integration. Full three-package gates, notices, strict specs,
independent review and latest-head GitHub CI are separate acceptance requirements.
