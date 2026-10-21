//! Session-local attention and bounded channel scheduling.
//!
//! callers admit producers and establish actual keyboard/window focus before
//! ingress. this layer neither parses terminal output nor executes deliveries.
//! a dequeued job is permission to attempt delivery, never a delivery receipt.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub const PULSE: Duration = Duration::from_secs(1);
pub const COOLDOWN: Duration = Duration::from_secs(2);
pub const MAX_PENDING: usize = 64;
pub const MAX_PENDING_PER_CHANNEL: usize = 16;
pub const MAX_DIAGNOSTICS: usize = 32;

static SESSION: AtomicU64 = AtomicU64::new(0);

/// tokens are minted by the host, not reconstructed from a pane number by a
/// producer. reusing a pane number cannot revive an old callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Source {
    session: u64,
    generation: u64,
    pane: u64,
}

impl Source {
    pub fn pane(self) -> u64 {
        self.pane
    }

    pub fn session(self) -> u64 {
        self.session
    }

    pub fn generation(self) -> u64 {
        self.generation
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Focus {
    /// includes application-owned transient windows, not just the main window.
    pub application_active: bool,
    /// only actual keyboard focus in the visible, active main window qualifies.
    pub viewed: Option<Source>,
}

impl Focus {
    fn views(self, source: Source) -> bool {
        self.application_active && self.viewed == Some(source)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eligibility {
    Always,
    OriginatingTerminalUnfocused,
    ApplicationBackground,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub enabled: bool,
    pub eligibility: Eligibility,
}

impl Policy {
    pub const VISUAL_DEFAULT: Self = Self {
        enabled: true,
        eligibility: Eligibility::OriginatingTerminalUnfocused,
    };
    pub const DELIVERY_DEFAULT: Self = Self {
        enabled: false,
        eligibility: Eligibility::ApplicationBackground,
    };

    pub fn permits(self, source: Option<Source>, focus: Focus) -> bool {
        self.enabled
            && match self.eligibility {
                Eligibility::Always => true,
                Eligibility::OriginatingTerminalUnfocused => {
                    source.is_some_and(|source| !focus.views(source))
                }
                Eligibility::ApplicationBackground => !focus.application_active,
            }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Attention {
    pub unread: bool,
    pub pulsing: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Aggregate {
    pub unread_panes: usize,
    pub pulsing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    TerminalBell,
    Application,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    session: u64,
    id: u64,
    kind: Kind,
    source: Option<Source>,
    received_at: Instant,
}

impl Event {
    pub fn session(self) -> u64 {
        self.session
    }

    pub fn id(self) -> u64 {
        self.id
    }

    pub fn kind(self) -> Kind {
        self.kind
    }

    pub fn source(self) -> Option<Source> {
        self.source
    }

    pub fn received_at(self) -> Instant {
        self.received_at
    }
}

/// the adapter owns its authoritative external identity. these opaque handles
/// are intentionally not provider names, workspace positions or plugin pids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Channel {
    session: u64,
    id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Job {
    channel: Channel,
    revision: u64,
    event: Event,
}

impl Job {
    pub fn channel(self) -> Channel {
        self.channel
    }

    pub fn event(self) -> Event {
        self.event
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Exhausted,
    StaleSource,
    StaleChannel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticKind {
    StaleSource,
    Coalesced,
    GlobalQueueFull,
    ChannelQueueFull,
}

/// diagnostics contain no terminal content, executable settings or event text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Diagnostic {
    pub kind: DiagnosticKind,
    pub occurrences: u64,
}

#[derive(Debug)]
struct LiveSource {
    token: Source,
    unread: bool,
    pulse_until: Option<Instant>,
    next_pulse: Option<Instant>,
}

#[derive(Debug)]
struct DeliveryChannel {
    policy: Policy,
    revision: u64,
    next_delivery: BTreeMap<Option<Source>, Instant>,
}

#[derive(Debug)]
pub struct Host {
    session: u64,
    next_source: u64,
    next_channel: u64,
    last_event: u64,
    visual: Policy,
    sources: BTreeMap<u64, LiveSource>,
    channels: BTreeMap<Channel, DeliveryChannel>,
    pending: VecDeque<Job>,
    diagnostics: VecDeque<Diagnostic>,
}

impl Host {
    pub fn new() -> Result<Self, Failure> {
        let previous = SESSION
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| Failure::Exhausted)?;
        Ok(Self {
            session: previous + 1,
            next_source: 0,
            next_channel: 0,
            last_event: 0,
            visual: Policy::VISUAL_DEFAULT,
            sources: BTreeMap::new(),
            channels: BTreeMap::new(),
            pending: VecDeque::new(),
            diagnostics: VecDeque::new(),
        })
    }

    pub fn attach(&mut self, pane: u64) -> Result<Source, Failure> {
        self.next_source = self.next_source.checked_add(1).ok_or(Failure::Exhausted)?;
        if let Some(old) = self.source(pane) {
            self.detach(old);
        }
        let token = Source {
            session: self.session,
            generation: self.next_source,
            pane,
        };
        self.sources.insert(
            pane,
            LiveSource {
                token,
                unread: false,
                pulse_until: None,
                next_pulse: None,
            },
        );
        Ok(token)
    }

    pub fn source(&self, pane: u64) -> Option<Source> {
        self.sources.get(&pane).map(|source| source.token)
    }

    pub fn is_live(&self, source: Source) -> bool {
        self.source(source.pane) == Some(source)
    }

    pub fn detach(&mut self, source: Source) {
        if !self.is_live(source) {
            return;
        }
        self.sources.remove(&source.pane);
        self.pending.retain(|job| job.event.source != Some(source));
        for channel in self.channels.values_mut() {
            channel.next_delivery.remove(&Some(source));
        }
    }

    pub fn visual_policy(&self) -> Policy {
        self.visual
    }

    pub fn set_visual_policy(&mut self, policy: Policy) {
        self.visual = policy;
        if !policy.enabled {
            // disabling is not a pause that replays old emphasis on re-enable.
            for source in self.sources.values_mut() {
                source.unread = false;
                source.pulse_until = None;
            }
        }
    }

    pub fn acknowledge(&mut self, focus: Focus) {
        if let Some(source) = focus.viewed.filter(|source| focus.views(*source)) {
            if self.is_live(source) {
                if let Some(state) = self.sources.get_mut(&source.pane) {
                    state.unread = false;
                }
            }
        }
    }

    pub fn attention(&self, source: Source, now: Instant) -> Attention {
        self.sources
            .get(&source.pane)
            .filter(|state| state.token == source)
            .map(|state| Attention {
                unread: state.unread,
                pulsing: state.pulse_until.is_some_and(|end| now < end),
            })
            .unwrap_or_default()
    }

    /// membership is supplied from the current workspace model on every read.
    /// no list index, colour or renamed label is ever retained by attention.
    pub fn aggregate(&self, members: impl IntoIterator<Item = u64>, now: Instant) -> Aggregate {
        let mut aggregate = Aggregate::default();
        for pane in members {
            if let Some(source) = self.source(pane) {
                let attention = self.attention(source, now);
                aggregate.unread_panes += usize::from(attention.unread);
                aggregate.pulsing |= attention.pulsing;
            }
        }
        aggregate
    }

    /// call synchronously at semantic host ingress, before deferred work. focus
    /// is snapshotted here, not re-evaluated when a queued delivery starts.
    pub fn bell(&mut self, source: Source, focus: Focus, now: Instant) -> Result<Event, Failure> {
        if !self.is_live(source) {
            self.diagnose(DiagnosticKind::StaleSource);
            return Err(Failure::StaleSource);
        }
        self.ingress(Kind::TerminalBell, Some(source), focus, now)
    }

    /// only host-authorized adapters may call this. absence of a target is not
    /// permission to substitute the selected terminal or create unread state.
    pub fn application_event(&mut self, focus: Focus, now: Instant) -> Result<Event, Failure> {
        self.ingress(Kind::Application, None, focus, now)
    }

    fn ingress(
        &mut self,
        kind: Kind,
        source: Option<Source>,
        focus: Focus,
        now: Instant,
    ) -> Result<Event, Failure> {
        self.last_event = self.last_event.checked_add(1).ok_or(Failure::Exhausted)?;
        let event = Event {
            session: self.session,
            id: self.last_event,
            kind,
            source,
            received_at: now,
        };
        self.acknowledge(focus);
        if self.visual.permits(source, focus) {
            if let Some(source) = source {
                if let Some(state) = self.sources.get_mut(&source.pane) {
                    state.unread = !focus.views(source);
                    if state.next_pulse.is_none_or(|next| now >= next) {
                        state.pulse_until = now.checked_add(PULSE);
                        state.next_pulse = now.checked_add(COOLDOWN);
                    }
                }
            }
        }
        // the eligible list is bounded by the host's admitted channel registry,
        // not by producer traffic. no unavailable adapter is invented here.
        let channels: Vec<_> = self
            .channels
            .iter()
            .filter(|(_, channel)| channel.policy.permits(source, focus))
            .map(|(id, channel)| (*id, channel.revision))
            .collect();
        for (channel, revision) in channels {
            self.enqueue(Job {
                channel,
                revision,
                event,
            });
        }
        Ok(event)
    }

    pub fn event_count(&self) -> u64 {
        self.last_event
    }

    pub fn register_channel(&mut self, policy: Policy) -> Result<Channel, Failure> {
        self.next_channel = self.next_channel.checked_add(1).ok_or(Failure::Exhausted)?;
        let id = Channel {
            session: self.session,
            id: self.next_channel,
        };
        self.channels.insert(
            id,
            DeliveryChannel {
                policy,
                revision: 0,
                next_delivery: BTreeMap::new(),
            },
        );
        Ok(id)
    }

    /// a settings change revokes in-flight tokens as well as queued work.
    /// adapters must also cancel their owned active operations on revocation.
    pub fn set_channel_policy(&mut self, id: Channel, policy: Policy) -> Result<(), Failure> {
        let channel = self.channels.get_mut(&id).ok_or(Failure::StaleChannel)?;
        if channel.policy == policy {
            return Ok(());
        }
        channel.revision = channel.revision.checked_add(1).ok_or(Failure::Exhausted)?;
        channel.policy = policy;
        channel.next_delivery.clear();
        self.pending.retain(|job| job.channel != id);
        Ok(())
    }

    pub fn remove_channel(&mut self, id: Channel) {
        self.channels.remove(&id);
        self.pending.retain(|job| job.channel != id);
    }

    fn enqueue(&mut self, job: Job) {
        if let Some(pending) = self.pending.iter_mut().find(|pending| {
            pending.channel == job.channel && pending.event.source == job.event.source
        }) {
            // keep its place: a continuous flood must not postpone another source.
            *pending = job;
            self.diagnose(DiagnosticKind::Coalesced);
        } else if self
            .pending
            .iter()
            .filter(|pending| pending.channel == job.channel)
            .count()
            >= MAX_PENDING_PER_CHANNEL
        {
            self.diagnose(DiagnosticKind::ChannelQueueFull);
        } else if self.pending.len() >= MAX_PENDING {
            // one source fanned out to many channels must not monopolize the
            // global budget. sacrifice its newest excess job for a quiet source.
            let mut counts = BTreeMap::<Option<Source>, usize>::new();
            for pending in &self.pending {
                *counts.entry(pending.event.source).or_default() += 1;
            }
            let incoming = counts.get(&job.event.source).copied().unwrap_or(0);
            if let Some((source, _)) = counts
                .into_iter()
                .max_by_key(|(_, count)| *count)
                .filter(|(_, count)| *count > incoming + 1)
            {
                if let Some(index) = self
                    .pending
                    .iter()
                    .rposition(|pending| pending.event.source == source)
                {
                    self.pending.remove(index);
                    self.pending.push_back(job);
                }
            }
            self.diagnose(DiagnosticKind::GlobalQueueFull);
        } else {
            self.pending.push_back(job);
        }
    }

    pub fn pending_count(&self, channel: Option<Channel>) -> usize {
        self.pending
            .iter()
            .filter(|job| channel.is_none_or(|channel| job.channel == channel))
            .count()
    }

    /// skipping cooling sources allows a quiet sibling to progress. a repeated
    /// bell replaces one queued event, but cannot extend the source deadline.
    pub fn take_ready(&mut self, id: Channel, now: Instant) -> Option<Job> {
        let channel = self.channels.get_mut(&id)?;
        let index = self.pending.iter().position(|job| {
            job.channel == id
                && channel
                    .next_delivery
                    .get(&job.event.source)
                    .is_none_or(|next| now >= *next)
        })?;
        let job = self.pending.remove(index)?;
        if let Some(next) = now.checked_add(COOLDOWN) {
            channel.next_delivery.insert(job.event.source, next);
        }
        Some(job)
    }

    pub fn is_current(&self, job: Job) -> bool {
        job.event.session == self.session
            && job.event.source.is_none_or(|source| self.is_live(source))
            && self
                .channels
                .get(&job.channel)
                .is_some_and(|channel| channel.policy.enabled && channel.revision == job.revision)
    }

    pub fn diagnostics(&self) -> &VecDeque<Diagnostic> {
        &self.diagnostics
    }

    fn diagnose(&mut self, kind: DiagnosticKind) {
        if let Some(last) = self.diagnostics.back_mut().filter(|last| last.kind == kind) {
            last.occurrences = last.occurrences.saturating_add(1);
        } else {
            if self.diagnostics.len() == MAX_DIAGNOSTICS {
                self.diagnostics.pop_front();
            }
            self.diagnostics.push_back(Diagnostic {
                kind,
                occurrences: 1,
            });
        }
    }
}

#[cfg(test)]
mod tests;
