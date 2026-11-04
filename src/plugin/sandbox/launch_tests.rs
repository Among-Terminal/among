//! the start protocol on real forked children: every way a confinement step
//! can fail or go missing must end with no running plugin process.

use super::hostile_tests::{fixture_executable, full_protection_available, Scratch};
use super::launch::{Confinement, Inject, Spec, StartError};
use super::level::{self, Protection};
use super::{data_dir, status::Step};
use crate::plugin::process::confined_program;
use std::collections::BTreeSet;
use std::fs::File;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Prepared {
    confinement: Confinement,
    command: Command,
}

fn prepare(
    scratch: &Scratch,
    required: Option<Protection>,
    adjust: impl FnOnce(Confinement) -> Confinement,
) -> Prepared {
    let probe = level::probe();
    let generation = File::open(&scratch.generation).unwrap();
    let executable = File::open(scratch.generation.join("plugin")).unwrap();
    let data = data_dir::open(&scratch.data_home, "12", "hostile").unwrap();
    let features = BTreeSet::new();
    let spec = Spec {
        generation: &generation,
        data_dir: &data.directory,
        features: &features,
        probe,
        required: required.unwrap_or_else(|| probe.best()),
    };
    let mut confinement = adjust(Confinement::prepare(&spec).unwrap());
    let mut command = confined_program(generation, executable, None, &mut confinement);
    command
        .env_clear()
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    Prepared {
        confinement,
        command,
    }
}

fn reap(mut child: Child) -> std::process::ExitStatus {
    let _ = child.kill();
    child.wait().unwrap()
}

fn failure(error: StartError) -> (String, bool) {
    match error {
        StartError::Failed {
            reason,
            confinement_failed,
            ..
        } => (reason, confinement_failed),
        StartError::Unproven { reason, child } => {
            reap(child);
            (reason, true)
        }
    }
}

#[test]
fn a_successful_start_reports_the_level_the_child_applied() {
    if !full_protection_available() {
        return;
    }
    let scratch = Scratch::new("success");
    let mut prepared = prepare(&scratch, None, |c| c);
    let (child, applied) = prepared
        .confinement
        .spawn(&mut prepared.command)
        .ok()
        .unwrap();
    assert!(applied.protection.is_full());
    assert!(applied.report.seccomp_loaded && applied.report.no_new_privs);
    assert_eq!(applied.report.failed, Step::None);
    assert_eq!(applied.digest.len(), 64);
    reap(child);
}

#[test]
fn the_same_confinement_starts_twice_with_a_fresh_report_each_time() {
    // a retry after ETXTBSY forks again with the same prepared data.
    if !full_protection_available() {
        return;
    }
    let scratch = Scratch::new("twice");
    let mut prepared = prepare(&scratch, None, |c| c);
    for _ in 0..2 {
        let (child, applied) = prepared
            .confinement
            .spawn(&mut prepared.command)
            .ok()
            .unwrap();
        assert!(applied.protection.is_full());
        reap(child);
    }
}

#[test]
fn every_failing_confinement_step_refuses_the_start_and_leaves_no_process() {
    if !full_protection_available() {
        return;
    }
    let scratch = Scratch::new("steps");
    for (step, description) in [
        (Step::Inheritance, "closing inherited descriptors failed"),
        (Step::NoNewPrivs, "setting no-new-privileges failed"),
        (
            Step::Landlock,
            "applying the Landlock filesystem rules failed",
        ),
        (Step::Seccomp, "loading the seccomp filter failed"),
    ] {
        let mut prepared = prepare(&scratch, None, |c| {
            c.with_inject(Inject {
                fail_step: Some(step),
                ..Inject::default()
            })
        });
        let error = match prepared.confinement.spawn(&mut prepared.command) {
            Ok((child, _)) => {
                reap(child);
                panic!("a start with a failing {step:?} step succeeded");
            }
            Err(error) => error,
        };
        // the child never reached exec, so no Child exists to leak.
        let StartError::Failed {
            reason,
            confinement_failed,
            ..
        } = error
        else {
            panic!("{step:?}: the failure left a process behind");
        };
        assert!(confinement_failed, "{step:?}");
        assert!(reason.starts_with(description), "{step:?}: {reason}");
        // the fixture never ran: it would have created nothing, but the proof
        // of absence is that no process of this executable has the data
        // directory open.
    }
}

#[test]
fn a_child_stuck_before_exec_is_killed_within_the_bounded_wait() {
    if !full_protection_available() {
        return;
    }
    let scratch = Scratch::new("hang");
    let mut prepared = prepare(&scratch, None, |c| {
        c.with_timeout(Duration::from_millis(400))
            .with_inject(Inject {
                hang: true,
                ..Inject::default()
            })
    });
    let started = Instant::now();
    let outcome = prepared.confinement.spawn(&mut prepared.command);
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "the wait was not bounded: {elapsed:?}"
    );
    match outcome {
        // the standard library reads end-of-stream from the killed child as a
        // completed exec, so the dead child comes back and is refused.
        Err(StartError::Unproven { child, reason }) => {
            let status = reap(child);
            assert_eq!(
                status.signal(),
                Some(libc::SIGKILL),
                "the child was killed: {reason}"
            );
        }
        Err(StartError::Failed {
            confinement_failed, ..
        }) => assert!(confinement_failed),
        Ok(_) => panic!("a child that never reached exec was accepted"),
    }
}

#[test]
fn exec_without_a_final_record_is_refused_and_the_process_is_returned_to_be_killed() {
    if !full_protection_available() {
        return;
    }
    let scratch = Scratch::new("silent");
    let mut prepared = prepare(&scratch, None, |c| {
        c.with_inject(Inject {
            silent: true,
            ..Inject::default()
        })
    });
    match prepared.confinement.spawn(&mut prepared.command) {
        Err(StartError::Unproven { child, reason }) => {
            assert!(reason.contains("incomplete"), "{reason}");
            // exactly this process ran the plugin without proof; it exists and
            // the caller kills it.
            let mut child = child;
            assert!(
                child.try_wait().unwrap().is_none(),
                "the exec'd process runs"
            );
            assert_eq!(reap(child).signal(), Some(libc::SIGKILL));
        }
        other => panic!("expected an unproven start, got {:?}", other.is_ok()),
    }
}

#[test]
fn a_short_record_is_refused() {
    if !full_protection_available() {
        return;
    }
    let scratch = Scratch::new("short");
    let mut prepared = prepare(&scratch, None, |c| {
        c.with_inject(Inject {
            short: true,
            ..Inject::default()
        })
    });
    match prepared.confinement.spawn(&mut prepared.command) {
        Err(StartError::Unproven { child, reason }) => {
            assert!(reason.contains("incomplete"), "{reason}");
            reap(child);
        }
        other => panic!("expected an unproven start, got {:?}", other.is_ok()),
    }
}

#[test]
fn a_complete_record_without_exec_is_a_failed_start_without_a_process() {
    if !full_protection_available() {
        return;
    }
    let scratch = Scratch::new("no-exec");
    let mut prepared = prepare(&scratch, None, |c| {
        c.with_inject(Inject {
            report_then_fail: true,
            ..Inject::default()
        })
    });
    match prepared.confinement.spawn(&mut prepared.command) {
        Err(StartError::Failed {
            confinement_failed,
            errno,
            ..
        }) => {
            // the report was clean, so this is an exec failure the caller may
            // classify as such; no process exists either way.
            assert!(!confinement_failed);
            assert_eq!(errno, Some(libc::EACCES));
        }
        _ => panic!("expected a failed start"),
    }
}

#[test]
fn a_child_that_applied_less_than_required_is_refused_and_returned_to_be_killed() {
    if !full_protection_available() {
        return;
    }
    let scratch = Scratch::new("below");
    let mut prepared = prepare(&scratch, Some(Protection::Full { abi: 99 }), |c| c);
    match prepared.confinement.spawn(&mut prepared.command) {
        Err(StartError::Unproven { child, reason }) => {
            assert!(reason.contains("required protection level"), "{reason}");
            reap(child);
        }
        _ => panic!("a below-requirement start was accepted"),
    }
}

#[test]
fn nothing_allocates_between_fork_and_exec_on_the_success_path() {
    // a global allocator aborts the forked child on any allocation; a
    // completed start therefore proves the hook, the Landlock restriction and
    // the seccomp load are allocation-free.
    if !full_protection_available() {
        return;
    }
    let scratch = Scratch::new("allocation");
    let mut prepared = prepare(&scratch, None, |c| {
        c.with_inject(Inject {
            forbid_allocation: true,
            ..Inject::default()
        })
    });
    let (child, applied) = prepared
        .confinement
        .spawn(&mut prepared.command)
        .ok()
        .unwrap();
    assert!(applied.protection.is_full());
    reap(child);
}

#[test]
fn the_allocation_guard_really_aborts_a_child_that_allocates() {
    // control for the test above: the guard must fire when the hook allocates.
    let scratch = Scratch::new("guard-control");
    let _ = fixture_executable();
    let mut command = Command::new(scratch.generation.join("plugin"));
    unsafe {
        command.pre_exec(|| {
            super::launch::allocation_guard::forbid();
            let allocation = Box::new(7u64);
            std::hint::black_box(allocation);
            Ok(())
        });
    }
    match command.spawn() {
        // the child exited 101 before exec; the standard library sees the pipe
        // close without an error report and returns the child.
        Ok(mut child) => {
            let status = child.wait().unwrap();
            assert_eq!(status.code(), Some(101), "the allocation guard fired");
        }
        Err(error) => panic!("unexpected spawn error {error}"),
    }
}
