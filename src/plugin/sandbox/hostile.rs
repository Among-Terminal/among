//! every hostile action of the fixture against the real kernel, each with its
//! own positive control. see `hostile_tests` for the harness.

use super::hostile_tests::*;
use std::collections::BTreeSet;
use std::io::Read;
use std::net::{TcpListener, UdpSocket};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::Path;

#[derive(Clone, Copy, Debug)]
enum Expect {
    Ok,
    Errno(&'static str),
    DiedBySignal,
}

impl Expect {
    fn holds(self, outcome: &Outcome) -> bool {
        match self {
            Self::Ok => outcome.succeeded(),
            Self::Errno(errno) => outcome.failed_with(errno),
            Self::DiedBySignal => matches!(outcome, Outcome::Died(Some(_))),
        }
    }
}

/// How binding a row is on this machine.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Need {
    /// A failed control fails the test: the environment must support it.
    Required,
    /// A failed control marks the row NOT VERIFIED and is reported, not hidden.
    Environment,
}

struct Run<'a> {
    scratch: &'a Scratch,
    features: BTreeSet<String>,
    unverified: Vec<String>,
}

impl Run<'_> {
    /// Runs `line` without and with confinement and judges the pair.
    fn pair(
        &mut self,
        label: &str,
        line: &str,
        control: Expect,
        confined: Expect,
        need: Need,
    ) -> (Outcome, Outcome) {
        let control_outcome = start(self.scratch, Mode::Control).ask(line);
        let confined_outcome = start(
            self.scratch,
            Mode::Confined {
                features: &self.features,
            },
        )
        .ask(line);
        self.judge(
            label,
            control,
            confined,
            need,
            &control_outcome,
            &confined_outcome,
        );
        (control_outcome, confined_outcome)
    }

    fn judge(
        &mut self,
        label: &str,
        control: Expect,
        confined: Expect,
        need: Need,
        control_outcome: &Outcome,
        confined_outcome: &Outcome,
    ) {
        if !control.holds(control_outcome) {
            let note = format!(
                "{label}: control gave `{}` instead of {control:?}",
                control_outcome.text()
            );
            if need != Need::Environment {
                fail(
                    label,
                    control_outcome,
                    confined_outcome,
                    &format!("required control failed: {note}"),
                );
            }
            record(
                label,
                control_outcome,
                confined_outcome,
                "NOT VERIFIED (control failed)",
            );
            self.unverified.push(note);
            return;
        }
        if !confined.holds(confined_outcome) {
            fail(
                label,
                control_outcome,
                confined_outcome,
                &format!(
                    "{label}: under confinement expected {confined:?}, got `{}` (control: `{}`)",
                    confined_outcome.text(),
                    control_outcome.text()
                ),
            );
        }
        record(
            label,
            control_outcome,
            confined_outcome,
            "confined as expected",
        );
    }
}

/// Records the failing row, writes the table so the evidence upload still has
/// it, then fails the test.
fn fail(label: &str, control: &Outcome, confined: &Outcome, message: &str) -> ! {
    record(label, control, confined, "FAILED");
    write_report(&format!("matrix-{}", std::env::consts::ARCH));
    panic!("{message}");
}

fn in_container() -> bool {
    std::env::var_os("AMONG_HOSTILE_CONTAINER").is_some()
}

#[test]
fn every_hostile_action_is_denied_under_confinement_after_succeeding_without_it() {
    if !full_protection_available() {
        return;
    }
    let scratch = Scratch::new("matrix");
    let mut run = Run {
        scratch: &scratch,
        features: BTreeSet::new(),
        unverified: Vec::new(),
    };

    // network: a listener on the loopback stands in for any remote service.
    let tcp_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let tcp_port = tcp_listener.local_addr().unwrap().port();
    run.pair(
        "TCP connect",
        &format!("tcp {tcp_port}"),
        Expect::Ok,
        Expect::Errno("EPERM"),
        Need::Required,
    );
    let udp_listener = UdpSocket::bind("127.0.0.1:0").unwrap();
    udp_listener
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let udp_port = udp_listener.local_addr().unwrap().port();
    let (control, confined) = run.pair(
        "UDP send",
        &format!("udp {udp_port}"),
        Expect::Ok,
        Expect::Errno("EPERM"),
        Need::Required,
    );
    assert!(control.succeeded() && !confined.succeeded());
    let mut datagram = [0u8; 4];
    assert_eq!(
        udp_listener.recv(&mut datagram).unwrap(),
        1,
        "the control datagram arrived"
    );
    udp_listener
        .set_read_timeout(Some(std::time::Duration::from_millis(200)))
        .unwrap();
    assert!(
        udp_listener.recv(&mut datagram).is_err(),
        "the confined run sent nothing"
    );

    // a local service: a UNIX socket stands in for the session bus.
    let bus = scratch.home.join("bus.sock");
    let _bus_listener = UnixListener::bind(&bus).unwrap();
    run.pair(
        "AF_UNIX connect (session bus stand-in)",
        &format!("unix {}", bus.display()),
        Expect::Ok,
        Expect::Errno("EPERM"),
        Need::Required,
    );
    run.pair(
        "socketpair",
        "socketpair",
        Expect::Ok,
        Expect::Errno("EPERM"),
        Need::Required,
    );
    run.pair(
        "io_uring_setup",
        "io_uring",
        Expect::Ok,
        Expect::Errno("EPERM"),
        Need::Environment,
    );

    // files.
    let secret = scratch.home.join("secret.txt");
    run.pair(
        "read a file in the synthetic home",
        &format!("read {}", secret.display()),
        Expect::Ok,
        Expect::Errno("EACCES"),
        Need::Required,
    );
    run.pair(
        "read /etc/passwd (outside the read set)",
        "read /etc/passwd",
        Expect::Ok,
        Expect::Errno("EACCES"),
        Need::Required,
    );
    let outside = scratch.home.join("created-by-plugin.txt");
    run.pair(
        "create a file outside the data directory",
        &format!("write {}", outside.display()),
        Expect::Ok,
        Expect::Errno("EACCES"),
        Need::Required,
    );
    let _ = std::fs::remove_file(&outside);
    let inside = scratch.data_path().join("note.txt");
    run.pair(
        "write inside the data directory",
        &format!("write {}", inside.display()),
        Expect::Ok,
        Expect::Ok,
        Need::Required,
    );
    run.pair(
        "read the dynamic loader cache",
        "read /etc/ld.so.cache",
        Expect::Ok,
        Expect::Ok,
        Need::Environment,
    );
    run.pair(
        "read the system runtime",
        "read /usr/bin/env",
        Expect::Ok,
        Expect::Ok,
        Need::Required,
    );

    // metadata Landlock does not mediate, on a file the plugin may write.
    let data_file = scratch.data_path().join("file.txt");
    std::fs::write(&data_file, b"user data").unwrap();
    std::fs::set_permissions(&data_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    run.pair(
        "chmod a data file",
        &format!("chmod {}", data_file.display()),
        Expect::Ok,
        Expect::Errno("EPERM"),
        Need::Required,
    );
    assert_eq!(
        std::fs::metadata(&data_file).unwrap().permissions().mode() & 0o777,
        0o666,
        "the control changed the mode and the confined run did not undo it"
    );
    std::fs::set_permissions(&data_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    run.pair(
        "chown a data file",
        &format!("chown {}", data_file.display()),
        Expect::Ok,
        Expect::Errno("EPERM"),
        Need::Required,
    );
    run.pair(
        "setxattr on a data file",
        &format!("setxattr {}", data_file.display()),
        Expect::Ok,
        Expect::Errno("EPERM"),
        Need::Environment,
    );
    run.pair(
        "utimensat on a data file",
        &format!("utimensat {}", data_file.display()),
        Expect::Ok,
        Expect::Errno("EPERM"),
        Need::Required,
    );
    // truncation of a file the plugin may read but not write: only Landlock
    // from ABI 3 refuses it.
    let package_file = scratch.generation.join("readme");
    let before = std::fs::read(&package_file).unwrap();
    run.pair(
        "truncate a read-only package file",
        &format!("truncate {}", package_file.display()),
        Expect::Ok,
        Expect::Errno("EACCES"),
        Need::Required,
    );
    // the control emptied it; restore for later runs.
    std::fs::write(&package_file, before).unwrap();

    // other processes.
    run.pair(
        "ptrace a child",
        "ptrace_child",
        Expect::Ok,
        Expect::Errno("EPERM"),
        Need::Environment,
    );
    run.pair(
        "pidfd_open + pidfd_getfd on a child",
        "pidfd_getfd_child",
        Expect::Ok,
        Expect::Errno("EPERM"),
        Need::Environment,
    );

    let mut victim = Victim::new();
    let environ = format!("read /proc/{}/environ", victim.pid());
    run.pair(
        "read /proc/<pid>/environ of another process",
        &environ,
        Expect::Ok,
        Expect::Errno("EACCES"),
        Need::Required,
    );

    // signals reach the victim only without confinement.
    let confined = {
        let features = BTreeSet::new();
        let mut session = start(
            &scratch,
            Mode::Confined {
                features: &features,
            },
        );
        session.ask(&format!("kill {}", victim.pid()))
    };
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(victim.alive(), "a confined plugin killed another process");
    assert!(
        confined.failed_with("EPERM"),
        "kill confined: {}",
        confined.text()
    );
    let control = start(&scratch, Mode::Control).ask(&format!("kill {}", victim.pid()));
    assert!(control.succeeded(), "kill control: {}", control.text());
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(
        !victim.alive(),
        "the control signal must have reached the victim"
    );
    record(
        "kill another process",
        &control,
        &confined,
        "confined as expected",
    );

    // a forked child outlives its parent and signals later; the filter is inherited.
    let mut victim = Victim::new();
    let result_file = scratch.data_path().join("orphan.result");
    let _ = std::fs::remove_file(&result_file);
    let line = format!("orphan_signal {} {}", victim.pid(), result_file.display());
    let control = start(&scratch, Mode::Control).ask(&line);
    std::thread::sleep(std::time::Duration::from_millis(900));
    let control_result = std::fs::read_to_string(&result_file).unwrap_or_default();
    assert!(
        control.succeeded() && control_result == "ok",
        "orphan control: {control:?} {control_result}"
    );
    assert!(!victim.alive(), "the control orphan signalled the victim");
    let mut victim = Victim::new();
    let _ = std::fs::remove_file(&result_file);
    let line = format!("orphan_signal {} {}", victim.pid(), result_file.display());
    let confined = {
        let mut session = start(
            &scratch,
            Mode::Confined {
                features: &BTreeSet::new(),
            },
        );
        let outcome = session.ask(&line);
        // the parent exits; the child keeps running under the inherited filter.
        session.ask("exit");
        outcome
    };
    std::thread::sleep(std::time::Duration::from_millis(900));
    assert_eq!(
        std::fs::read_to_string(&result_file).unwrap_or_default(),
        "err:EPERM"
    );
    assert!(
        victim.alive(),
        "a signal from an orphaned child reached the victim"
    );
    record(
        "signal from a forked child after the parent exited",
        &Outcome::Answered(control_result),
        &Outcome::Answered("err:EPERM".into()),
        "confined as expected",
    );
    let _ = confined;

    // process control aimed at another process.
    let mut victim = Victim::new();
    let pid = victim.pid();
    run.pair(
        "prlimit64 on another process",
        &format!("prlimit {pid}"),
        Expect::Ok,
        Expect::Errno("EPERM"),
        Need::Required,
    );
    // the control changed the victim's limit; the confined run could not have.
    let limits_after_control = victim.limits();
    assert!(
        limits_after_control
            .lines()
            .any(|l| l.starts_with("Max open files") && l.contains(" 7 ")),
        "the control must have lowered the victim's limit: {limits_after_control}"
    );
    let mut fresh = Victim::new();
    let session_victim = fresh.pid();
    {
        let features = BTreeSet::new();
        let mut session = start(
            &scratch,
            Mode::Confined {
                features: &features,
            },
        );
        assert!(session
            .ask(&format!("prlimit {session_victim}"))
            .failed_with("EPERM"));
    }
    assert!(
        !fresh
            .limits()
            .lines()
            .any(|l| l.starts_with("Max open files") && l.contains(" 7 ")),
        "a confined plugin changed another process's limit"
    );
    assert!(fresh.alive());
    run.pair(
        "sched_setaffinity on another process",
        &format!("affinity {pid}"),
        Expect::Ok,
        Expect::Errno("EPERM"),
        Need::Required,
    );
    run.pair(
        "setpriority(PRIO_PROCESS, other pid)",
        &format!("setpriority_process {pid}"),
        Expect::Ok,
        Expect::Errno("EPERM"),
        Need::Required,
    );
    run.pair(
        "ioprio_set(IOPRIO_WHO_PROCESS, other pid)",
        &format!("ioprio_other {pid}"),
        Expect::Ok,
        Expect::Errno("EPERM"),
        Need::Environment,
    );
    let _ = victim.alive();

    // user-wide priority changes: positive controls only under a throwaway
    // user in a disposable container, never against a real session.
    for (label, action) in [
        ("setpriority(PRIO_PGRP, 0)", "setpriority_group_self"),
        ("setpriority(PRIO_USER, 0)", "setpriority_user_self"),
    ] {
        if in_container() {
            run.pair(
                label,
                action,
                Expect::Ok,
                Expect::Errno("EPERM"),
                Need::Required,
            );
        } else {
            let note = format!("{label}: control not run outside a disposable container");
            record(
                label,
                &Outcome::Answered("not run".into()),
                &Outcome::Answered("not run".into()),
                "NOT VERIFIED (container-only)",
            );
            run.unverified.push(note);
        }
    }

    // abort must still end the process although it cannot signal itself.
    run.pair(
        "abort()",
        "abort",
        Expect::DiedBySignal,
        Expect::DiedBySignal,
        Need::Required,
    );

    // new user, mount and network namespaces hold a full capability set inside
    // themselves. the seccomp filter and Landlock domain are inherited and no_new_privs
    // is set, so none of the denied things becomes possible there.
    let namespace_line = format!("userns {} {}", secret.display(), outside.display());
    let control = start(&scratch, Mode::Control).ask(&namespace_line);
    let confined = start(
        &scratch,
        Mode::Confined {
            features: &BTreeSet::new(),
        },
    )
    .ask(&namespace_line);
    let entered = |outcome: &Outcome| outcome.text().contains("unshare=ok");
    if entered(&control) {
        let text = confined.text();
        assert!(
            entered(&confined),
            "unshare is allowed and harmless: {text}"
        );
        for expected in [
            "mount=err:EPERM",
            "socket=err:EPERM",
            "read=err:EACCES",
            "write=err:EACCES",
        ] {
            assert!(
                text.contains(expected),
                "inside a user namespace `{expected}` must hold: {text}"
            );
        }
        // the control shows what the namespace gives when nothing confines it.
        // a host policy such as AppArmor's restriction of unprivileged user
        // namespaces can still refuse part of it (mount), and then that part
        // is not verified, although the confined result above holds anyway.
        let missing: Vec<&str> = ["mount=ok", "socket=ok", "read=ok", "write=ok"]
            .into_iter()
            .filter(|allowed| !control.text().contains(allowed))
            .collect();
        if missing.is_empty() {
            record(
                "inside a new user namespace: mount, socket, read, write",
                &control,
                &confined,
                "confined as expected",
            );
        } else {
            record(
                "inside a new user namespace: mount, socket, read, write",
                &control,
                &confined,
                "NOT VERIFIED for the parts the control was refused (see control column)",
            );
            run.unverified
                .push(format!("userns control did not allow {missing:?}"));
        }
    } else {
        record(
            "inside a new user namespace: mount, socket, read, write",
            &control,
            &confined,
            "NOT VERIFIED (control cannot create a user namespace here)",
        );
        run.unverified
            .push(format!("userns control gave `{}`", control.text()));
    }

    // alternative system call entry points.
    #[cfg(target_arch = "x86_64")]
    {
        let control = start(&scratch, Mode::Control).ask("x32");
        let confined = start(
            &scratch,
            Mode::Confined {
                features: &BTreeSet::new(),
            },
        )
        .ask("x32");
        // the filter kills with SIGSYS before the kernel looks at the call, so
        // that is asserted whatever the control did.
        assert!(
            matches!(confined, Outcome::Died(Some(libc::SIGSYS))),
            "x32 confined must die by SIGSYS: {}",
            confined.text()
        );
        // a control that merely returns ENOSYS shows only that this kernel has
        // no x32 entry, not that the call would have worked: not verified.
        if control.succeeded() {
            record(
                "x32 system call entry (x86-64)",
                &control,
                &confined,
                "confined as expected",
            );
        } else {
            record(
                "x32 system call entry (x86-64)",
                &control,
                &confined,
                "NOT VERIFIED (control returns an error: this kernel has no working x32 entry)",
            );
            run.unverified
                .push(format!("x32 control gave `{}`", control.text()));
        }
        let control = start(&scratch, Mode::Control).ask("compat32");
        let confined = start(
            &scratch,
            Mode::Confined {
                features: &BTreeSet::new(),
            },
        )
        .ask("compat32");
        assert!(
            matches!(confined, Outcome::Died(Some(libc::SIGSYS))),
            "int 0x80 confined must die by SIGSYS: {}",
            confined.text()
        );
        if control.succeeded() {
            record(
                "32-bit int 0x80 entry (x86-64)",
                &control,
                &confined,
                "confined as expected",
            );
        } else {
            record(
                "32-bit int 0x80 entry (x86-64)",
                &control,
                &confined,
                "NOT VERIFIED (control failed)",
            );
            run.unverified
                .push(format!("int 0x80 control gave `{}`", control.text()));
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        record("x32 and 32-bit entries", &Outcome::Answered("n/a".into()), &Outcome::Answered("n/a".into()), "NOT VERIFIED (no x86-64 compat entry points on this architecture; architecture filter covered by unit tests only)");
        run.unverified
            .push("x32 and 32-bit entries are x86-64 only".into());
    }

    // the environment a confined plugin sees.
    let features = BTreeSet::new();
    let mut session = start(
        &scratch,
        Mode::Confined {
            features: &features,
        },
    );
    let names = session.ask("environment");
    assert_eq!(
        names,
        Outcome::Answered("ok:AMONG_PLUGIN_DATA_DIR,PATH".into())
    );
    let data = session.ask("data_dir");
    assert_eq!(
        data,
        Outcome::Answered(format!("ok:{}", scratch.data_path().display()))
    );
    drop(session);

    write_report(&format!("matrix-{}", std::env::consts::ARCH));
    if !run.unverified.is_empty() {
        eprintln!("NOT VERIFIED rows:\n{}", run.unverified.join("\n"));
    }
}

#[test]
fn the_system_metrics_grant_adds_exactly_three_files() {
    if !full_protection_available() {
        return;
    }
    let scratch = Scratch::new("metrics");
    let victim = Victim::new();
    let environ = format!("read /proc/{}/environ", victim.pid());
    let without = BTreeSet::new();
    let with: BTreeSet<String> = ["system.metrics".to_string()].into();
    for file in ["/proc/stat", "/proc/meminfo", "/proc/loadavg"] {
        let line = format!("read {file}");
        let control = start(&scratch, Mode::Control).ask(&line);
        assert!(control.succeeded(), "{file} control: {}", control.text());
        let denied = start(&scratch, Mode::Confined { features: &without }).ask(&line);
        assert!(
            denied.failed_with("EACCES"),
            "{file} without the grant: {}",
            denied.text()
        );
        let allowed = start(&scratch, Mode::Confined { features: &with }).ask(&line);
        assert!(
            allowed.succeeded(),
            "{file} with the grant: {}",
            allowed.text()
        );
        record(
            &format!("read {file}"),
            &control,
            &allowed,
            "granted by system.metrics; denied without",
        );
    }
    // nothing else of /proc opens with the grant.
    let mut granted = start(&scratch, Mode::Confined { features: &with });
    for line in [
        environ.as_str(),
        "read /proc/self/environ",
        "read /proc/self/status",
        "read /proc/version",
        "read /proc/cpuinfo",
    ] {
        let outcome = granted.ask(line);
        assert!(
            outcome.failed_with("EACCES"),
            "`{line}` with the grant: {}",
            outcome.text()
        );
    }
    let control_environ = start(&scratch, Mode::Control).ask(&environ);
    assert!(
        control_environ.succeeded(),
        "the other process's environ is readable without confinement"
    );
    write_report(&format!("metrics-{}", std::env::consts::ARCH));
}

/// Descriptors above stderr must not survive into the plugin.
#[test]
fn only_the_standard_descriptors_and_the_executable_survive_exec() {
    const MARKER: &str = "AMONG_DESCRIPTOR_LEAK_PROCESS";
    // the test leaks descriptors and raises a limit on purpose, so it runs in
    // a process of its own where no sibling test can inherit them.
    if std::env::var_os(MARKER).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "plugin::sandbox::hostile::only_the_standard_descriptors_and_the_executable_survive_exec",
                "--exact",
                "--nocapture",
            ])
            .env(MARKER, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    if !full_protection_available() {
        return;
    }
    // raise the soft limit so the scan covers numbers above 1024.
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) };
    let wanted = 4096.min(limit.rlim_max);
    assert!(
        wanted >= 2200,
        "hard descriptor limit too low to place leaks above 1024"
    );
    limit.rlim_cur = wanted;
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
    let scratch = Scratch::new("descriptors");
    // leaked control descriptors without close-on-exec, below and above 1024
    // and near the limit, as a host library could leave behind.
    let source = std::fs::File::open("/dev/null").unwrap();
    let mut leaked = Vec::new();
    for number in [200, 1023, 1030, 2000, (wanted - 1) as i32] {
        let fd = unsafe { libc::fcntl(source.as_raw_fd(), libc::F_DUPFD, number) };
        assert!(fd >= number, "could not place a leak at {number}");
        leaked.push(fd);
    }
    let listing = |outcome: Outcome| -> Vec<i32> {
        let Outcome::Answered(text) = outcome else {
            panic!("no listing: {outcome:?}")
        };
        text.strip_prefix("ok:")
            .unwrap()
            .split(',')
            .filter(|number| !number.is_empty())
            .map(|number| number.parse().unwrap())
            .collect()
    };
    // the probe finds every leak when nothing closes them (control).
    let control = listing(start(&scratch, Mode::Control).ask("descriptors"));
    for fd in &leaked {
        assert!(
            control.contains(fd),
            "the probe missed leaked descriptor {fd}: {control:?}"
        );
    }
    // under the launch path none of them survives; only stdio and the pinned
    // executable remain.
    let features = BTreeSet::new();
    let confined = listing(
        start(
            &scratch,
            Mode::Confined {
                features: &features,
            },
        )
        .ask("descriptors"),
    );
    for fd in &leaked {
        assert!(
            !confined.contains(fd),
            "descriptor {fd} survived into the plugin: {confined:?}"
        );
    }
    assert!(
        confined.iter().take_while(|fd| **fd < 3).count() == 3,
        "stdio is open: {confined:?}"
    );
    let extra: Vec<_> = confined.iter().filter(|fd| **fd >= 3).collect();
    assert_eq!(
        extra.len(),
        1,
        "exactly the pinned executable stays open: {confined:?}"
    );
    for fd in leaked {
        unsafe { libc::close(fd) };
    }
}

#[test]
fn the_fixture_survives_confinement_when_it_does_only_what_is_allowed() {
    // the allowed path is part of the evidence: the loader, libraries and the
    // data directory work, so a denial is not a broken sandbox.
    if !full_protection_available() {
        return;
    }
    let scratch = Scratch::new("allowed");
    let features = BTreeSet::new();
    let mut session = start(
        &scratch,
        Mode::Confined {
            features: &features,
        },
    );
    let applied = session.applied.clone().expect("a confined start reports");
    assert!(applied.protection.is_full());
    assert!(applied.report.seccomp_loaded && applied.report.no_new_privs);
    let note = scratch.data_path().join("allowed.txt");
    assert!(session
        .ask(&format!("write {}", note.display()))
        .succeeded());
    let mut written = String::new();
    std::fs::File::open(&note)
        .unwrap()
        .read_to_string(&mut written)
        .unwrap();
    assert_eq!(written, "hostile");
    assert!(Path::new(&note).exists());
}
