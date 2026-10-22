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
        backend
            .observe_host_event(event, Some(10), Focus::default())
            .unwrap();
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
    // the quiet event precedes the flood: replacing noisy envelopes must not
    // evict it while its shared-core job is still waiting in the same position.
    let quiet = core
        .borrow_mut()
        .bell(sources[99], Focus::default(), now)
        .unwrap();
    backend
        .observe_host_event(quiet, Some(10), Focus::default())
        .unwrap();
    for _ in 0..100_000 {
        let event = core
            .borrow_mut()
            .bell(sources[0], Focus::default(), now)
            .unwrap();
        backend
            .observe_host_event(event, Some(10), Focus::default())
            .unwrap();
    }
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

#[test]
fn independent_origin_lease_revokes_active_and_queued_shared_core_work() {
    let core = Rc::new(RefCell::new(Host::new().unwrap()));
    let mut backend = Backend::new(core.clone(), Focus::default());
    backend.bind("session");
    backend.register_identity(&owner(1).identity).unwrap();
    let origin_lease = backend.admit(owner(1), false, None).unwrap();
    backend
        .declare(Declaration {
            key: key(2, 1),
            name: "Provider".into(),
            description: "Synthetic".into(),
        })
        .unwrap();
    backend.plugin_enabled(&owner(2).identity, true);
    backend
        .set_policy(
            &key(2, 1),
            Policy {
                enabled: true,
                eligibility: Eligibility::Always,
            },
        )
        .unwrap();
    backend.admit(owner(2), true, None).unwrap();
    let now = Instant::now();
    let event = backend
        .produce(&owner(1), input(1, None), None, now)
        .unwrap();
    let job = backend.take_delivery(&owner(2), now).unwrap();
    assert!(backend.envelope(event, now).is_some());
    // stop_io can clear this without acquiring the renderer/backend RefCell.
    let worker_lease = backend.origin_lease(event, now).unwrap();
    origin_lease.revoke();
    assert!(!std::thread::spawn(move || worker_lease.is_current())
        .join()
        .unwrap());
    assert!(backend.envelope(event, now).is_none());
    backend.housekeep(now);
    assert_eq!(backend.active_count(), 0);
    assert_eq!(
        backend.completions().back().unwrap().status,
        Status::CancelledUncertain
    );
    assert!(!backend.result(&owner(2), job.job_id, DeliveryOutcome::Accepted, now));

    let replacement = Owner {
        identity: owner(1).identity,
        generation: "replacement-generation".into(),
    };
    let replacement_lease = backend.admit(replacement.clone(), false, None).unwrap();
    let now = now + Duration::from_secs(2);
    let queued = backend
        .produce(&replacement, input(1, None), None, now)
        .unwrap();
    assert_eq!(core.borrow().pending_count(None), 1);
    replacement_lease.revoke();
    assert!(backend.envelope(queued, now).is_none());
    assert!(backend.take_delivery(&owner(2), now).is_none());
    assert_eq!(core.borrow().pending_count(None), 0);
}

#[test]
fn later_ineligible_event_cannot_erase_another_channels_earlier_eligible_job() {
    let core = Rc::new(RefCell::new(Host::new().unwrap()));
    let source = core.borrow_mut().attach(1).unwrap();
    let mut backend = Backend::new(core.clone(), Focus::default());
    backend.bind("session");
    backend.register_identity(&owner(1).identity).unwrap();
    backend.admit(owner(1), false, None).unwrap();
    for channel in 1..=2 {
        backend
            .declare(Declaration {
                key: key(2, channel),
                name: "Provider".into(),
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
                eligibility: Eligibility::ApplicationBackground,
            },
        )
        .unwrap();
    backend
        .set_policy(
            &key(2, 2),
            Policy {
                enabled: true,
                eligibility: Eligibility::Always,
            },
        )
        .unwrap();
    backend.admit(owner(2), true, None).unwrap();
    let now = Instant::now();
    let background = backend
        .produce(&owner(1), input(1, Some(source)), Some(10), now)
        .unwrap();
    backend.set_focus(Focus {
        application_active: true,
        viewed: Some(source),
    });
    let foreground = backend
        .produce(
            &owner(1),
            input(2, Some(source)),
            Some(10),
            now + Duration::from_secs(2),
        )
        .unwrap();
    let background_job = backend
        .take_delivery(&owner(2), now + Duration::from_secs(2))
        .unwrap();
    assert_eq!(background_job.channel, "channel-1");
    assert_eq!(background_job.event.event_id, background.id().to_string());
    let always_job = backend
        .take_delivery(&owner(2), now + Duration::from_secs(2))
        .unwrap();
    assert_eq!(always_job.channel, "channel-2");
    assert_eq!(always_job.event.event_id, foreground.id().to_string());
}

#[test]
fn managed_ingress_keeps_its_kind_in_shared_core_jobs() {
    let core = Rc::new(RefCell::new(Host::new().unwrap()));
    let source = core.borrow_mut().attach(1).unwrap();
    let channel = core
        .borrow_mut()
        .register_channel(Policy {
            enabled: true,
            eligibility: Eligibility::Always,
        })
        .unwrap();
    let mut backend = Backend::new(core.clone(), Focus::default());
    backend.bind("session");
    backend.register_identity(&owner(1).identity).unwrap();
    backend.admit(owner(1), false, None).unwrap();
    let now = Instant::now();
    for (index, target) in [Some(source), None].into_iter().enumerate() {
        let at = now + Duration::from_secs(index as u64 * 2);
        let event = backend
            .produce(
                &owner(1),
                input(index as u64 + 1, target),
                target.map(|_| 10),
                at,
            )
            .unwrap();
        let job = core.borrow_mut().take_ready(channel, at).unwrap();
        assert_eq!(job.event(), event);
        assert_eq!(
            job.event().kind(),
            among::notifications::Kind::ManagedNotification,
            "the backend envelope cannot repair a mislabelled shared-core job"
        );
        let origin = job.event().origin().unwrap();
        assert_eq!(
            core.borrow().origin(origin).unwrap().identity,
            owner(1).identity
        );
        assert_eq!(
            core.borrow().origin(origin).unwrap().generation,
            owner(1).generation
        );
    }
}

#[test]
fn quiet_targetless_origin_progresses_after_noisy_production_has_begun() {
    let core = Rc::new(RefCell::new(Host::new().unwrap()));
    let channel = core
        .borrow_mut()
        .register_channel(Policy {
            enabled: true,
            eligibility: Eligibility::Always,
        })
        .unwrap();
    let mut backend = Backend::new(core.clone(), Focus::default());
    backend.bind("session");
    backend.register_identity(&owner(1).identity).unwrap();
    backend.admit(owner(1), false, None).unwrap();
    let now = Instant::now();
    // accepted managed events, not rate-limit refusals. quiet arrives only after
    // this producer has flooded the shared scheduler and acquired a cooldown.
    for sequence in 1..=1_000 {
        backend
            .produce(
                &owner(1),
                input(sequence, None),
                None,
                now + Duration::from_secs((sequence - 1) * 2),
            )
            .unwrap();
    }
    let at = now + Duration::from_secs(1_998);
    let noisy = core.borrow_mut().take_ready(channel, at).unwrap();
    assert_eq!(
        backend.envelope(noisy.event(), at).unwrap().origin.as_ref(),
        Some(&owner(1))
    );
    let quiet_at = at + Duration::from_millis(1);
    backend.register_identity(&owner(3).identity).unwrap();
    backend.admit(owner(3), false, None).unwrap();
    let quiet = backend
        .produce(&owner(3), input(1, None), None, quiet_at)
        .unwrap();
    let job = core
        .borrow_mut()
        .take_ready(channel, quiet_at)
        .expect("a later quiet producer must not share the noisy producer's cooldown");
    assert_eq!(job.event(), quiet);
    assert_eq!(
        backend
            .envelope(job.event(), quiet_at)
            .unwrap()
            .origin
            .as_ref(),
        Some(&owner(3))
    );
}

#[test]
fn joined_delivery_authority_rechecks_kind_origin_source_policy_and_worker_revocation() {
    let core = Rc::new(RefCell::new(Host::new().unwrap()));
    let source = core.borrow_mut().attach(1).unwrap();
    let on = Policy {
        enabled: true,
        eligibility: Eligibility::Always,
    };
    let channel = core.borrow_mut().register_channel(on).unwrap();
    let mut backend = Backend::new(core.clone(), Focus::default());
    backend.bind("session");
    backend.register_identity(&owner(1).identity).unwrap();
    let lease = backend.admit(owner(1), false, None).unwrap();
    let now = Instant::now();
    let event = backend
        .produce(&owner(1), input(1, Some(source)), Some(10), now)
        .unwrap();
    let job = core.borrow_mut().take_ready(channel, now).unwrap();
    let authorization = backend.authorize_delivery(job, now).unwrap();
    assert_eq!(authorization.job(), job);
    assert_eq!(authorization.envelope().kind, "managed_notification");
    assert_eq!(authorization.envelope().origin.as_ref(), Some(&owner(1)));
    assert!(authorization.is_current(&core.borrow(), now));
    assert!(!authorization.is_current(&Host::new().unwrap(), now));
    // the core and worker read the same atomic lease even while the main thread
    // holds immutable model borrows and cleanup cannot remove queued metadata.
    let queued = backend
        .produce(
            &owner(1),
            input(2, None),
            None,
            now + Duration::from_secs(2),
        )
        .unwrap();
    let core_read = core.borrow();
    let model: &Host = &core_read;
    let worker = authorization.clone();
    let backend_read = &backend;
    std::thread::scope(|scope| {
        let read = scope.spawn(move || {
            lease.revoke();
            assert!(!worker.origin_lease().unwrap().is_current());
            assert!(!worker.is_current(model, now));
        });
        read.join().unwrap();
        assert!(backend_read.envelope(event, now).is_none());
    });
    assert!(!core_read.is_current(job));
    assert!(!core_read.is_event_current(queued));
    drop(core_read);
    assert!(backend.authorize_delivery(job, now).is_none());
    assert!(core
        .borrow_mut()
        .take_ready(channel, now + Duration::from_secs(2))
        .is_none());
    assert_eq!(
        backend.observe_host_event(event, Some(10), Focus::default()),
        Err(Rejection::Unauthorized)
    );
    backend.revoke(&owner(1).identity);
    assert_eq!(
        backend.observe_host_event(event, Some(10), Focus::default()),
        Err(Rejection::Unauthorized)
    );
    let replacement = Owner {
        identity: owner(1).identity,
        generation: "replacement".into(),
    };
    backend.admit(replacement.clone(), false, None).unwrap();
    let at = now + Duration::from_secs(4);
    backend
        .produce(&replacement, input(1, Some(source)), Some(10), at)
        .unwrap();
    let job = core.borrow_mut().take_ready(channel, at).unwrap();
    let authorization = backend.authorize_delivery(job, at).unwrap();
    assert_eq!(authorization.envelope().origin.as_ref(), Some(&replacement));
    core.borrow_mut()
        .set_channel_policy(channel, Policy::DELIVERY_DEFAULT)
        .unwrap();
    assert!(!authorization.is_current(&core.borrow(), at));
    core.borrow_mut().set_channel_policy(channel, on).unwrap();
    assert!(!authorization.is_current(&core.borrow(), at));
    backend
        .produce(
            &replacement,
            input(2, Some(source)),
            Some(10),
            at + Duration::from_secs(2),
        )
        .unwrap();
    let job = core
        .borrow_mut()
        .take_ready(channel, at + Duration::from_secs(2))
        .unwrap();
    let authorization = backend
        .authorize_delivery(job, at + Duration::from_secs(2))
        .unwrap();
    core.borrow_mut().detach(source);
    core.borrow_mut().attach(1).unwrap();
    assert!(!authorization.is_current(&core.borrow(), at + Duration::from_secs(2)));
}

#[test]
fn host_application_authority_has_no_origin_lease_and_expired_snapshots_cannot_run() {
    let core = Rc::new(RefCell::new(Host::new().unwrap()));
    let channel = core
        .borrow_mut()
        .register_channel(Policy {
            enabled: true,
            eligibility: Eligibility::Always,
        })
        .unwrap();
    let mut backend = Backend::new(core.clone(), Focus::default());
    backend.bind("session");
    let now = Instant::now();
    let foreign = Host::new()
        .unwrap()
        .application_event(Focus::default(), now)
        .unwrap();
    assert_eq!(
        backend.observe_host_event(foreign, None, Focus::default()),
        Err(Rejection::Stale)
    );
    let event = core
        .borrow_mut()
        .application_event(Focus::default(), now)
        .unwrap();
    backend
        .observe_host_event(event, None, Focus::default())
        .unwrap();
    let job = core.borrow_mut().take_ready(channel, now).unwrap();
    let authorization = backend.authorize_delivery(job, now).unwrap();
    assert!(authorization.origin_lease().is_none());
    assert_eq!(authorization.envelope().kind, "application");
    assert!(authorization.is_current(&core.borrow(), now));
    assert!(!authorization.is_current(&core.borrow(), now + Duration::from_secs(10)));
    assert!(backend
        .authorize_delivery(job, now + Duration::from_secs(10))
        .is_none());
}

// retain the two independent ca73e6e failure probes, not merely shape assertions.
// their input ordering/limits are unchanged; the original review stays immutable.
mod independent_review_regressions {
    use super::*;

    fn owner(id: u64) -> Owner {
        Owner {
            identity: format!("github:{id}/sample"),
            generation: "review-generation".into(),
        }
    }
    fn key() -> ChannelKey {
        ChannelKey {
            identity: owner(3).identity,
            channel: "main".into(),
        }
    }
    fn event(sequence: u64) -> EventInput {
        EventInput {
            sequence,
            target: Target::Application,
            source_generation: None,
            message: "independent test event".into(),
            causal_job: None,
        }
    }
    fn setup() -> (Rc<RefCell<Host>>, Backend) {
        let core = Rc::new(RefCell::new(Host::new().unwrap()));
        let mut bridge = Backend::new(core.clone(), Focus::default());
        assert!(bridge.bind("review-session"));
        for id in 1..=2 {
            bridge.register_identity(&owner(id).identity).unwrap();
            bridge.admit(owner(id), false, None).unwrap();
        }
        bridge
            .declare(Declaration {
                key: key(),
                name: "Provider".into(),
                description: "Synthetic provider".into(),
            })
            .unwrap();
        bridge.plugin_enabled(&owner(3).identity, true);
        bridge
            .set_policy(
                &key(),
                Policy {
                    enabled: true,
                    eligibility: Eligibility::Always,
                },
            )
            .unwrap();
        bridge.admit(owner(3), true, None).unwrap();
        (core, bridge)
    }

    #[test]
    fn quiet_targetless_producer_is_not_starved_by_later_noisy_production() {
        let (core, mut bridge) = setup();
        let now = Instant::now();
        let mut quiet_received = 0;
        let mut noisy_received = 0;
        let mut noisy_sequence = 1;
        let mut bounded_rejections = 0;
        // Noisy is enqueued first. Quiet then has a legitimate, rate-admitted event;
        // the next noisy event comes before draining each provider turn.
        bridge.produce(&owner(1), event(1), None, now).unwrap();
        for turn in 0..8 {
            let quiet_at = now + Duration::from_millis(turn * 2100 + 5);
            bridge
                .produce(&owner(2), event(turn + 1), None, quiet_at)
                .unwrap();
            for _ in 0..1000 {
                noisy_sequence += 1;
                let attempt = bridge.produce(
                    &owner(1),
                    event(noisy_sequence),
                    None,
                    quiet_at + Duration::from_millis(5),
                );
                assert_eq!(attempt, Err(Rejection::RateLimited));
                bounded_rejections += 1;
            }
            let drain_at = now + Duration::from_millis((turn + 1) * 2100);
            noisy_sequence += 1;
            bridge
                .produce(&owner(1), event(noisy_sequence), None, drain_at)
                .unwrap();
            let job = bridge.take_delivery(&owner(3), drain_at).unwrap();
            match job.event.origin.as_ref().unwrap().identity.as_str() {
                "github:1/sample" => noisy_received += 1,
                "github:2/sample" => quiet_received += 1,
                other => panic!("unexpected origin: {other}"),
            }
            assert!(bridge.result(&owner(3), job.job_id, DeliveryOutcome::Accepted, drain_at));
            assert!(core.borrow().pending_count(None) <= 64 && bridge.event_count() <= 64);
        }
        println!("targetless eight-turn result: noisy={noisy_received}, quiet={quiet_received}, rate-limited noisy flood attempts={bounded_rejections}; both origins admitted every turn, noisy began first");
        assert!(
            quiet_received > 0,
            "a valid quiet targetless producer was permanently overwritten"
        );
    }

    #[test]
    fn active_provider_envelope_is_retained_under_bounded_fresh_source_traffic() {
        let (core, mut bridge) = setup();
        let now = Instant::now();
        let first_source = core.borrow_mut().attach(100).unwrap();
        let first = core
            .borrow_mut()
            .bell(first_source, Focus::default(), now)
            .unwrap();
        bridge
            .observe_host_event(first, Some(10), Focus::default())
            .unwrap();
        let active = bridge.take_delivery(&owner(3), now).unwrap();
        for pane in 101..=164 {
            let source = core.borrow_mut().attach(pane).unwrap();
            let event = core
                .borrow_mut()
                .bell(source, Focus::default(), now)
                .unwrap();
            bridge
                .observe_host_event(event, Some(10), Focus::default())
                .unwrap();
            assert!(bridge.event_count() <= 64 && core.borrow().pending_count(None) <= 64);
        }
        bridge.housekeep(now);
        println!(
            "same-time bounded traffic: active={}, completions={:?}",
            bridge.active_count(),
            bridge.completions()
        );
        assert!(
            bridge.envelope(first, now).is_some(),
            "live active envelope was evicted before its deadline"
        );
        assert!(bridge.result(&owner(3), active.job_id, DeliveryOutcome::Accepted, now));
    }

    #[test]
    fn both_active_ids_are_pinned_inside_the_same_metadata_budget() {
        let (core, mut bridge) = setup();
        let now = Instant::now();
        let first_source = core.borrow_mut().attach(100).unwrap();
        let first = core
            .borrow_mut()
            .bell(first_source, Focus::default(), now)
            .unwrap();
        bridge
            .observe_host_event(first, Some(10), Focus::default())
            .unwrap();
        let active_one = bridge.take_delivery(&owner(3), now).unwrap();
        // retain the first live job while admitting an independent provider.
        let second_owner = owner(4);
        let second_key = ChannelKey {
            identity: second_owner.identity.clone(),
            channel: "other".into(),
        };
        bridge
            .declare(Declaration {
                key: second_key.clone(),
                name: "Other".into(),
                description: "Synthetic".into(),
            })
            .unwrap();
        bridge.plugin_enabled(&second_owner.identity, true);
        bridge
            .set_policy(
                &second_key,
                Policy {
                    enabled: true,
                    eligibility: Eligibility::Always,
                },
            )
            .unwrap();
        bridge.admit(second_owner.clone(), true, None).unwrap();
        let second_source = core.borrow_mut().attach(101).unwrap();
        let second = core
            .borrow_mut()
            .bell(second_source, Focus::default(), now)
            .unwrap();
        bridge
            .observe_host_event(second, Some(10), Focus::default())
            .unwrap();
        let active_two = bridge.take_delivery(&second_owner, now).unwrap();
        assert_ne!(active_one.event.event_id, active_two.event.event_id);
        for pane in 102..=165 {
            let source = core.borrow_mut().attach(pane).unwrap();
            let event = core
                .borrow_mut()
                .bell(source, Focus::default(), now)
                .unwrap();
            bridge
                .observe_host_event(event, Some(10), Focus::default())
                .unwrap();
            assert!(bridge.event_count() <= 64 && core.borrow().pending_count(None) <= 64);
        }
        bridge.housekeep(now);
        assert_eq!(bridge.event_count(), 64);
        assert_eq!(bridge.active_count(), 2);
        assert!(bridge.envelope(first, now).is_some());
        assert!(bridge.envelope(second, now).is_some());
        assert!(bridge.completions().is_empty());
        assert!(bridge.result(&owner(3), active_one.job_id, DeliveryOutcome::Accepted, now));
        assert!(bridge.result(
            &second_owner,
            active_two.job_id,
            DeliveryOutcome::Accepted,
            now
        ));
    }

    #[test]
    fn delivery_authority_excludes_production_before_and_after_non_enqueued_or_completed_work() {
        for enqueued in [true, false] {
            let (core, mut bridge) = setup();
            let now = Instant::now();
            assert_eq!(
                bridge.produce(&owner(3), event(1), None, now),
                Err(Rejection::Unauthorized)
            );
            let first = core
                .borrow_mut()
                .application_event(Focus::default(), now)
                .unwrap();
            bridge
                .observe_host_event(first, None, Focus::default())
                .unwrap();
            let job = bridge.take_delivery(&owner(3), now).unwrap();
            if enqueued {
                assert!(bridge.result(&owner(3), job.job_id, DeliveryOutcome::Accepted, now));
            } else {
                bridge.not_enqueued(&owner(3), job.job_id);
            }
            assert_eq!(
                bridge.produce(&owner(3), event(2), None, now + Duration::from_secs(3)),
                Err(Rejection::Unauthorized)
            );
            // the independent producer's authority is not tainted by assignment
            // or no-enqueue; explicit role separation replaces intent guessing.
            assert!(bridge
                .produce(&owner(1), event(1), None, now + Duration::from_secs(3))
                .is_ok());
        }
    }
}
