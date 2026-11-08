use super::{permits, Text};
use crate::notifications::{
    Channel, Failure, Focus, Host, Kind, Policy, Registration, Source, EVENT_LIFETIME,
};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// One attempt per 100 ms globally; each terminal also keeps the core's
/// two-second cooldown. Work waiting for this budget stays in the core queue.
pub const GLOBAL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    Checking,
    Available,
    Unavailable,
}

/// A click binds the backend incarnation and the complete live terminal
/// identity. A reused pane number cannot target a replacement shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub instance: String,
    pub session: u64,
    pub generation: u64,
    pub pane: u64,
}

/// GIO reports an attempted send, not that the notification was displayed.
/// Adapters must not add blocking work or queue a retry around the platform API.
pub trait Sender {
    fn availability(&self) -> Availability;
    fn attempt(&mut self, id: &str, text: &Text, target: &Target);
    fn withdraw(&mut self, id: &str);

    /// Stop accepting new work. Synchronous test senders have nothing to drain.
    fn shutdown(&mut self) {}

    /// True when no platform send or close is still in flight. Test senders that
    /// complete synchronously keep the default.
    fn is_quiescent(&self) -> bool {
        true
    }
}

pub struct Runtime {
    channel: Channel,
    _registration: Registration,
    policy: Policy,
    instance: String,
    sender: Box<dyn Sender>,
    posted: BTreeMap<Source, String>,
    next_attempt: Option<Instant>,
    configuration_failure: Option<Failure>,
    closed: bool,
}

impl Runtime {
    pub fn new(
        host: &mut Host,
        sender: Box<dyn Sender>,
        instance: String,
    ) -> Result<Self, Failure> {
        let channel = host.register_terminal_bell_channel(Policy::DESKTOP_DEFAULT)?;
        let registration = host.registration(channel).ok_or(Failure::StaleChannel)?;
        Ok(Self {
            channel,
            _registration: registration,
            policy: Policy::DESKTOP_DEFAULT,
            instance,
            sender,
            posted: BTreeMap::new(),
            next_attempt: None,
            configuration_failure: None,
            closed: false,
        })
    }

    pub fn availability(&self) -> Availability {
        self.sender.availability()
    }

    pub fn configuration_failure(&self) -> Option<Failure> {
        self.configuration_failure
    }

    /// Capture the desktop-only suppression rule in the semantic bell signal.
    /// A later focus change or service appearance cannot revive that event.
    pub fn ingress(&self, host: &mut Host, source: Source, focus: Focus) {
        if self.configuration_failure.is_some()
            || !permits(self.policy, Some(source), focus)
            || self.sender.availability() == Availability::Unavailable
        {
            host.discard_pending_source(self.channel, source);
        }
    }

    pub fn configure(&mut self, host: &mut Host, policy: Policy) -> Result<(), Failure> {
        if self.closed {
            return Err(Failure::StaleChannel);
        }
        if policy == self.policy && self.configuration_failure.is_none() {
            return Ok(());
        }
        // the core retires old authority before allocating its replacement.
        // even a refused edit must remove notifications owned by that revision.
        self.withdraw_all();
        if let Err(reason) = host.set_channel_policy(self.channel, policy) {
            self.configuration_failure = Some(reason);
            host.discard_pending(self.channel);
            return Err(reason);
        }
        self.policy = policy;
        self.configuration_failure = None;
        Ok(())
    }

    pub fn target(&self, source: Source) -> Target {
        Target {
            instance: self.instance.clone(),
            session: source.session(),
            generation: source.generation(),
            pane: source.pane(),
        }
    }

    pub fn resolve(&self, host: &Host, target: &Target) -> Option<Source> {
        if self.closed || target.instance != self.instance {
            return None;
        }
        let source = host.source(target.pane)?;
        (source.session() == target.session && source.generation() == target.generation)
            .then_some(source)
    }

    pub fn observe(&mut self, host: &mut Host, focus: Focus) {
        if focus.application_active {
            if let Some(source) = focus.viewed {
                // Viewing acknowledges even a bell still waiting for a send
                // budget. Leaving the terminal must not revive that bell.
                host.discard_pending_source(self.channel, source);
            }
        }
        let removed: Vec<_> = self
            .posted
            .keys()
            .copied()
            .filter(|source| {
                host.source(source.pane()) != Some(*source)
                    || (focus.application_active && focus.viewed == Some(*source))
            })
            .collect();
        for source in removed {
            if let Some(id) = self.posted.remove(&source) {
                self.sender.withdraw(&id);
            }
        }
    }

    pub fn poll(
        &mut self,
        host: &mut Host,
        focus: Focus,
        now: Instant,
        mut text: impl FnMut(Source) -> Option<Text>,
    ) {
        self.observe(host, focus);
        if self.closed
            || self.configuration_failure.is_some()
            || self.sender.availability() == Availability::Unavailable
        {
            // A missing service does not turn old bells into later popups.
            host.discard_pending(self.channel);
            return;
        }
        // discovery has not established absence. keep only the core's bounded
        // jobs; focus, policy, source identity and expiry are rechecked on send.
        if self.sender.availability() == Availability::Checking {
            return;
        }
        if self.next_attempt.is_some_and(|next| now < next) {
            return;
        }
        while let Some(job) = host.take_ready(self.channel, now) {
            let event = job.event();
            if !host.is_current(job)
                || event.kind() != Kind::TerminalBell
                || now.saturating_duration_since(event.received_at()) > EVENT_LIFETIME
                || !permits(self.policy, event.source(), focus)
            {
                continue;
            }
            let Some(source) = event.source() else {
                continue;
            };
            let Some(text) = text(source) else {
                continue;
            };
            let id = format!(
                "bell-{}-{}-{}-{}",
                self.instance,
                source.session(),
                source.generation(),
                source.pane()
            );
            let target = self.target(source);
            self.sender.attempt(&id, &text, &target);
            self.posted.insert(source, id);
            self.next_attempt = now.checked_add(GLOBAL_INTERVAL);
            break;
        }
    }

    pub fn shutdown(&mut self, host: &mut Host) {
        self.closed = true;
        host.remove_channel(self.channel);
        self.sender.shutdown();
        self.withdraw_all();
    }

    pub fn is_quiescent(&self) -> bool {
        self.sender.is_quiescent()
    }

    fn withdraw_all(&mut self) {
        for (_, id) in std::mem::take(&mut self.posted) {
            self.sender.withdraw(&id);
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.withdraw_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::{Eligibility, COOLDOWN};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Call {
        Send(String, Text, Target),
        Withdraw(String),
    }

    struct Recorder {
        calls: Rc<RefCell<Vec<Call>>>,
        available: Rc<Cell<Availability>>,
    }
    impl Sender for Recorder {
        fn availability(&self) -> Availability {
            self.available.get()
        }
        fn attempt(&mut self, id: &str, text: &Text, target: &Target) {
            self.calls
                .borrow_mut()
                .push(Call::Send(id.into(), text.clone(), target.clone()));
        }
        fn withdraw(&mut self, id: &str) {
            self.calls.borrow_mut().push(Call::Withdraw(id.into()));
        }
    }
    fn text(_: Source) -> Option<Text> {
        Some(Text::new(Some(b"build"), Some(b"make"), Some(b"/work")))
    }

    #[test]
    fn failed_policy_replacement_withdraws_and_never_uses_revoked_authority() {
        let mut host = Host::new().unwrap();
        let source = host.attach(1).unwrap();
        let waiting = host.attach(2).unwrap();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let mut runtime = Runtime::new(
            &mut host,
            Box::new(Recorder {
                calls: calls.clone(),
                available: Rc::new(Cell::new(Availability::Available)),
            }),
            "instance".into(),
        )
        .unwrap();
        let now = Instant::now();
        host.bell(source, Focus::default(), now).unwrap();
        runtime.poll(&mut host, Focus::default(), now, text);
        let Call::Send(id, _, _) = calls.borrow()[0].clone() else {
            panic!("the initial notification was not sent");
        };
        host.bell(source, Focus::default(), now + COOLDOWN).unwrap();
        let job = host.take_ready(runtime.channel, now + COOLDOWN).unwrap();
        let authority = host.authorization(job).unwrap();
        host.bell(waiting, Focus::default(), now + COOLDOWN)
            .unwrap();
        host.channels.get_mut(&runtime.channel).unwrap().revision = u64::MAX;
        assert_eq!(
            runtime.configure(
                &mut host,
                Policy {
                    enabled: false,
                    ..Policy::DESKTOP_DEFAULT
                }
            ),
            Err(Failure::Exhausted)
        );
        assert_eq!(runtime.configuration_failure(), Some(Failure::Exhausted));
        assert_eq!(calls.borrow().last(), Some(&Call::Withdraw(id)));
        assert!(runtime.posted.is_empty());
        assert!(!authority.is_current());
        assert_eq!(host.pending_count(Some(runtime.channel)), 0);
        host.bell(source, Focus::default(), now + COOLDOWN * 2)
            .unwrap();
        runtime.poll(&mut host, Focus::default(), now + COOLDOWN * 2, text);
        assert_eq!(calls.borrow().len(), 2);
        assert_eq!(
            runtime.configure(&mut host, Policy::DESKTOP_DEFAULT),
            Err(Failure::Exhausted),
            "retrying the previous policy must not take the equality success path"
        );
        assert_eq!(host.channels.len(), 1);
    }

    #[test]
    fn discovery_preserves_only_bounded_core_work_and_resolves_text_at_send() {
        let mut host = Host::new().unwrap();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let available = Rc::new(Cell::new(Availability::Checking));
        let mut runtime = Runtime::new(
            &mut host,
            Box::new(Recorder {
                calls: calls.clone(),
                available: available.clone(),
            }),
            "instance".into(),
        )
        .unwrap();
        let now = Instant::now();
        for pane in 1..=crate::notifications::MAX_PENDING_PER_CHANNEL + 1 {
            let source = host.attach(pane as u64).unwrap();
            host.bell(source, Focus::default(), now).unwrap();
            runtime.ingress(&mut host, source, Focus::default());
        }
        let builds = Cell::new(0);
        runtime.poll(&mut host, Focus::default(), now, |source| {
            builds.set(builds.get() + 1);
            text(source)
        });
        assert_eq!(builds.get(), 0);
        assert!(calls.borrow().is_empty());
        assert_eq!(
            host.pending_count(Some(runtime.channel)),
            crate::notifications::MAX_PENDING_PER_CHANNEL
        );
        available.set(Availability::Available);
        let fresh = Text::new(Some(b"renamed"), Some(b"new title"), Some(b"/new"));
        runtime.poll(&mut host, Focus::default(), now + GLOBAL_INTERVAL, |_| {
            builds.set(builds.get() + 1);
            Some(fresh.clone())
        });
        assert_eq!(builds.get(), 1);
        let recorded = calls.borrow();
        let [Call::Send(_, actual, _)] = recorded.as_slice() else {
            panic!("discovery did not release exactly one core job");
        };
        assert_eq!(actual, &fresh);
    }

    #[test]
    fn discovery_revalidates_focus_policy_source_and_expiry_before_delivery() {
        for invalidation in [
            "viewed",
            "foreground",
            "policy",
            "source",
            "expired",
            "unavailable",
        ] {
            let mut host = Host::new().unwrap();
            let source = host.attach(1).unwrap();
            let calls = Rc::new(RefCell::new(Vec::new()));
            let available = Rc::new(Cell::new(Availability::Checking));
            let mut runtime = Runtime::new(
                &mut host,
                Box::new(Recorder {
                    calls: calls.clone(),
                    available: available.clone(),
                }),
                "instance".into(),
            )
            .unwrap();
            let now = Instant::now();
            host.bell(source, Focus::default(), now).unwrap();
            runtime.ingress(&mut host, source, Focus::default());
            runtime.poll(&mut host, Focus::default(), now, text);
            assert_eq!(host.pending_count(Some(runtime.channel)), 1);
            match invalidation {
                "viewed" => runtime.observe(
                    &mut host,
                    Focus {
                        application_active: true,
                        viewed: Some(source),
                    },
                ),
                "policy" => {
                    runtime
                        .configure(
                            &mut host,
                            Policy {
                                enabled: false,
                                ..Policy::DESKTOP_DEFAULT
                            },
                        )
                        .unwrap();
                    runtime
                        .configure(&mut host, Policy::DESKTOP_DEFAULT)
                        .unwrap();
                }
                "source" => {
                    host.detach(source);
                    host.attach(source.pane()).unwrap();
                }
                "unavailable" => {
                    available.set(Availability::Unavailable);
                    runtime.poll(&mut host, Focus::default(), now + GLOBAL_INTERVAL, text);
                }
                "expired" | "foreground" => {}
                _ => unreachable!(),
            }
            available.set(Availability::Available);
            let ready = if invalidation == "expired" {
                now + EVENT_LIFETIME + Duration::from_millis(1)
            } else {
                now + GLOBAL_INTERVAL * 2
            };
            let focus = Focus {
                application_active: invalidation == "foreground",
                viewed: None,
            };
            runtime.poll(&mut host, focus, ready, text);
            runtime.poll(&mut host, Focus::default(), ready + GLOBAL_INTERVAL, text);
            assert!(calls.borrow().is_empty(), "replayed after {invalidation}");
            assert_eq!(host.pending_count(Some(runtime.channel)), 0);
            let current = host.source(source.pane()).unwrap();
            host.bell(current, Focus::default(), ready + COOLDOWN)
                .unwrap();
            runtime.poll(&mut host, Focus::default(), ready + COOLDOWN, text);
            assert_eq!(
                calls.borrow().len(),
                1,
                "fresh bell lost after {invalidation}"
            );
        }
    }

    #[test]
    fn eligible_bells_replace_and_focus_close_and_exit_withdraw() {
        let mut host = Host::new().unwrap();
        let source = host.attach(1).unwrap();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let mut runtime = Runtime::new(
            &mut host,
            Box::new(Recorder {
                calls: calls.clone(),
                available: Rc::new(Cell::new(Availability::Available)),
            }),
            "instance".into(),
        )
        .unwrap();
        let now = Instant::now();
        host.bell(source, Focus::default(), now).unwrap();
        runtime.poll(&mut host, Focus::default(), now, text);
        host.bell(source, Focus::default(), now + COOLDOWN).unwrap();
        runtime.poll(&mut host, Focus::default(), now + COOLDOWN, text);
        let recorded = calls.borrow();
        assert_eq!(recorded.len(), 2);
        let Call::Send(first, payload, target) = &recorded[0] else {
            panic!("no send");
        };
        assert_eq!(payload, &text(source).unwrap());
        assert_eq!(runtime.resolve(&host, target), Some(source));
        let Call::Send(second, _, _) = &recorded[1] else {
            panic!("no replacement");
        };
        assert_eq!(first, second);
        let id = first.clone();
        drop(recorded);
        runtime.observe(
            &mut host,
            Focus {
                application_active: true,
                viewed: Some(source),
            },
        );
        assert_eq!(calls.borrow().last(), Some(&Call::Withdraw(id)));
        host.bell(source, Focus::default(), now + COOLDOWN * 2)
            .unwrap();
        runtime.poll(&mut host, Focus::default(), now + COOLDOWN * 2, text);
        host.detach(source);
        runtime.observe(&mut host, Focus::default());
        assert!(matches!(calls.borrow().last(), Some(Call::Withdraw(_))));
        let replacement = host.attach(1).unwrap();
        host.bell(replacement, Focus::default(), now + COOLDOWN * 3)
            .unwrap();
        runtime.poll(&mut host, Focus::default(), now + COOLDOWN * 3, text);
        runtime.shutdown(&mut host);
        assert!(matches!(calls.borrow().last(), Some(Call::Withdraw(_))));
        assert_eq!(
            calls
                .borrow()
                .iter()
                .filter(|call| matches!(call, Call::Send(..)))
                .count(),
            4
        );
    }

    #[test]
    fn viewed_off_stale_and_missing_service_bells_never_replay() {
        let mut host = Host::new().unwrap();
        let source = host.attach(1).unwrap();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let available = Rc::new(Cell::new(Availability::Unavailable));
        let mut runtime = Runtime::new(
            &mut host,
            Box::new(Recorder {
                calls: calls.clone(),
                available: available.clone(),
            }),
            "instance".into(),
        )
        .unwrap();
        let now = Instant::now();
        host.bell(source, Focus::default(), now).unwrap();
        runtime.poll(&mut host, Focus::default(), now, text);
        available.set(Availability::Available);
        runtime.poll(&mut host, Focus::default(), now + COOLDOWN, text);
        assert!(calls.borrow().is_empty());
        runtime
            .configure(
                &mut host,
                Policy {
                    enabled: true,
                    eligibility: Eligibility::Always,
                },
            )
            .unwrap();
        let viewed = Focus {
            application_active: true,
            viewed: Some(source),
        };
        host.bell(source, viewed, now + COOLDOWN * 2).unwrap();
        runtime.poll(&mut host, viewed, now + COOLDOWN * 2, text);
        assert!(calls.borrow().is_empty());
        runtime
            .configure(
                &mut host,
                Policy {
                    enabled: false,
                    eligibility: Eligibility::Always,
                },
            )
            .unwrap();
        host.bell(source, Focus::default(), now + COOLDOWN * 3)
            .unwrap();
        runtime.poll(&mut host, Focus::default(), now + COOLDOWN * 3, text);
        runtime
            .configure(&mut host, Policy::DESKTOP_DEFAULT)
            .unwrap();
        runtime.poll(&mut host, Focus::default(), now + COOLDOWN * 4, text);
        assert!(calls.borrow().is_empty());
        let target = runtime.target(source);
        host.detach(source);
        host.attach(source.pane()).unwrap();
        assert!(runtime.resolve(&host, &target).is_none());
        let mut foreign = target;
        foreign.instance = "previous process".into();
        assert!(runtime.resolve(&host, &foreign).is_none());
    }

    #[test]
    fn briefly_viewing_a_queued_bell_prevents_replay_after_leaving_the_terminal() {
        for source_cooldown in [false, true] {
            let mut host = Host::new().unwrap();
            let first = host.attach(1).unwrap();
            let waiting = if source_cooldown {
                first
            } else {
                host.attach(2).unwrap()
            };
            let calls = Rc::new(RefCell::new(Vec::new()));
            let mut runtime = Runtime::new(
                &mut host,
                Box::new(Recorder {
                    calls: calls.clone(),
                    available: Rc::new(Cell::new(Availability::Available)),
                }),
                "instance".into(),
            )
            .unwrap();
            let now = Instant::now();
            host.bell(first, Focus::default(), now).unwrap();
            runtime.poll(&mut host, Focus::default(), now, text);
            let queued = now + Duration::from_millis(10);
            host.bell(waiting, Focus::default(), queued).unwrap();
            runtime.poll(&mut host, Focus::default(), queued, text);
            assert_eq!(host.pending_count(Some(runtime.channel)), 1);

            let viewed = Focus {
                application_active: true,
                viewed: Some(waiting),
            };
            runtime.poll(&mut host, viewed, now + Duration::from_millis(20), text);
            runtime.poll(&mut host, Focus::default(), now + COOLDOWN, text);
            assert_eq!(
                calls
                    .borrow()
                    .iter()
                    .filter(|call| matches!(call, Call::Send(..)))
                    .count(),
                1,
                "viewed bell replayed after leaving the terminal; source cooldown={source_cooldown}"
            );
            assert_eq!(host.pending_count(Some(runtime.channel)), 0);

            let fresh = now + COOLDOWN * 2;
            host.bell(waiting, Focus::default(), fresh).unwrap();
            runtime.poll(&mut host, Focus::default(), fresh, text);
            assert_eq!(
                calls
                    .borrow()
                    .iter()
                    .filter(|call| matches!(call, Call::Send(..)))
                    .count(),
                2,
                "a fresh bell must still be delivered"
            );
        }
    }

    #[test]
    fn global_attempt_budget_keeps_the_quiet_source_in_the_core_queue() {
        let mut host = Host::new().unwrap();
        let loud = host.attach(1).unwrap();
        let quiet = host.attach(2).unwrap();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let mut runtime = Runtime::new(
            &mut host,
            Box::new(Recorder {
                calls: calls.clone(),
                available: Rc::new(Cell::new(Availability::Available)),
            }),
            "instance".into(),
        )
        .unwrap();
        let now = Instant::now();
        for millis in 0..5_000 {
            let time = now + Duration::from_millis(millis);
            host.bell(loud, Focus::default(), time).unwrap();
            if millis == 1 {
                host.bell(quiet, Focus::default(), time).unwrap();
            }
            runtime.poll(&mut host, Focus::default(), time, text);
        }
        let calls = calls.borrow();
        let sends: Vec<_> = calls
            .iter()
            .filter_map(|call| match call {
                Call::Send(_, _, target) => Some(target.pane),
                _ => None,
            })
            .collect();
        assert_eq!(sends, [loud.pane(), quiet.pane(), loud.pane(), loud.pane()]);
    }

    #[test]
    fn ingress_suppression_survives_focus_changes_and_leaves_other_channels_alone() {
        let mut host = Host::new().unwrap();
        let source = host.attach(1).unwrap();
        let policy = Policy {
            enabled: true,
            eligibility: Eligibility::Always,
        };
        let other = host.register_channel(policy).unwrap();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let mut runtime = Runtime::new(
            &mut host,
            Box::new(Recorder {
                calls: calls.clone(),
                available: Rc::new(Cell::new(Availability::Available)),
            }),
            "instance".into(),
        )
        .unwrap();
        runtime.configure(&mut host, policy).unwrap();
        let now = Instant::now();
        let viewed = Focus {
            application_active: true,
            viewed: Some(source),
        };
        let bell = host.bell(source, viewed, now).unwrap();
        runtime.ingress(&mut host, source, viewed);
        runtime.poll(&mut host, Focus::default(), now, text);
        assert!(calls.borrow().is_empty());
        assert_eq!(host.take_ready(other, now).unwrap().event(), bell);
        host.application_event(Focus::default(), now + COOLDOWN)
            .unwrap();
        assert_eq!(host.pending_count(Some(runtime.channel)), 0);
        assert_eq!(host.pending_count(Some(other)), 1);
    }
}
