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
/// Implementations must not block the UI loop or queue a retry.
pub trait Sender {
    fn availability(&self) -> Availability;
    fn attempt(&mut self, id: &str, text: &Text, target: &Target);
    fn withdraw(&mut self, id: &str);
}

pub struct Runtime {
    channel: Channel,
    _registration: Registration,
    policy: Policy,
    instance: String,
    sender: Box<dyn Sender>,
    posted: BTreeMap<Source, String>,
    next_attempt: Option<Instant>,
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
            closed: false,
        })
    }

    pub fn availability(&self) -> Availability {
        self.sender.availability()
    }

    /// Capture the desktop-only suppression rule in the semantic bell signal.
    /// A later focus change or service appearance cannot revive that event.
    pub fn ingress(&self, host: &mut Host, source: Source, focus: Focus) {
        if !permits(self.policy, Some(source), focus)
            || self.sender.availability() != Availability::Available
        {
            host.discard_pending_source(self.channel, source);
        }
    }

    pub fn configure(&mut self, host: &mut Host, policy: Policy) -> Result<(), Failure> {
        if self.closed || policy == self.policy {
            return Ok(());
        }
        host.set_channel_policy(self.channel, policy)?;
        self.policy = policy;
        self.withdraw_all();
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

    pub fn observe(&mut self, host: &Host, focus: Focus) {
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
        if self.closed || self.sender.availability() != Availability::Available {
            // A missing service does not turn old bells into later popups.
            host.discard_pending(self.channel);
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
        self.withdraw_all();
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
            &host,
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
        runtime.observe(&host, Focus::default());
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
