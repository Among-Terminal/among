use super::*;
use std::path::{Path, PathBuf};

struct Load {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Drop for Load {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn cpu() -> usize {
    let mut allowed: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::sched_getaffinity(0, std::mem::size_of_val(&allowed), &mut allowed) },
        0
    );
    (0..libc::CPU_SETSIZE as usize)
        .find(|cpu| unsafe { libc::CPU_ISSET(*cpu, &allowed) })
        .unwrap()
}

fn load(cpu: usize) -> Load {
    let stop = Arc::new(AtomicBool::new(false));
    let ready = Arc::new(AtomicUsize::new(0));
    let mut threads = Vec::new();
    for _ in 0..2 {
        let stop = stop.clone();
        let ready = ready.clone();
        threads.push(std::thread::spawn(move || {
            let mut mask: libc::cpu_set_t = unsafe { std::mem::zeroed() };
            unsafe {
                libc::CPU_SET(cpu, &mut mask);
            }
            assert_eq!(
                unsafe { libc::sched_setaffinity(0, std::mem::size_of_val(&mask), &mask) },
                0
            );
            ready.fetch_add(1, Ordering::Release);
            let end = Instant::now() + Duration::from_secs(2);
            while !stop.load(Ordering::Acquire) && Instant::now() < end {
                std::hint::spin_loop();
            }
        }));
    }
    let end = Instant::now() + Duration::from_secs(2);
    while ready.load(Ordering::Acquire) != 2 {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(1));
    }
    Load { stop, threads }
}

fn root() -> PathBuf {
    let path = std::env::var_os("AMONG_CHECK_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target/plugin-retirement"))
        .join(format!("retirement-{}", glib::uuid_string_random()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn compile(root: &Path) -> PathBuf {
    let source = root.join("fixture.rs");
    let executable = root.join("fixture");
    std::fs::write(&source, include_str!("retirement_fixture.rs")).unwrap();
    let result = Command::new("rustc")
        .args(["--edition=2021", "-C", "codegen-units=1"])
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    executable
}

#[test]
fn finished_never_precedes_term_ignoring_descendant_quiescence_under_scheduler_pressure() {
    retirement_cases(&["inherited", "group-tree"], 12, false);
}

#[test]
fn a_zombie_main_thread_does_not_hide_a_live_descendant_worker() {
    retirement_cases(&["inherited-thread", "group-tree-thread"], 4, true);
}

fn pidfd(pid: u32) -> OwnedFd {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    assert!(fd >= 0, "pidfd_open: {}", io::Error::last_os_error());
    unsafe { OwnedFd::from_raw_fd(fd as RawFd) }
}

fn exit_ready(fd: &OwnedFd) -> bool {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert!(unsafe { libc::poll(&mut poll, 1, 0) } >= 0);
    assert_eq!(poll.revents & (libc::POLLERR | libc::POLLNVAL), 0);
    poll.revents & libc::POLLIN != 0
}

fn retirement_cases(modes: &[&str], rounds: usize, threaded: bool) {
    let root = root();
    let fixture = compile(&root);
    let cpu = cpu();
    for round in 0..rounds {
        for &mode in modes {
            let case = root.join(format!("{round}-{mode}"));
            std::fs::create_dir(&case).unwrap();
            let mut command = Command::new(&fixture);
            command.args([mode, case.to_str().unwrap(), &cpu.to_string()]);
            let process = Process::start(command).unwrap();
            let mut descendant = None;
            tests::until(|| {
                for frame in process.frames.try_iter() {
                    let value: serde_json::Value = serde_json::from_str(&frame).unwrap();
                    if value["type"] == "notify" {
                        descendant = value["message"].as_str().unwrap().parse::<u32>().ok();
                    }
                }
                descendant.is_some()
            });
            let descendant = descendant.unwrap();
            let exit = pidfd(descendant);
            assert!(!exit_ready(&exit));
            if threaded {
                tests::until(|| {
                    std::fs::read_to_string(format!("/proc/{descendant}/stat"))
                        .unwrap()
                        .rsplit_once(')')
                        .unwrap()
                        .1
                        .trim_start()
                        .starts_with('Z')
                });
                assert!(!exit_ready(&exit), "fixture must still have a live worker");
                assert!(
                    !member_exited(descendant).unwrap(),
                    "a Z main thread is not process exit"
                );
            } else {
                assert!(tests::live(descendant));
            }
            let pressure = load(cpu);
            std::fs::write(case.join("release-leader"), b"go").unwrap();
            let start = Instant::now();
            if mode.starts_with("group-tree") {
                process.stop();
                assert!(start.elapsed() < Duration::from_millis(100));
            }
            tests::until(|| process.finished());
            let stat = std::fs::read_to_string(format!("/proc/{descendant}/stat"));
            println!("round={round} mode={mode} finished_after={:?} leader={} descendant={} stat={stat:?}",start.elapsed(),process.pid(),descendant);
            assert!(
                exit_ready(&exit),
                "whole descendant process still live after {mode}"
            );
            assert!(!tests::live(process.pid()), "leader still live");
            assert!(
                !tests::live(descendant),
                "descendant still live after {mode} under scheduler pressure"
            );
            drop(pressure);
        }
    }
}

#[test]
fn stat_observation_handles_comm_bytes_and_never_guesses_a_truncated_group() {
    assert_eq!(
        stat_group(b"8 (comm ) R 1 123 \xff) Z 4 77 77 0\n").unwrap(),
        77
    );
    assert_eq!(stat_group(b"9 (fixture) R 4 77 77 0\n").unwrap(), 77);
    assert_eq!(stat_group(b"9 (fixture) S 4 0 0 0\n").unwrap(), 0);
    for bytes in [
        b"".as_slice(),
        b"8 (fixture) R 1 77",
        b"8 (fixture) R 1 +77 77",
        b"8 (fixture) R 1 2147483648 77",
        b"8 (fixture) R 1 -77 77",
    ] {
        assert!(
            stat_group(bytes).is_err(),
            "accepted malformed group: {bytes:?}"
        );
    }
}

#[test]
fn complete_bounded_scans_are_required_and_unknown_observations_are_not_quiet() {
    let root = root();
    for pid in 1..=521 {
        let path = root.join(pid.to_string());
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("stat"), format!("{pid} (fixture) R 1 44 44 0\n")).unwrap();
    }
    let mut probe = GroupQuiescence::new(&root);
    for _ in 0..5 {
        assert!(!probe
            .poll(77, |_| panic!("foreign group must not open a pidfd"))
            .unwrap());
    }
    assert!(probe
        .poll(77, |_| panic!("foreign group must not open a pidfd"))
        .unwrap());
    std::fs::write(root.join("1/stat"), b"1 (fixture) R 1 77 77 0\n").unwrap();
    let mut probe = GroupQuiescence::new(&root);
    for _ in 0..8 {
        assert!(!probe
            .poll(77, |pid| {
                assert_eq!(pid, 1);
                Ok(false)
            })
            .unwrap());
    }
    std::fs::write(root.join("1/stat"), b"1 (fixture) Z 1 77 77 0\n").unwrap();
    let mut probe = GroupQuiescence::new(&root);
    for _ in 0..5 {
        assert!(!probe
            .poll(77, |pid| {
                assert_eq!(pid, 1);
                Ok(true)
            })
            .unwrap());
    }
    assert!(probe
        .poll(77, |pid| {
            assert_eq!(pid, 1);
            Ok(true)
        })
        .unwrap());
    std::fs::write(root.join("1/stat"), b"1 (fixture) R 1 7").unwrap();
    let mut probe = GroupQuiescence::new(&root);
    let mut unknown = false;
    for _ in 0..12 {
        match probe.poll(77, |_| {
            panic!("malformed stat cannot authorize pidfd observation")
        }) {
            Ok(quiet) => assert!(!quiet),
            Err(_) => unknown = true,
        }
    }
    assert!(unknown);
}

#[test]
fn missing_or_nonregular_procfs_observations_fail_closed_without_waiting_on_a_fifo() {
    let root = root();
    assert!(GroupQuiescence::new(&root.join("absent"))
        .poll(77, |_| panic!("absent procfs"))
        .is_err());
    let path = root.join("fifo");
    let path_c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
    assert!(member_group(&path).is_err());
    std::fs::write(root.join("oversized"), vec![b'x'; 4096]).unwrap();
    assert!(member_group(&root.join("oversized")).is_err());
    assert_eq!(member_group(&root.join("gone")).unwrap(), None);
}

#[test]
fn unavailable_pidfd_observations_cannot_confirm_quiescence() {
    let root = root();
    std::fs::create_dir(root.join("1")).unwrap();
    std::fs::write(root.join("1/stat"), b"1 (fixture) Z 0 77 77 0\n").unwrap();
    for errno in [
        libc::ENOSYS,
        libc::EPERM,
        libc::EACCES,
        libc::EMFILE,
        libc::EINTR,
    ] {
        let mut probe = GroupQuiescence::new(&root);
        assert!(!probe.poll(77, |_| Ok(true)).unwrap());
        assert!(probe
            .poll(77, |_| Err(io::Error::from_raw_os_error(errno)))
            .is_err());
        assert!(!probe.poll(77, |_| Ok(true)).unwrap());
        assert!(probe.poll(77, |_| Ok(true)).unwrap());
    }
}

#[test]
fn a_reaped_leader_cannot_authorize_another_group_signal() {
    let mut child = Command::new("/bin/true")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    child.wait().unwrap();
    let error = signal_reserved_group(&child, libc::SIGKILL).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
}
