use among::notifications::{Eligibility, Focus, Host, Policy};
use among::plugin_notifications::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

fn owner(id: u64) -> Owner {
    Owner {
        identity: format!("github:{id}/sample"),
        generation: "live-generation".into(),
    }
}
fn key(id: u64, channel: u64) -> ChannelKey {
    ChannelKey {
        identity: owner(id).identity,
        channel: format!("channel-{channel}"),
    }
}
fn input(sequence: u64, source: Option<among::notifications::Source>) -> EventInput {
    EventInput {
        sequence,
        target: source.map_or(Target::Application, |source| Target::Pane {
            reference: source.pane(),
        }),
        source_generation: source.map(|source| source.generation().to_string()),
        message: "data".into(),
        causal_job: None,
    }
}

#[test]
fn real_core_queue_limits_quiet_source_progress_and_session_revocation() {
    let core = Rc::new(RefCell::new(Host::new().unwrap()));
    let sources: Vec<_> = (1..=100)
        .map(|pane| core.borrow_mut().attach(pane).unwrap())
        .collect();
    let mut backend = Backend::new(core.clone(), Focus::default());
    assert!(backend.bind("session"));
    assert!(!backend.bind("other-session"));
    backend.register_identity(&owner(1).identity).unwrap();
    backend.admit(owner(1), false, None).unwrap();
    for id in 2..=6 {
        backend
            .declare(Declaration {
                key: key(id, 1),
                name: "Provider".into(),
                description: "Synthetic".into(),
            })
            .unwrap();
        backend.plugin_enabled(&owner(id).identity, true);
        backend
            .set_policy(
                &key(id, 1),
                Policy {
                    enabled: true,
                    eligibility: Eligibility::Always,
                },
            )
            .unwrap();
        backend.admit(owner(id), true, None).unwrap();
    }
    let now = Instant::now();
    for source in &sources {
        let event = core
            .borrow_mut()
            .bell(*source, Focus::default(), now)
            .unwrap();
        backend.observe_host_event(event, Some(10)).unwrap();
        assert!(core.borrow().pending_count(None) <= 64);
        assert!(backend.event_count() <= 64);
    }
    // the frozen core refuses new distinct sources once a channel already has
    // sixteen. record that bounded overload separately from single-source flood
    // fairness: clearing policy revokes that saturated batch, never replays it.
    assert!(core.borrow().diagnostics().iter().any(
        |diagnostic| diagnostic.kind == among::notifications::DiagnosticKind::ChannelQueueFull
    ));
    for id in 2..=6 {
        backend
            .set_policy(&key(id, 1), Policy::DELIVERY_DEFAULT)
            .unwrap();
        backend
            .set_policy(
                &key(id, 1),
                Policy {
                    enabled: true,
                    eligibility: Eligibility::Always,
                },
            )
            .unwrap();
    }
    // refreshing a noisy source cannot indefinitely postpone a quiet sibling.
    for _ in 0..100_000 {
        let event = core
            .borrow_mut()
            .bell(sources[0], Focus::default(), now)
            .unwrap();
        backend.observe_host_event(event, Some(10)).unwrap();
    }
    let quiet = core
        .borrow_mut()
        .bell(sources[99], Focus::default(), now)
        .unwrap();
    backend.observe_host_event(quiet, Some(10)).unwrap();
    let mut quiet_delivered = false;
    for step in 0..20 {
        for id in 2..=6 {
            if let Some(delivery) =
                backend.take_delivery(&owner(id), now + Duration::from_millis(step))
            {
                quiet_delivered |= delivery
                    .event
                    .source
                    .as_ref()
                    .is_some_and(|source| source.pane == "100");
                assert!(backend.active_count() <= MAX_ACTIVE);
                assert!(backend.result(
                    &owner(id),
                    delivery.job_id,
                    DeliveryOutcome::Accepted,
                    now + Duration::from_millis(step)
                ));
            }
        }
    }
    assert!(
        quiet_delivered,
        "quiet event lost behind repeated noisy-source replacement"
    );
    assert!(backend.completions().len() <= 32);
    backend.shutdown();
    assert!(backend.take_delivery(&owner(2), now).is_none());
    assert_eq!(core.borrow().pending_count(None), 0);
}

#[test]
fn source_generation_reuse_off_background_unfocused_and_tombstone_budgets() {
    let core = Rc::new(RefCell::new(Host::new().unwrap()));
    let source = core.borrow_mut().attach(1).unwrap();
    let mut backend = Backend::new(core.clone(), Focus::default());
    backend.bind("session");
    backend.register_identity(&owner(1).identity).unwrap();
    backend.admit(owner(1), false, None).unwrap();
    for channel in 1..=3 {
        backend
            .declare(Declaration {
                key: key(2, channel),
                name: "Channel".into(),
                description: "Synthetic".into(),
            })
            .unwrap();
    }
    backend.plugin_enabled(&owner(2).identity, true);
    backend
        .set_policy(
            &key(2, 1),
            Policy {
                enabled: true,
                eligibility: Eligibility::OriginatingTerminalUnfocused,
            },
        )
        .unwrap();
    backend
        .set_policy(
            &key(2, 2),
            Policy {
                enabled: true,
                eligibility: Eligibility::ApplicationBackground,
            },
        )
        .unwrap();
    backend.admit(owner(2), true, None).unwrap();
    let now = Instant::now();
    backend.set_focus(Focus {
        application_active: true,
        viewed: Some(source),
    });
    backend
        .produce(&owner(1), input(1, None), None, now)
        .unwrap();
    assert!(backend.take_delivery(&owner(2), now).is_none());
    backend.set_focus(Focus::default());
    backend
        .produce(
            &owner(1),
            input(2, None),
            None,
            now + Duration::from_secs(2),
        )
        .unwrap();
    let job = backend
        .take_delivery(&owner(2), now + Duration::from_secs(2))
        .unwrap();
    assert_eq!(job.channel, "channel-2");
    backend.result(
        &owner(2),
        job.job_id,
        DeliveryOutcome::Accepted,
        now + Duration::from_secs(2),
    );
    core.borrow_mut().detach(source);
    let replacement = core.borrow_mut().attach(1).unwrap();
    assert_ne!(replacement, source);
    assert_eq!(
        backend.produce(
            &owner(1),
            input(3, Some(source)),
            Some(10),
            now + Duration::from_secs(4)
        ),
        Err(Rejection::Stale)
    );
    for id in 1..=MAX_CONTRIBUTIONS_PER_PLUGIN {
        let contribution = ContributionInput {
            id: format!("item-{id}"),
            revision: 1,
            scope: Scope {
                slot: "app_header".into(),
                target: Target::Application,
            },
            source_generation: None,
            text: "safe".into(),
            tooltip: None,
            icon: None,
            style: Style::Default,
            actions: Vec::new(),
        };
        backend
            .contribute(&owner(1), contribution, None, now)
            .unwrap();
        backend.withdraw(&owner(1), &format!("item-{id}"));
    }
    let contribution = ContributionInput {
        id: "over-budget".into(),
        revision: 1,
        scope: Scope {
            slot: "app_header".into(),
            target: Target::Application,
        },
        source_generation: None,
        text: "safe".into(),
        tooltip: None,
        icon: None,
        style: Style::Default,
        actions: Vec::new(),
    };
    assert_eq!(
        backend.contribute(&owner(1), contribution, None, now),
        Err(Rejection::Full)
    );
    backend.uninstall(&owner(2).identity);
    assert!(backend
        .inventory()
        .iter()
        .all(|view| !view.installed && view.owner.is_none()));
    assert!(backend.inventory().iter().any(|view| view.policy.enabled));
}
