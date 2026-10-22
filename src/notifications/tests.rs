use super::*;

fn enabled(eligibility: Eligibility) -> Policy {
    Policy {
        enabled: true,
        eligibility,
    }
}

#[test]
fn policies_cover_every_focus_target_and_enablement_combination() {
    let mut host = Host::new().unwrap();
    let source = host.attach(1).unwrap();
    let sibling = host.attach(2).unwrap();
    for active in [false, true] {
        for viewed in [None, Some(source), Some(sibling)] {
            for target in [None, Some(source)] {
                for eligibility in [
                    Eligibility::Always,
                    Eligibility::OriginatingTerminalUnfocused,
                    Eligibility::ApplicationBackground,
                ] {
                    for on in [false, true] {
                        let expected = on
                            && match eligibility {
                                Eligibility::Always => true,
                                Eligibility::OriginatingTerminalUnfocused => {
                                    target.is_some() && !(active && viewed == target)
                                }
                                Eligibility::ApplicationBackground => !active,
                            };
                        assert_eq!(
                            Policy {
                                enabled: on,
                                eligibility,
                            }
                            .permits(
                                target,
                                Focus {
                                    application_active: active,
                                    viewed,
                                }
                            ),
                            expected,
                            "on={on} policy={eligibility:?} active={active} viewed={viewed:?} target={target:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn default_bell_marks_only_its_source_and_never_queues_disabled_delivery() {
    let mut host = Host::new().unwrap();
    let source = host.attach(1).unwrap();
    let sibling = host.attach(2).unwrap();
    let audio = host.register_channel(Policy::DELIVERY_DEFAULT).unwrap();
    let command = host.register_channel(Policy::DELIVERY_DEFAULT).unwrap();
    let now = Instant::now();
    let focus = Focus {
        application_active: true,
        viewed: Some(sibling),
    };
    let first = host.bell(source, focus, now).unwrap();
    assert_eq!(first.kind(), Kind::TerminalBell);
    assert_eq!(first.source(), Some(source));
    assert_eq!(first.received_at(), now);
    assert_eq!(first.session(), source.session());
    assert_eq!(first.id(), 1);
    assert_eq!(host.event_count(), 1);
    assert_eq!(host.visual_policy(), Policy::VISUAL_DEFAULT);
    assert_eq!(
        host.attention(source, now),
        Attention {
            unread: true,
            pulsing: true,
        }
    );
    assert_eq!(host.attention(sibling, now), Attention::default());
    assert_eq!(host.aggregate([1, 2], now).unread_panes, 1);
    assert!(host.take_ready(audio, now).is_none());
    assert!(host.take_ready(command, now).is_none());
    assert_eq!(host.pending_count(None), 0);
}

#[test]
fn pulse_is_one_second_cooldown_is_two_seconds_and_flood_does_not_extend_them() {
    let mut host = Host::new().unwrap();
    let source = host.attach(1).unwrap();
    let now = Instant::now();
    host.bell(source, Focus::default(), now).unwrap();
    for millis in 1..2_000 {
        let at = now + Duration::from_millis(millis);
        host.bell(source, Focus::default(), at).unwrap();
        assert_eq!(host.attention(source, at).pulsing, millis < 1_000);
    }
    assert!(host.attention(source, now + PULSE).unread);
    assert!(!host.attention(source, now + COOLDOWN).pulsing);
    host.bell(source, Focus::default(), now + COOLDOWN).unwrap();
    assert!(host.attention(source, now + COOLDOWN).pulsing);
    assert!(!host.attention(source, now + COOLDOWN + PULSE).pulsing);
}

#[test]
fn actual_view_acknowledges_one_sibling_but_selection_and_preferences_do_not() {
    let mut host = Host::new().unwrap();
    let one = host.attach(1).unwrap();
    let two = host.attach(2).unwrap();
    let now = Instant::now();
    host.bell(one, Focus::default(), now).unwrap();
    host.bell(two, Focus::default(), now).unwrap();
    for focus in [
        Focus::default(), // remembered selection is not a focus observation
        Focus {
            application_active: true, // preferences, menu, header control
            viewed: None,
        },
        Focus {
            application_active: false,
            viewed: Some(one), // remembered main-window focus in the background
        },
    ] {
        host.acknowledge(focus);
        assert_eq!(host.aggregate([1, 2], now).unread_panes, 2);
    }
    host.acknowledge(Focus {
        application_active: true,
        viewed: Some(one),
    });
    assert!(!host.attention(one, now).unread);
    assert!(host.attention(two, now).unread);
    assert_eq!(host.aggregate([1, 2], now).unread_panes, 1);
    assert_eq!(host.aggregate([1], now).unread_panes, 0);
}

#[test]
fn always_can_pulse_and_deliver_in_a_viewed_terminal_without_unread() {
    let mut host = Host::new().unwrap();
    let source = host.attach(1).unwrap();
    host.set_visual_policy(enabled(Eligibility::Always));
    let channel = host.register_channel(enabled(Eligibility::Always)).unwrap();
    let focus = Focus {
        application_active: true,
        viewed: Some(source),
    };
    let now = Instant::now();
    let event = host.bell(source, focus, now).unwrap();
    assert_eq!(
        host.attention(source, now),
        Attention {
            unread: false,
            pulsing: true,
        }
    );
    assert_eq!(host.take_ready(channel, now).unwrap().event(), event);
}

#[test]
fn visual_audio_command_and_provider_policies_fan_out_independently() {
    let mut host = Host::new().unwrap();
    let source = host.attach(1).unwrap();
    host.set_visual_policy(enabled(Eligibility::Always));
    let audio = host
        .register_channel(enabled(Eligibility::ApplicationBackground))
        .unwrap();
    let command = host
        .register_channel(enabled(Eligibility::OriginatingTerminalUnfocused))
        .unwrap();
    let provider = host.register_channel(enabled(Eligibility::Always)).unwrap();
    let now = Instant::now();
    host.bell(
        source,
        Focus {
            application_active: true,
            viewed: None,
        },
        now,
    )
    .unwrap();
    assert!(host.attention(source, now).unread);
    assert!(host.take_ready(audio, now).is_none());
    let command_job = host.take_ready(command, now).unwrap();
    host.remove_channel(command); // failed/unavailable adapter cannot affect siblings
    assert!(!host.is_current(command_job));
    assert!(host.take_ready(provider, now).is_some());
    assert!(host.attention(source, now).unread);
}

#[test]
fn targetless_events_never_invent_attention_or_match_terminal_unfocused() {
    for active in [false, true] {
        let mut host = Host::new().unwrap();
        host.attach(1).unwrap();
        host.set_visual_policy(enabled(Eligibility::Always));
        let always = host.register_channel(enabled(Eligibility::Always)).unwrap();
        let background = host
            .register_channel(enabled(Eligibility::ApplicationBackground))
            .unwrap();
        let unfocused = host
            .register_channel(enabled(Eligibility::OriginatingTerminalUnfocused))
            .unwrap();
        let now = Instant::now();
        let event = host
            .application_event(
                Focus {
                    application_active: active,
                    viewed: None,
                },
                now,
            )
            .unwrap();
        assert_eq!(event.source(), None);
        assert_eq!(event.kind(), Kind::Application);
        assert_eq!(host.aggregate([1], now), Aggregate::default());
        assert_eq!(host.take_ready(always, now).unwrap().event(), event);
        assert_eq!(host.take_ready(background, now).is_some(), !active);
        assert!(host.take_ready(unfocused, now).is_none());
    }
}

#[test]
fn focus_is_snapshotted_at_ingress_and_suppressed_events_are_not_replayed() {
    let mut host = Host::new().unwrap();
    let source = host.attach(1).unwrap();
    let channel = host
        .register_channel(enabled(Eligibility::ApplicationBackground))
        .unwrap();
    let now = Instant::now();
    host.bell(source, Focus::default(), now).unwrap();
    host.acknowledge(Focus {
        application_active: true,
        viewed: Some(source),
    });
    assert!(host.take_ready(channel, now).is_some());
    host.bell(
        source,
        Focus {
            application_active: true,
            viewed: None,
        },
        now + COOLDOWN,
    )
    .unwrap();
    assert!(host.take_ready(channel, now + COOLDOWN * 2).is_none());
}

#[test]
fn removal_reuse_and_fresh_sessions_reject_stale_sources_and_jobs() {
    let mut host = Host::new().unwrap();
    let old = host.attach(1).unwrap();
    let sibling = host.attach(2).unwrap();
    let channel = host.register_channel(enabled(Eligibility::Always)).unwrap();
    let now = Instant::now();
    host.bell(old, Focus::default(), now).unwrap();
    host.bell(sibling, Focus::default(), now).unwrap();
    let in_flight = host.take_ready(channel, now).unwrap();
    host.bell(old, Focus::default(), now).unwrap();
    host.detach(old);
    assert!(!host.is_current(in_flight));
    assert_eq!(host.pending_count(None), 1);
    assert_eq!(host.aggregate([1, 2], now).unread_panes, 1);
    let replacement = host.attach(1).unwrap();
    assert_ne!(replacement.generation(), old.generation());
    host.detach(old); // late close cannot remove its replacement
    assert!(host.is_live(replacement));
    assert_eq!(
        host.bell(old, Focus::default(), now),
        Err(Failure::StaleSource)
    );
    assert_eq!(host.attention(replacement, now), Attention::default());
    assert_eq!(host.attention(old, now), Attention::default());
    assert_eq!(
        host.take_ready(channel, now).unwrap().event().source(),
        Some(sibling)
    );

    let mut fresh = Host::new().unwrap();
    let restored = fresh.attach(1).unwrap();
    assert_ne!(old.session(), restored.session());
    assert_eq!(fresh.aggregate([1], now), Aggregate::default());
    assert!(!fresh.is_current(in_flight));
    assert_eq!(fresh.pending_count(None), 0);
    assert_eq!(
        fresh.bell(old, Focus::default(), now),
        Err(Failure::StaleSource)
    );
}

#[test]
fn channel_disable_revokes_queued_and_in_flight_work_without_replay() {
    let mut host = Host::new().unwrap();
    let source = host.attach(1).unwrap();
    let on = enabled(Eligibility::Always);
    let channel = host.register_channel(on).unwrap();
    let now = Instant::now();
    host.bell(source, Focus::default(), now).unwrap();
    let in_flight = host.take_ready(channel, now).unwrap();
    assert_eq!(in_flight.channel(), channel);
    assert!(host.is_current(in_flight));
    host.bell(source, Focus::default(), now).unwrap();
    host.set_channel_policy(channel, Policy::DELIVERY_DEFAULT)
        .unwrap();
    assert!(!host.is_current(in_flight));
    assert_eq!(host.pending_count(None), 0);
    host.set_channel_policy(channel, on).unwrap();
    assert!(!host.is_current(in_flight));
    assert!(host.take_ready(channel, now + COOLDOWN).is_none());
    host.bell(source, Focus::default(), now + COOLDOWN).unwrap();
    assert!(host.take_ready(channel, now + COOLDOWN).is_some());
}

#[test]
fn coalescing_and_cooldown_cannot_starve_another_source() {
    let mut host = Host::new().unwrap();
    let noisy = host.attach(1).unwrap();
    let quiet = host.attach(2).unwrap();
    let channel = host.register_channel(enabled(Eligibility::Always)).unwrap();
    let now = Instant::now();
    host.bell(noisy, Focus::default(), now).unwrap();
    host.take_ready(channel, now).unwrap();
    for _ in 0..100_000 {
        host.bell(noisy, Focus::default(), now + PULSE).unwrap();
    }
    assert_eq!(host.pending_count(None), 1);
    host.bell(quiet, Focus::default(), now + PULSE).unwrap();
    assert_eq!(
        host.take_ready(channel, now + PULSE)
            .unwrap()
            .event()
            .source(),
        Some(quiet)
    );
    assert!(host.take_ready(channel, now + PULSE).is_none());
    assert_eq!(
        host.take_ready(channel, now + COOLDOWN)
            .unwrap()
            .event()
            .source(),
        Some(noisy)
    );
    assert_eq!(host.pending_count(None), 0);
    assert!(host.diagnostics().len() <= MAX_DIAGNOSTICS);
    assert_eq!(host.diagnostics().back().unwrap().occurrences, 99_999);
}

#[test]
fn queues_and_diagnostics_remain_bounded_under_many_sources_and_channels() {
    let mut host = Host::new().unwrap();
    let channels: Vec<_> = (0..6)
        .map(|_| host.register_channel(enabled(Eligibility::Always)).unwrap())
        .collect();
    let sources: Vec<_> = (0..100).map(|pane| host.attach(pane).unwrap()).collect();
    let now = Instant::now();
    for _ in 0..4 {
        for &source in &sources {
            host.bell(source, Focus::default(), now).unwrap();
        }
    }
    assert_eq!(host.pending_count(None), MAX_PENDING);
    for &channel in &channels {
        assert!(host.pending_count(Some(channel)) <= MAX_PENDING_PER_CHANNEL);
    }
    assert_eq!(host.aggregate(0..100, now).unread_panes, 100);
    assert!(host.diagnostics().len() <= MAX_DIAGNOSTICS);
    assert!(host
        .diagnostics()
        .iter()
        .any(|d| d.kind == DiagnosticKind::GlobalQueueFull));
    let stale = host.attach(u64::MAX).unwrap();
    host.detach(stale);
    for _ in 0..64 {
        assert_eq!(
            host.bell(stale, Focus::default(), now),
            Err(Failure::StaleSource)
        );
        host.bell(sources[0], Focus::default(), now).unwrap();
    }
    assert_eq!(host.diagnostics().len(), MAX_DIAGNOSTICS);
    for source in sources {
        host.detach(source);
    }
    assert_eq!(host.pending_count(None), 0);
    assert_eq!(host.aggregate(0..100, now), Aggregate::default());

    let mut host = Host::new().unwrap();
    let channel = host.register_channel(enabled(Eligibility::Always)).unwrap();
    for pane in 0..20 {
        let source = host.attach(pane).unwrap();
        host.bell(source, Focus::default(), now).unwrap();
    }
    assert_eq!(host.pending_count(Some(channel)), MAX_PENDING_PER_CHANNEL);
    assert_eq!(
        host.diagnostics().back().unwrap().kind,
        DiagnosticKind::ChannelQueueFull
    );
}

#[test]
fn a_single_source_fanout_cannot_monopolize_the_global_budget() {
    let mut host = Host::new().unwrap();
    let channels: Vec<_> = (0..MAX_PENDING + 1)
        .map(|_| host.register_channel(enabled(Eligibility::Always)).unwrap())
        .collect();
    let noisy = host.attach(1).unwrap();
    let quiet = host.attach(2).unwrap();
    let now = Instant::now();
    host.bell(noisy, Focus::default(), now).unwrap();
    assert_eq!(host.pending_count(None), MAX_PENDING);
    host.bell(quiet, Focus::default(), now).unwrap();
    assert_eq!(host.pending_count(None), MAX_PENDING);
    let mut jobs = Vec::new();
    for channel in channels {
        while let Some(job) = host.take_ready(channel, now) {
            jobs.push(job);
        }
    }
    assert_eq!(jobs.len(), MAX_PENDING);
    assert_eq!(
        jobs.iter()
            .filter(|job| job.event().source() == Some(quiet))
            .count(),
        MAX_PENDING / 2
    );
    assert_eq!(
        jobs.iter()
            .filter(|job| job.event().source() == Some(noisy))
            .count(),
        MAX_PENDING / 2
    );
}

#[test]
fn disabling_visual_clears_emphasis_and_reenable_does_not_replay() {
    let mut host = Host::new().unwrap();
    let source = host.attach(1).unwrap();
    let now = Instant::now();
    host.bell(source, Focus::default(), now).unwrap();
    host.set_visual_policy(Policy {
        enabled: false,
        ..Policy::VISUAL_DEFAULT
    });
    assert_eq!(host.attention(source, now), Attention::default());
    host.set_visual_policy(Policy::VISUAL_DEFAULT);
    assert_eq!(host.attention(source, now + COOLDOWN), Attention::default());
}

#[test]
fn counter_exhaustion_fails_closed_instead_of_reusing_identity() {
    let mut host = Host::new().unwrap();
    let source = host.attach(1).unwrap();
    host.next_source = u64::MAX;
    assert_eq!(host.attach(1), Err(Failure::Exhausted));
    assert!(host.is_live(source));
    host.next_channel = u64::MAX;
    assert_eq!(
        host.register_channel(Policy::DELIVERY_DEFAULT),
        Err(Failure::Exhausted)
    );
    host.last_event = u64::MAX;
    assert_eq!(
        host.bell(source, Focus::default(), Instant::now()),
        Err(Failure::Exhausted)
    );
}

#[test]
fn worker_authorization_revokes_independently_of_the_borrowed_host() {
    let now = Instant::now();
    for operation in 0..4 {
        let mut host = Host::new().unwrap();
        let source = host.attach(1).unwrap();
        let channel = host.register_channel(enabled(Eligibility::Always)).unwrap();
        host.bell(source, Focus::default(), now).unwrap();
        let job = host.take_ready(channel, now).unwrap();
        let authorization = host.authorization(job).unwrap();
        assert!(authorization.is_current());
        match operation {
            0 => host.detach(source),
            1 => host.cancel_channel_work(channel).unwrap(),
            2 => host.remove_channel(channel),
            _ => drop(host),
        }
        assert!(!authorization.is_current());
    }
}

#[test]
fn managed_kind_origin_and_focus_remain_independent_for_targeted_and_targetless_events() {
    for active in [false, true] {
        for viewed in [0, 1, 2] {
            for targeted in [false, true] {
                let mut host = Host::new().unwrap();
                let source = host.attach(1).unwrap();
                let sibling = host.attach(2).unwrap();
                let (origin, _) = host
                    .register_origin("github:12/sample", "generation-1")
                    .unwrap();
                let focus = Focus {
                    application_active: active,
                    viewed: match viewed {
                        1 => Some(source),
                        2 => Some(sibling),
                        _ => None,
                    },
                };
                let channels = [
                    Eligibility::Always,
                    Eligibility::OriginatingTerminalUnfocused,
                    Eligibility::ApplicationBackground,
                ]
                .map(|eligibility| host.register_channel(enabled(eligibility)).unwrap());
                let target = targeted.then_some(source);
                let now = Instant::now();
                let event = host.managed_event(origin, target, focus, now).unwrap();
                assert_eq!(event.kind(), Kind::ManagedNotification);
                assert_eq!(event.origin(), Some(origin));
                assert_eq!(event.source(), target);
                assert_eq!(
                    host.origin(origin).unwrap(),
                    &OriginIdentity {
                        identity: "github:12/sample".into(),
                        generation: "generation-1".into(),
                    }
                );
                assert_eq!(
                    host.attention(source, now).unread,
                    targeted && !(active && viewed == 1)
                );
                assert!(!host.attention(sibling, now).unread);
                for (channel, expected) in
                    channels
                        .into_iter()
                        .zip([true, targeted && !(active && viewed == 1), !active])
                {
                    assert_eq!(
                        host.take_ready(channel, now).map(|job| job.event()),
                        expected.then_some(event)
                    );
                }
            }
        }
    }
}

#[test]
fn later_quiet_targetless_origin_bypasses_noisy_coalescing_and_cooldown() {
    let mut host = Host::new().unwrap();
    let (noisy, _) = host.register_origin("noisy", "one").unwrap();
    let channel = host.register_channel(enabled(Eligibility::Always)).unwrap();
    let now = Instant::now();
    host.managed_event(noisy, None, Focus::default(), now)
        .unwrap();
    host.take_ready(channel, now).unwrap();
    for _ in 0..100_000 {
        host.managed_event(noisy, None, Focus::default(), now + PULSE)
            .unwrap();
    }
    assert_eq!(host.pending_count(None), 1);
    let (quiet, _) = host.register_origin("quiet", "one").unwrap();
    let event = host
        .managed_event(quiet, None, Focus::default(), now + PULSE)
        .unwrap();
    host.managed_event(noisy, None, Focus::default(), now + PULSE)
        .unwrap();
    assert_eq!(host.pending_count(None), 2);
    assert_eq!(
        host.take_ready(channel, now + PULSE).unwrap().event(),
        event
    );
    let application = host
        .application_event(Focus::default(), now + PULSE)
        .unwrap();
    assert_eq!(application.origin(), None);
    assert_eq!(application.kind(), Kind::Application);
    assert_eq!(
        host.take_ready(channel, now + PULSE).unwrap().event(),
        application
    );
    assert!(host.take_ready(channel, now + PULSE).is_none());
    assert_eq!(
        host.take_ready(channel, now + COOLDOWN)
            .unwrap()
            .event()
            .origin(),
        Some(noisy)
    );
}

#[test]
fn targetless_origin_fanout_uses_the_same_bounded_fair_global_queue() {
    let mut host = Host::new().unwrap();
    let channels: Vec<_> = (0..=MAX_PENDING)
        .map(|_| host.register_channel(enabled(Eligibility::Always)).unwrap())
        .collect();
    let (noisy, _) = host.register_origin("noisy", "one").unwrap();
    let now = Instant::now();
    for _ in 0..100 {
        host.managed_event(noisy, None, Focus::default(), now)
            .unwrap();
    }
    let (quiet, _) = host.register_origin("quiet", "one").unwrap();
    host.managed_event(quiet, None, Focus::default(), now)
        .unwrap();
    assert_eq!(host.pending_count(None), MAX_PENDING);
    let mut quiet_jobs = 0;
    let mut all_jobs = 0;
    for channel in channels {
        assert!(host.pending_count(Some(channel)) <= MAX_PENDING_PER_CHANNEL);
        while let Some(job) = host.take_ready(channel, now) {
            all_jobs += 1;
            quiet_jobs += usize::from(job.event().origin() == Some(quiet));
        }
    }
    assert_eq!(all_jobs, MAX_PENDING);
    assert_eq!(quiet_jobs, MAX_PENDING / 2);
    assert!(host.diagnostics().len() <= MAX_DIAGNOSTICS);
}

#[test]
fn shared_origin_lease_revokes_core_jobs_before_mutable_cleanup_and_cannot_revive() {
    let mut host = Host::new().unwrap();
    let source = host.attach(1).unwrap();
    let channel = host.register_channel(enabled(Eligibility::Always)).unwrap();
    let (origin, lease) = host.register_origin("producer", "old").unwrap();
    let now = Instant::now();
    host.managed_event(origin, Some(source), Focus::default(), now)
        .unwrap();
    let active = host.take_ready(channel, now).unwrap();
    let queued = host
        .managed_event(origin, None, Focus::default(), now)
        .unwrap();
    assert!(host.is_current(active));
    std::thread::spawn(move || lease.revoke()).join().unwrap();
    assert_eq!(host.pending_count(None), 1); // no mutable cleanup has run
    assert!(!host.is_current(active));
    assert!(!host.is_event_current(queued));
    assert!(host.origin(origin).is_none());
    assert!(host.take_ready(channel, now).is_none());
    assert!(host.origins.is_empty());
    let (replacement, live) = host.register_origin("producer", "new").unwrap();
    assert_ne!(replacement, origin);
    assert_eq!(
        host.managed_event(origin, None, Focus::default(), now),
        Err(Failure::StaleOrigin)
    );
    let mut foreign = Host::new().unwrap();
    assert_eq!(
        foreign.managed_event(replacement, None, Focus::default(), now),
        Err(Failure::StaleOrigin)
    );
    host.managed_event(replacement, Some(source), Focus::default(), now + COOLDOWN)
        .unwrap();
    let active = host.take_ready(channel, now + COOLDOWN).unwrap();
    host.detach(source);
    assert!(!host.is_current(active));
    assert!(live.is_current()); // source revocation is not producer revocation
    host.managed_event(replacement, None, Focus::default(), now + COOLDOWN)
        .unwrap();
    let active = host.take_ready(channel, now + COOLDOWN).unwrap();
    host.set_channel_policy(channel, Policy::DELIVERY_DEFAULT)
        .unwrap();
    assert!(!host.is_current(active));
    host.set_channel_policy(channel, enabled(Eligibility::Always))
        .unwrap();
    assert!(!host.is_current(active));
    host.retire_origin(replacement);
    assert!(!live.is_current());
}

#[test]
fn origin_registry_is_bounded_reclaims_revoked_metadata_and_fails_closed() {
    let mut host = Host::new().unwrap();
    assert!(matches!(
        host.register_origin("", "generation"),
        Err(Failure::InvalidOrigin)
    ));
    assert!(matches!(
        host.register_origin("identity", &"x".repeat(129)),
        Err(Failure::InvalidOrigin)
    ));
    assert!(matches!(
        host.register_origin("identity\n", "generation"),
        Err(Failure::InvalidOrigin)
    ));
    let registrations: Vec<_> = (0..MAX_ORIGINS)
        .map(|id| {
            host.register_origin(&format!("origin-{id}"), "generation")
                .unwrap()
        })
        .collect();
    let channel = host.register_channel(enabled(Eligibility::Always)).unwrap();
    let now = Instant::now();
    for (origin, _) in &registrations {
        host.managed_event(*origin, None, Focus::default(), now)
            .unwrap();
        assert!(host.pending_count(None) <= MAX_PENDING);
        assert!(host.pending_count(Some(channel)) <= MAX_PENDING_PER_CHANNEL);
    }
    assert_eq!(host.pending_count(Some(channel)), MAX_PENDING_PER_CHANNEL);
    assert_eq!(
        host.diagnostics().back().unwrap().kind,
        DiagnosticKind::ChannelQueueFull
    );
    assert!(matches!(
        host.register_origin("over", "generation"),
        Err(Failure::Exhausted)
    ));
    registrations[0].1.revoke();
    let (_, lease) = host.register_origin("replacement", "generation").unwrap();
    assert_eq!(host.origins.len(), MAX_ORIGINS);
    assert!(host.origin(registrations[0].0).is_none());
    host.next_origin = u64::MAX;
    registrations[1].1.revoke();
    assert!(matches!(
        host.register_origin("exhausted", "generation"),
        Err(Failure::Exhausted)
    ));
    drop(host);
    assert!(!lease.is_current());
    assert!(registrations.iter().all(|(_, lease)| !lease.is_current()));
}
