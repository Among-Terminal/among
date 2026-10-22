//! explicit managed-plugin backend; ordinary application startup does not attach
//! this bridge. the caller supplies the same core used by semantic bell ingress.

use crate::notifications::{self as core, Channel, Event, Focus, Host, Job, Policy, Source};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const FEATURES: &[&str] = &[
    "notifications.produce",
    "notifications.ui",
    "notifications.deliver",
];
pub const DEADLINE: Duration = Duration::from_secs(5);
pub const MAX_ACTIVE: usize = 2;
pub const MAX_PAYLOAD_BYTES: usize = 16 * 1024;
pub const MAX_CONTRIBUTIONS: usize = 128;
pub const MAX_CONTRIBUTIONS_PER_PLUGIN: usize = 8;
const MAX_CHANNELS: usize = 128 * 32;
const MAX_ACTIONS: usize = 16;
const MAX_EVENTS: usize = core::MAX_PENDING;
const EVENT_LIFETIME: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Target {
    Application,
    Workspace {
        #[serde(
            serialize_with = "serialize_reference",
            deserialize_with = "deserialize_reference"
        )]
        reference: u64,
    },
    Pane {
        #[serde(
            serialize_with = "serialize_reference",
            deserialize_with = "deserialize_reference"
        )]
        reference: u64,
    },
}

fn serialize_reference<S: serde::Serializer>(value: &u64, output: S) -> Result<S::Ok, S::Error> {
    output.serialize_str(&value.to_string())
}

fn deserialize_reference<'de, D: serde::Deserializer<'de>>(input: D) -> Result<u64, D::Error> {
    let text = String::deserialize(input)?;
    let value = text.parse::<u64>().map_err(serde::de::Error::custom)?;
    if text.len() > 20 || value.to_string() != text {
        return Err(serde::de::Error::custom("noncanonical target reference"));
    }
    Ok(value)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub slot: String,
    pub target: Target,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Owner {
    pub identity: String,
    pub generation: String,
}

/// a delivery worker can observe origin revocation without borrowing GTK or the
/// bridge. only revocation is public; an old process cannot re-enable this lease.
#[derive(Clone, Debug)]
pub struct OriginLease(Arc<AtomicBool>);

impl OriginLease {
    pub fn revoke(&self) {
        self.0.store(false, Ordering::Release);
    }
    pub fn is_current(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ChannelKey {
    pub identity: String,
    pub channel: String,
}

#[derive(Clone, Debug)]
pub struct Declaration {
    pub key: ChannelKey,
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug)]
pub struct ChannelView {
    pub declaration: Declaration,
    pub policy: Policy,
    pub installed: bool,
    pub plugin_enabled: bool,
    pub owner: Option<Owner>,
}

struct RegisteredChannel {
    view: ChannelView,
    token: Option<Channel>,
}

/// package artwork is already bounded/decoded by static inspection. wire data
/// can select this snapshot, but cannot cause an arbitrary filesystem read.
#[derive(Clone, Debug)]
pub enum Icon {
    Theme(String),
    Raster(Arc<[u8]>),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Style {
    #[default]
    Default,
    Success,
    Warning,
    Error,
    Accent,
    Dim,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionInput {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContributionInput {
    pub id: String,
    pub revision: u64,
    pub scope: Scope,
    pub source_generation: Option<String>,
    pub text: String,
    pub tooltip: Option<String>,
    pub icon: Option<String>,
    #[serde(default)]
    pub style: Style,
    #[serde(default)]
    pub actions: Vec<ActionInput>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionHandle {
    owner: Owner,
    contribution: String,
    revision: u64,
    action: String,
}

#[derive(Clone, Debug)]
pub struct ActionView {
    pub label: String,
    pub handle: ActionHandle,
}

#[derive(Clone, Debug)]
pub struct Contribution {
    pub owner: Owner,
    pub id: String,
    pub revision: u64,
    pub scope: Scope,
    pub text: String,
    pub tooltip: Option<String>,
    pub icon: Option<Icon>,
    pub style: Style,
    pub actions: Vec<ActionView>,
    source: Option<Source>,
    expires: Instant,
    last_action: Option<Instant>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventInput {
    pub sequence: u64,
    pub target: Target,
    pub source_generation: Option<String>,
    pub message: String,
    pub causal_job: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SourceIdentity {
    pub pane: String,
    pub generation: String,
    pub workspace: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Envelope {
    pub event_id: String,
    pub causal_id: String,
    pub core_session: String,
    pub kind: &'static str,
    pub origin: Option<Owner>,
    pub source: Option<SourceIdentity>,
    pub message: Option<String>,
}

struct RememberedEvent {
    event: Event,
    envelope: Envelope,
    eligibility: [bool; 3],
}

#[derive(Clone, Debug, Serialize)]
pub struct Delivery {
    pub job_id: u64,
    pub channel: String,
    pub event: Envelope,
}

struct Active {
    owner: Owner,
    key: ChannelKey,
    job: Job,
    deadline: Instant,
    origin: Option<OriginLease>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryOutcome {
    Accepted,
    Refused,
    Failed,
    Uncertain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Result(DeliveryOutcome),
    TimedOutUncertain,
    CancelledUncertain,
    NotEnqueued,
}

#[derive(Clone, Debug)]
pub struct Completion {
    pub owner: Owner,
    pub job_id: u64,
    pub event_id: u64,
    pub status: Status,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    Unavailable,
    Unauthorized,
    InvalidData,
    Stale,
    RateLimited,
    CausalFeedback,
    Full,
}

struct Producer {
    owner: Owner,
    sequence: u64,
    next_event: Option<Instant>,
    revisions: BTreeMap<String, u64>,
    icon: Option<Icon>,
    delivery_seen: bool,
    live: OriginLease,
}

pub struct Backend {
    core: Rc<RefCell<Host>>,
    session: Option<String>,
    usable: bool,
    focus: Focus,
    producers: BTreeMap<String, Producer>,
    registrations: BTreeSet<String>,
    channels: BTreeMap<ChannelKey, RegisteredChannel>,
    events: VecDeque<RememberedEvent>,
    contributions: Vec<Contribution>,
    actions: VecDeque<ActionHandle>,
    active: BTreeMap<u64, Active>,
    cancellations: VecDeque<(Owner, u64)>,
    completions: VecDeque<Completion>,
    rejections: VecDeque<Rejection>,
    next_job: u64,
    next_channel: usize,
}

impl Backend {
    pub fn new(core: Rc<RefCell<Host>>, focus: Focus) -> Self {
        Self {
            core,
            session: None,
            usable: true,
            focus,
            producers: BTreeMap::new(),
            registrations: BTreeSet::new(),
            channels: BTreeMap::new(),
            events: VecDeque::new(),
            contributions: Vec::new(),
            actions: VecDeque::new(),
            active: BTreeMap::new(),
            cancellations: VecDeque::new(),
            completions: VecDeque::new(),
            rejections: VecDeque::new(),
            next_job: 0,
            next_channel: 0,
        }
    }

    pub fn bind(&mut self, session: &str) -> bool {
        if self.session.as_deref().is_some_and(|old| old != session) || !self.usable {
            return false;
        }
        self.session = Some(session.into());
        true
    }

    pub fn usable(&self, session: &str) -> bool {
        self.usable && self.session.as_deref() == Some(session) && self.core.try_borrow().is_ok()
    }

    pub fn set_focus(&mut self, focus: Focus) {
        self.focus = focus;
    }

    pub fn source(&self, pane: u64) -> Option<Source> {
        self.core.try_borrow().ok()?.source(pane)
    }

    pub fn inventory(&self) -> Vec<ChannelView> {
        self.channels
            .values()
            .map(|channel| {
                let mut view = channel.view.clone();
                if view
                    .owner
                    .as_ref()
                    .is_some_and(|owner| !self.current(owner))
                {
                    view.owner = None;
                }
                view
            })
            .collect()
    }

    pub fn register_identity(&mut self, key: &str) -> Result<(), Rejection> {
        if !identity(key) {
            return Err(Rejection::InvalidData);
        }
        if !self.registrations.contains(key) && self.registrations.len() >= 128 {
            return Err(Rejection::Full);
        }
        self.registrations.insert(key.into());
        Ok(())
    }

    pub fn registration_current(&self, key: &str) -> bool {
        self.registrations.contains(key)
    }

    pub fn declare(&mut self, declaration: Declaration) -> Result<(), Rejection> {
        if !identity(&declaration.key.identity)
            || !local_id(&declaration.key.channel)
            || !text(&declaration.name, 128, false)
            || !text(&declaration.description, 1024, true)
        {
            return Err(Rejection::InvalidData);
        }
        self.register_identity(&declaration.key.identity)?;
        if let Some(channel) = self.channels.get_mut(&declaration.key) {
            channel.view.declaration = declaration;
            channel.view.installed = true;
            return Ok(());
        }
        if self.channels.len() >= MAX_CHANNELS {
            return Err(Rejection::Full);
        }
        self.channels.insert(
            declaration.key.clone(),
            RegisteredChannel {
                view: ChannelView {
                    declaration,
                    policy: Policy::DELIVERY_DEFAULT,
                    installed: true,
                    plugin_enabled: false,
                    owner: None,
                },
                token: None,
            },
        );
        Ok(())
    }

    /// this edits channel consent only; no process-start callback exists here.
    pub fn set_policy(&mut self, key: &ChannelKey, policy: Policy) -> Result<(), Rejection> {
        let channel = self.channels.get_mut(key).ok_or(Rejection::Unavailable)?;
        if let Some(token) = channel.token {
            self.core
                .try_borrow_mut()
                .map_err(|_| Rejection::Unavailable)?
                .set_channel_policy(token, policy)
                .map_err(|_| Rejection::Unavailable)?;
        }
        channel.view.policy = policy;
        self.housekeep(Instant::now());
        Ok(())
    }

    pub fn plugin_enabled(&mut self, identity: &str, enabled: bool) {
        if !enabled {
            self.revoke(identity);
        }
        for channel in self
            .channels
            .values_mut()
            .filter(|channel| channel.view.declaration.key.identity == identity)
        {
            channel.view.plugin_enabled = enabled;
        }
    }

    pub fn admit(
        &mut self,
        owner: Owner,
        delivery: bool,
        icon: Option<Icon>,
    ) -> Result<OriginLease, Rejection> {
        if !identity(&owner.identity)
            || owner.generation.is_empty()
            || owner.generation.len() > 128
            || !self.usable
            || !self.registrations.contains(&owner.identity)
            || (!self.producers.contains_key(&owner.identity) && self.producers.len() >= 32)
        {
            return Err(Rejection::Unavailable);
        }
        self.revoke(&owner.identity);
        if delivery {
            let mut core = self
                .core
                .try_borrow_mut()
                .map_err(|_| Rejection::Unavailable)?;
            for channel in self.channels.values_mut().filter(|channel| {
                channel.view.declaration.key.identity == owner.identity
                    && channel.view.installed
                    && channel.view.plugin_enabled
            }) {
                channel.token = Some(
                    core.register_channel(channel.view.policy)
                        .map_err(|_| Rejection::Unavailable)?,
                );
                channel.view.owner = Some(owner.clone());
            }
        }
        let live = OriginLease(Arc::new(AtomicBool::new(true)));
        self.producers.insert(
            owner.identity.clone(),
            Producer {
                owner,
                sequence: 0,
                next_event: None,
                revisions: BTreeMap::new(),
                icon,
                delivery_seen: false,
                live: live.clone(),
            },
        );
        Ok(live)
    }

    pub fn revoke(&mut self, identity: &str) {
        if let Some(producer) = self.producers.remove(identity) {
            producer.live.revoke();
        }
        self.contributions
            .retain(|item| item.owner.identity != identity);
        self.actions
            .retain(|action| action.owner.identity != identity);
        self.events.retain(|event| {
            event
                .envelope
                .origin
                .as_ref()
                .is_none_or(|owner| owner.identity != identity)
        });
        if let Ok(mut core) = self.core.try_borrow_mut() {
            for channel in self
                .channels
                .values_mut()
                .filter(|channel| channel.view.declaration.key.identity == identity)
            {
                if let Some(token) = channel.token.take() {
                    core.remove_channel(token);
                }
                channel.view.owner = None;
            }
        }
        self.housekeep(Instant::now());
    }

    pub fn uninstall(&mut self, identity: &str) {
        self.revoke(identity);
        self.registrations.remove(identity);
        for channel in self
            .channels
            .values_mut()
            .filter(|channel| channel.view.declaration.key.identity == identity)
        {
            channel.view.installed = false;
            channel.view.plugin_enabled = false;
        }
    }

    pub fn shutdown(&mut self) {
        self.usable = false;
        let identities: Vec<_> = self.producers.keys().cloned().collect();
        for identity in identities {
            self.revoke(&identity);
        }
    }

    fn current(&self, owner: &Owner) -> bool {
        self.usable
            && self.registrations.contains(&owner.identity)
            && self
                .producers
                .get(&owner.identity)
                .is_some_and(|producer| &producer.owner == owner && producer.live.is_current())
    }

    /// workspace membership is host-resolved; plugin text cannot supply origin,
    /// event/session IDs, target generation, cwd, process state or acknowledgement.
    pub fn produce(
        &mut self,
        owner: &Owner,
        input: EventInput,
        workspace: Option<u64>,
        now: Instant,
    ) -> Result<Event, Rejection> {
        if !self.current(owner) {
            return Err(Rejection::Unauthorized);
        }
        if input.causal_job.is_some() {
            return Err(Rejection::CausalFeedback);
        }
        if !text(&input.message, 240, false) || input.message.len() > 1024 || input.sequence == 0 {
            return Err(Rejection::InvalidData);
        }
        let producer = self
            .producers
            .get_mut(&owner.identity)
            .ok_or(Rejection::Unauthorized)?;
        if producer.delivery_seen {
            return Err(Rejection::CausalFeedback);
        }
        if input.sequence <= producer.sequence {
            return Err(Rejection::Stale);
        }
        producer.sequence = input.sequence;
        if producer.next_event.is_some_and(|next| now < next) {
            return Err(Rejection::RateLimited);
        }
        producer.next_event = now.checked_add(core::COOLDOWN);
        let (event, source) = {
            let mut core = self
                .core
                .try_borrow_mut()
                .map_err(|_| Rejection::Unavailable)?;
            match input.target {
                Target::Application => (
                    core.application_event(self.focus, now)
                        .map_err(|_| Rejection::Stale)?,
                    None,
                ),
                Target::Pane { reference } => {
                    let source = core.source(reference).ok_or(Rejection::Stale)?;
                    if input.source_generation.as_deref()
                        != Some(source.generation().to_string().as_str())
                    {
                        return Err(Rejection::Stale);
                    }
                    let workspace = workspace.ok_or(Rejection::Unauthorized)?;
                    (
                        core.bell(source, self.focus, now)
                            .map_err(|_| Rejection::Stale)?,
                        Some(SourceIdentity {
                            pane: reference.to_string(),
                            generation: source.generation().to_string(),
                            workspace: workspace.to_string(),
                        }),
                    )
                }
                Target::Workspace { .. } => return Err(Rejection::Unauthorized),
            }
        };
        self.remember(
            event,
            Envelope {
                event_id: event.id().to_string(),
                causal_id: event.id().to_string(),
                core_session: event.session().to_string(),
                kind: "managed_notification",
                origin: Some(owner.clone()),
                source,
                message: Some(input.message),
            },
            self.focus,
        );
        Ok(event)
    }

    /// call immediately after real host ingress, on this same core. unobserved
    /// queue entries are dropped, never mislabelled as terminal bells after eviction.
    pub fn observe_host_event(
        &mut self,
        event: Event,
        workspace: Option<u64>,
        focus: Focus,
    ) -> Result<(), Rejection> {
        let core = self.core.try_borrow().map_err(|_| Rejection::Unavailable)?;
        let source = match event.source() {
            Some(source) if core.is_live(source) => Some(SourceIdentity {
                pane: source.pane().to_string(),
                generation: source.generation().to_string(),
                workspace: workspace.ok_or(Rejection::InvalidData)?.to_string(),
            }),
            Some(_) => return Err(Rejection::Stale),
            None => None,
        };
        drop(core);
        if self.events.iter().any(|old| old.event == event) {
            return Err(Rejection::Stale);
        }
        self.remember(
            event,
            Envelope {
                event_id: event.id().to_string(),
                causal_id: event.id().to_string(),
                core_session: event.session().to_string(),
                kind: if event.source().is_some() {
                    "terminal_bell"
                } else {
                    "application"
                },
                origin: None,
                source,
                message: None,
            },
            focus,
        );
        Ok(())
    }

    fn remember(&mut self, event: Event, envelope: Envelope, focus: Focus) {
        let eligibility = [
            core::Eligibility::Always,
            core::Eligibility::OriginatingTerminalUnfocused,
            core::Eligibility::ApplicationBackground,
        ]
        .map(|eligibility| {
            Policy {
                enabled: true,
                eligibility,
            }
            .permits(event.source(), focus)
        });
        // mirror source coalescing without evicting a quiet source's envelope
        // on every noisy bell. different ingress eligibility must survive: an
        // ineligible later bell cannot erase another channel's earlier job.
        self.events.retain(|old| {
            old.event.source() != event.source()
                || old.envelope.origin != envelope.origin
                || old.eligibility != eligibility
                || self
                    .active
                    .values()
                    .any(|active| active.job.event() == old.event)
        });
        if self.events.len() == MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(RememberedEvent {
            event,
            envelope,
            eligibility,
        });
    }

    pub fn envelope(&self, event: Event, now: Instant) -> Option<&Envelope> {
        self.events
            .iter()
            .find(|record| {
                record.event == event
                    && now.saturating_duration_since(event.received_at()) < EVENT_LIFETIME
                    && record
                        .envelope
                        .origin
                        .as_ref()
                        .is_none_or(|owner| self.current(owner))
                    && event.source().is_none_or(|source| {
                        self.core
                            .try_borrow()
                            .is_ok_and(|core| core.is_live(source))
                    })
            })
            .map(|record| &record.envelope)
    }

    pub fn origin_lease(&self, event: Event, now: Instant) -> Option<OriginLease> {
        let origin = self.envelope(event, now)?.origin.as_ref()?;
        Some(self.producers.get(&origin.identity)?.live.clone())
    }

    pub fn contribute(
        &mut self,
        owner: &Owner,
        input: ContributionInput,
        source: Option<Source>,
        now: Instant,
    ) -> Result<(), Rejection> {
        if !self.current(owner) {
            return Err(Rejection::Unauthorized);
        }
        if let Some(source) = source {
            if input.scope.target
                != (Target::Pane {
                    reference: source.pane(),
                })
                || input.source_generation.as_deref()
                    != Some(source.generation().to_string().as_str())
                || !self
                    .core
                    .try_borrow()
                    .is_ok_and(|core| core.is_live(source))
            {
                return Err(Rejection::Stale);
            }
        } else if matches!(input.scope.target, Target::Pane { .. }) {
            return Err(Rejection::Stale);
        }
        if !local_id(&input.id)
            || input.revision == 0
            || !valid_scope(&input.scope)
            || !text(&input.text, 512, false)
            || input.text.len() > 2048
            || input
                .tooltip
                .as_ref()
                .is_some_and(|tooltip| !text(tooltip, 512, true) || tooltip.len() > 2048)
            || input.actions.len() > 4
        {
            return Err(Rejection::InvalidData);
        }
        let mut actions = Vec::new();
        for action in input.actions {
            if !local_id(&action.id)
                || !text(&action.label, 64, false)
                || actions
                    .iter()
                    .any(|old: &ActionView| old.handle.action == action.id)
            {
                return Err(Rejection::InvalidData);
            }
            actions.push(ActionView {
                label: action.label,
                handle: ActionHandle {
                    owner: owner.clone(),
                    contribution: input.id.clone(),
                    revision: input.revision,
                    action: action.id,
                },
            });
        }
        let producer = self
            .producers
            .get_mut(&owner.identity)
            .ok_or(Rejection::Unauthorized)?;
        if producer
            .revisions
            .get(&input.id)
            .is_some_and(|revision| input.revision <= *revision)
        {
            return Err(Rejection::Stale);
        }
        // tombstones share the same eight-ID budget; withdrawal cannot open an
        // unbounded revision/dedup cache or revive an old callback.
        if !producer.revisions.contains_key(&input.id)
            && producer.revisions.len() >= MAX_CONTRIBUTIONS_PER_PLUGIN
        {
            return Err(Rejection::Full);
        }
        let icon = match input.icon.as_deref() {
            Some("package") => producer.icon.clone(),
            Some(name) => Some(Icon::Theme(
                theme(name).ok_or(Rejection::InvalidData)?.into(),
            )),
            None => None,
        };
        let replacing = self
            .contributions
            .iter()
            .any(|old| old.owner == *owner && old.id == input.id);
        if !replacing && self.contributions.len() >= MAX_CONTRIBUTIONS {
            return Err(Rejection::Full);
        }
        producer.revisions.insert(input.id.clone(), input.revision);
        self.contributions
            .retain(|old| old.owner != *owner || old.id != input.id);
        self.contributions.push(Contribution {
            owner: owner.clone(),
            id: input.id,
            revision: input.revision,
            scope: input.scope,
            text: input.text,
            tooltip: input.tooltip,
            icon,
            style: input.style,
            actions,
            source,
            expires: now
                .checked_add(EVENT_LIFETIME)
                .ok_or(Rejection::Unavailable)?,
            last_action: None,
        });
        Ok(())
    }

    pub fn withdraw(&mut self, owner: &Owner, id: &str) {
        self.contributions
            .retain(|old| old.owner != *owner || old.id != id);
    }

    pub fn invalidate_content(&mut self, owner: &Owner) {
        self.contributions.retain(|old| old.owner != *owner);
        self.actions.retain(|action| action.owner != *owner);
    }

    pub fn retain_scopes(&mut self, live: impl Fn(&Scope) -> bool) {
        self.contributions.retain(|item| live(&item.scope));
    }

    pub fn contributions(&self, now: Instant) -> Vec<Contribution> {
        self.contributions
            .iter()
            .filter(|item| self.item_current(item, now))
            .cloned()
            .collect()
    }

    fn item_current(&self, item: &Contribution, now: Instant) -> bool {
        now < item.expires
            && self.current(&item.owner)
            && item.source.is_none_or(|source| {
                self.core
                    .try_borrow()
                    .is_ok_and(|core| core.is_live(source))
            })
    }

    pub fn activate(&mut self, handle: &ActionHandle, now: Instant) -> Result<(), Rejection> {
        let index = self
            .contributions
            .iter()
            .position(|item| {
                item.owner == handle.owner
                    && item.id == handle.contribution
                    && item.revision == handle.revision
                    && item.actions.iter().any(|action| &action.handle == handle)
                    && self.item_current(item, now)
            })
            .ok_or(Rejection::Stale)?;
        let item = &mut self.contributions[index];
        if item
            .last_action
            .is_some_and(|last| now.saturating_duration_since(last) < Duration::from_millis(250))
        {
            return Err(Rejection::RateLimited);
        }
        if self.actions.len() >= MAX_ACTIONS {
            return Err(Rejection::Full);
        }
        item.last_action = Some(now);
        self.actions.push_back(handle.clone());
        Ok(())
    }

    pub fn take_action(&mut self, now: Instant) -> Option<(Owner, String, u64, String)> {
        while let Some(handle) = self.actions.pop_front() {
            if self.contributions.iter().any(|item| {
                item.owner == handle.owner
                    && item.id == handle.contribution
                    && item.revision == handle.revision
                    && self.item_current(item, now)
            }) {
                return Some((
                    handle.owner,
                    handle.contribution,
                    handle.revision,
                    handle.action,
                ));
            }
        }
        None
    }

    /// no private pending-delivery queue: take only from the shared core, and
    /// only when the real process has transport capacity and shared reply credit.
    pub fn take_delivery(&mut self, owner: &Owner, now: Instant) -> Option<Delivery> {
        self.housekeep(now);
        if !self.current(owner) || self.active.len() >= MAX_ACTIVE {
            return None;
        }
        let keys: Vec<_> = self
            .channels
            .iter()
            .filter(|(_, channel)| {
                channel.view.owner.as_ref() == Some(owner)
                    && channel.view.installed
                    && channel.view.plugin_enabled
                    && channel.view.policy.enabled
            })
            .map(|(key, _)| key.clone())
            .collect();
        if keys.is_empty() {
            return None;
        }
        let first = self.next_channel % keys.len();
        self.next_channel = first + 1;
        for offset in 0..keys.len() {
            let key = &keys[(first + offset) % keys.len()];
            if self.active.values().any(|active| &active.key == key) {
                continue;
            }
            let token = self.channels.get(key)?.token?;
            // each pass consumes at most the core's finite per-channel budget.
            for _ in 0..core::MAX_PENDING_PER_CHANNEL {
                let job = self.core.try_borrow_mut().ok()?.take_ready(token, now);
                let Some(job) = job else {
                    break;
                };
                if !self.core.try_borrow().ok()?.is_current(job) {
                    continue;
                }
                let Some(envelope) = self.envelope(job.event(), now) else {
                    self.reject(Rejection::Stale);
                    continue;
                };
                if envelope
                    .origin
                    .as_ref()
                    .is_some_and(|origin| origin.identity == owner.identity)
                {
                    self.reject(Rejection::CausalFeedback);
                    continue;
                }
                let mut envelope = envelope.clone();
                // notification delivery is not an alternate terminal enumeration
                // grant. the runtime further strips source metadata without consent.
                if job.event().source().is_none() {
                    envelope.source = None;
                }
                let origin = if envelope.origin.is_some() {
                    Some(self.origin_lease(job.event(), now)?)
                } else {
                    None
                };
                self.next_job = self.next_job.checked_add(1)?;
                let job_id = self.next_job;
                self.active.insert(
                    job_id,
                    Active {
                        owner: owner.clone(),
                        key: key.clone(),
                        job,
                        deadline: now.checked_add(DEADLINE)?,
                        origin,
                    },
                );
                if let Some(producer) = self.producers.get_mut(&owner.identity) {
                    producer.delivery_seen = true;
                }
                return Some(Delivery {
                    job_id,
                    channel: key.channel.clone(),
                    event: envelope,
                });
            }
        }
        None
    }

    pub fn not_enqueued(&mut self, owner: &Owner, job_id: u64) {
        if self
            .active
            .get(&job_id)
            .is_some_and(|active| &active.owner == owner)
        {
            self.finish(job_id, Status::NotEnqueued);
        }
    }

    pub fn result(
        &mut self,
        owner: &Owner,
        job_id: u64,
        outcome: DeliveryOutcome,
        now: Instant,
    ) -> bool {
        self.housekeep(now);
        if !self.current(owner)
            || self
                .active
                .get(&job_id)
                .is_none_or(|active| &active.owner != owner)
        {
            return false;
        }
        self.finish(job_id, Status::Result(outcome));
        true
    }

    pub fn housekeep(&mut self, now: Instant) {
        self.events.retain(|record| {
            now.saturating_duration_since(record.event.received_at()) < EVENT_LIFETIME
        });
        let stale: Vec<_> = self
            .active
            .iter()
            .filter_map(|(id, active)| {
                let current = self.current(&active.owner)
                    && active.origin.as_ref().is_none_or(OriginLease::is_current)
                    && self
                        .core
                        .try_borrow()
                        .is_ok_and(|core| core.is_current(active.job))
                    && self.envelope(active.job.event(), now).is_some();
                if !current {
                    Some((*id, Status::CancelledUncertain))
                } else if now >= active.deadline {
                    Some((*id, Status::TimedOutUncertain))
                } else {
                    None
                }
            })
            .collect();
        for (id, status) in stale {
            if let Some(active) = self.active.get(&id) {
                if self.cancellations.len() < MAX_ACTIONS {
                    self.cancellations.push_back((active.owner.clone(), id));
                }
            }
            self.finish(id, status);
        }
        let core = &self.core;
        self.contributions.retain(|item| {
            now < item.expires
                && item
                    .source
                    .is_none_or(|source| core.try_borrow().is_ok_and(|core| core.is_live(source)))
        });
    }

    fn finish(&mut self, job_id: u64, status: Status) {
        if let Some(active) = self.active.remove(&job_id) {
            if self.completions.len() == core::MAX_DIAGNOSTICS / 2 {
                self.completions.pop_front();
            }
            self.completions.push_back(Completion {
                owner: active.owner,
                job_id,
                event_id: active.job.event().id(),
                status,
            });
        }
    }

    pub fn take_cancellation(&mut self) -> Option<(Owner, u64)> {
        self.cancellations.pop_front()
    }
    pub fn completions(&self) -> &VecDeque<Completion> {
        &self.completions
    }
    pub fn active_count(&self) -> usize {
        self.active.len()
    }
    pub fn event_count(&self) -> usize {
        self.events.len()
    }
    pub fn reject(&mut self, reason: Rejection) {
        if self.rejections.len() == core::MAX_DIAGNOSTICS / 2 {
            self.rejections.pop_front();
        }
        self.rejections.push_back(reason);
    }
    pub fn rejections(&self) -> &VecDeque<Rejection> {
        &self.rejections
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn identity(value: &str) -> bool {
    value
        .strip_prefix("github:")
        .and_then(|value| value.split_once('/'))
        .is_some_and(|(repository, plugin)| {
            repository
                .parse::<u64>()
                .is_ok_and(|id| id != 0 && id.to_string() == repository)
                && local_id(plugin)
        })
}

fn local_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn text(value: &str, limit: usize, multiline: bool) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= limit
        && value.chars().all(|ch| {
            (!ch.is_control() || (multiline && matches!(ch, '\n' | '\t')))
                && !matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
}

fn theme(value: &str) -> Option<&str> {
    value.strip_prefix("theme:").filter(|name| {
        !name.is_empty()
            && name.len() <= 128
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

fn valid_scope(scope: &Scope) -> bool {
    match scope.target {
        Target::Application => matches!(scope.slot.as_str(), "app_header" | "app_footer"),
        Target::Workspace { .. } => matches!(
            scope.slot.as_str(),
            "workspace_header" | "workspace_summary"
        ),
        Target::Pane { .. } => matches!(scope.slot.as_str(), "pane_header" | "pane_footer"),
    }
}
