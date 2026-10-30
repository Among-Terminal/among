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

fn replace_stat_field(record: &[u8], index: usize, value: &[u8]) -> Vec<u8> {
    let space = record.iter().position(|byte| *byte == b' ').unwrap();
    let end = record.iter().rposition(|byte| *byte == b')').unwrap();
    let mut output = Vec::new();
    output.extend_from_slice(if index == 1 { value } else { &record[..space] });
    output.extend_from_slice(b" (");
    output.extend_from_slice(if index == 2 {
        value
    } else {
        &record[space + 2..end]
    });
    output.extend_from_slice(b") ");
    for (offset, field) in record[end + 2..]
        .strip_suffix(b"\n")
        .unwrap()
        .split(|byte| *byte == b' ')
        .enumerate()
    {
        if offset != 0 {
            output.push(b' ');
        }
        output.extend_from_slice(if index == offset + 3 { value } else { field });
    }
    output.push(b'\n');
    output
}

fn stat_record(pid: u32, group: i32, state: u8) -> Vec<u8> {
    // retain a genuine kernel tail, not the old truncated positive prefixes.
    let mut record = std::fs::read("/proc/self/stat").unwrap();
    for (index, value) in [
        (1, pid.to_string()),
        (3, char::from(state).to_string()),
        (4, "1".into()),
        (5, group.to_string()),
        (6, group.to_string()),
    ] {
        record = replace_stat_field(&record, index, value.as_bytes());
    }
    record
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
        stat_group(&replace_stat_field(
            &stat_record(8, 77, b'Z'),
            2,
            b"comm ) R 1 123 \xff"
        ))
        .unwrap(),
        (8, 77)
    );
    assert_eq!(stat_group(&stat_record(9, 77, b'R')).unwrap(), (9, 77));
    assert_eq!(stat_group(&stat_record(9, 0, b'S')).unwrap(), (9, 0));
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
        std::fs::write(path.join("stat"), stat_record(pid, 44, b'R')).unwrap();
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
    std::fs::write(root.join("1/stat"), stat_record(1, 77, b'R')).unwrap();
    let mut probe = GroupQuiescence::new(&root);
    for _ in 0..8 {
        assert!(!probe
            .poll(77, |pid| {
                assert_eq!(pid, 1);
                Ok(false)
            })
            .unwrap());
    }
    std::fs::write(root.join("1/stat"), stat_record(1, 77, b'Z')).unwrap();
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
    std::fs::write(root.join("1/stat"), stat_record(1, 77, b'Z')).unwrap();
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

fn unknown_stat_case(label: &str, directory: &str, bytes: &[u8]) {
    let root = root();
    let path = root.join(directory);
    std::fs::create_dir(&path).unwrap();
    let stat = path.join("stat");
    let mut probe = GroupQuiescence::new(&root);
    // an earlier valid foreign observation must not survive corruption.
    std::fs::write(&stat, stat_record(999, 44, b'S')).unwrap();
    if directory == "999" {
        assert!(!probe.poll(77, |_| panic!("foreign callback")).unwrap());
    }
    std::fs::write(&stat, bytes).unwrap();
    let mut cold = GroupQuiescence::new(&root);
    for scan in 1..=2 {
        let result = cold.poll(77, |_| {
            panic!("{label}: malformed cold record authorized pidfd")
        });
        assert!(result.is_err(), "{label} cold scan={scan}: {result:?}");
        let result = probe.poll(77, |_| panic!("{label}: malformed record authorized pidfd"));
        assert!(result.is_err(), "{label} scan={scan}: {result:?}");
    }
    println!("UNKNOWN label={label} directory={directory}: cold and prior-quiet first/subsequent scans Err; no pidfd callback; input={bytes:?}");
}

#[test]
fn reviewer_r1_malformed_foreign_and_matching_records_never_establish_quiet() {
    for group in [44, 77] {
        let input = format!("not-a-pid (broken) ? not-a-ppid {group} not-a-session\n");
        assert!(stat_group(input.as_bytes()).is_err());
        unknown_stat_case(
            &format!("reviewer-R1-group-{group}"),
            "999",
            input.as_bytes(),
        );
    }
}

#[test]
fn malformed_stat_matrix_is_unknown_before_foreign_or_matching_classification() {
    for group in [44, 77] {
        let record = stat_record(999, group, b'S');
        let mut variants = Vec::new();
        for (label, field, value) in [
            ("pid-text", 1, "not-a-pid"),
            ("pid-zero", 1, "0"),
            ("pid-negative", 1, "-999"),
            ("pid-plus", 1, "+999"),
            ("pid-leading-zero", 1, "0999"),
            ("pid-overflow", 1, "2147483648"),
            (
                "pid-huge",
                1,
                "999999999999999999999999999999999999999999999",
            ),
            ("pid-directory-mismatch", 1, "998"),
            ("state-unknown", 3, "?"),
            ("state-multiple", 3, "RS"),
            ("state-empty", 3, ""),
            ("ppid-text", 4, "not-a-ppid"),
            ("ppid-negative", 4, "-1"),
            ("ppid-overflow", 4, "2147483648"),
            ("ppid-minus-zero", 4, "-0"),
            ("pgrp-text", 5, "bad"),
            ("pgrp-negative", 5, "-77"),
            ("pgrp-overflow", 5, "2147483648"),
            ("pgrp-plus", 5, "+77"),
            ("session-text", 6, "not-a-session"),
            ("session-negative", 6, "-1"),
            ("session-overflow", 6, "2147483648"),
            ("session-missing", 6, ""),
            ("tty-overflow", 7, "2147483648"),
            ("flags-overflow", 9, "4294967296"),
            ("unsigned-negative", 10, "-1"),
            ("unsigned-overflow", 14, "18446744073709551616"),
            ("signed-overflow", 16, "9223372036854775808"),
            ("signed-underflow", 17, "-9223372036854775809"),
            ("threads-negative", 20, "-1"),
            ("tail-text", 52, "bad"),
        ] {
            variants.push((label, replace_stat_field(&record, field, value.as_bytes())));
        }
        let mut opening = record.clone();
        opening[4] = b'[';
        variants.push(("comm-opening", opening));
        let end = record.iter().rposition(|byte| *byte == b')').unwrap();
        let mut closing = record.clone();
        closing[end] = b']';
        variants.push(("comm-closing", closing));
        let mut separator = record.clone();
        separator[end + 1] = b'\t';
        variants.push(("state-separator", separator));
        variants.push(("comm-nul", replace_stat_field(&record, 2, b"bad\0comm")));
        variants.push(("missing-newline", record[..record.len() - 1].to_vec()));
        variants.push(("double-newline", [record.as_slice(), b"\n"].concat()));
        variants.push(("suffix-text", [record.as_slice(), b"junk"].concat()));
        variants.push((
            "unknown-extra-field",
            replace_stat_field(&record, 52, b"0 0"),
        ));
        variants.push((
            "missing-last-field",
            record[..record.iter().rposition(|byte| *byte == b' ').unwrap()].to_vec(),
        ));
        variants.push(("truncated-header", b"999 (fixture) S 1".to_vec()));
        variants.push((
            "truncated-after-session",
            format!("999 (fixture) S 1 {group} 77\n").into_bytes(),
        ));
        let long = replace_stat_field(&record, 2, &vec![b'c'; 4096]);
        variants.push(("length-over-bound", long));
        for (label, bytes) in variants {
            unknown_stat_case(&format!("group-{group}-{label}"), "999", &bytes);
        }
        for directory in [
            "0",
            "0999",
            "2147483648",
            "9999999999999999999999999999999999999999",
        ] {
            unknown_stat_case(
                &format!("group-{group}-directory-identity"),
                directory,
                &record,
            );
        }
        for cut in 0..record.len() {
            assert!(
                stat_group(&record[..cut]).is_err(),
                "accepted truncation at {cut}"
            );
        }
        println!(
            "TRUNCATION group={group}: all {} proper byte prefixes rejected",
            record.len()
        );
    }
}

#[test]
fn genuine_kernel_records_gate_pidfd_admission_and_preserve_comm_bytes() {
    let record = std::fs::read("/proc/self/stat").unwrap();
    let pid = std::process::id();
    let group = unsafe { libc::getpgrp() };
    assert_eq!(stat_group(&record).unwrap(), (pid as i32, group));
    println!("GENUINE self stat={record:?}");
    let root = root();
    let directory = root.join(pid.to_string());
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("stat"), &record).unwrap();
    let mut foreign = GroupQuiescence::new(&root);
    let other = if group == 77 { 78 } else { 77 };
    assert!(!foreign
        .poll(other, |_| panic!("genuine foreign record callback"))
        .unwrap());
    assert!(foreign
        .poll(other, |_| panic!("genuine foreign record callback"))
        .unwrap());
    let mut matching = GroupQuiescence::new(&root);
    let mut calls = 0;
    for _ in 0..2 {
        assert!(!matching
            .poll(group, |observed| {
                assert_eq!(observed, pid);
                calls += 1;
                member_exited(observed)
            })
            .unwrap());
    }
    assert_eq!(calls, 2);
    let unusual = std::thread::spawn(|| {
        let name = std::ffi::CString::new(b" ()(\xff\n".to_vec()).unwrap();
        assert_eq!(
            unsafe { libc::prctl(libc::PR_SET_NAME, name.as_ptr(), 0, 0, 0) },
            0
        );
        let record = std::fs::read("/proc/thread-self/stat").unwrap();
        let tid = unsafe { libc::syscall(libc::SYS_gettid) } as i32;
        assert_eq!(
            stat_group(&record).unwrap(),
            (tid, unsafe { libc::getpgrp() })
        );
        assert!(record
            .windows(name.as_bytes().len())
            .any(|window| window == name.as_bytes()));
        record
    })
    .join()
    .unwrap();
    println!("GENUINE kernel positives: self PID={pid} pgrp={group}; foreign quiet without callback; matching live pidfd queried twice; unusual comm bytes record={unusual:?}");

    let mut child = OwnedFixture::spawn(Command::new("/bin/true").process_group(0));
    let owned = child.child.as_mut().unwrap();
    tests::until(|| child_exited(owned.id()).unwrap());
    let zombie = std::fs::read(format!("/proc/{}/stat", owned.id())).unwrap();
    let (zpid, zgroup) = stat_group(&zombie).unwrap();
    println!("GENUINE exited child stat={zombie:?}");
    assert_eq!(zpid as u32, owned.id());
    assert_eq!(zgroup, zpid);
    std::fs::remove_file(directory.join("stat")).unwrap();
    std::fs::remove_dir(directory).unwrap();
    let directory = root.join(zpid.to_string());
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("stat"), &zombie).unwrap();
    let mut matching = GroupQuiescence::new(&root);
    assert!(!matching.poll(zgroup, member_exited).unwrap());
    assert!(matching.poll(zgroup, member_exited).unwrap());
    child.child.take().unwrap().wait().unwrap();
    println!("GENUINE kernel exited member PID={zpid}: pidfd admitted and two quiet scans; direct child then reaped");
}

#[test]
fn zero_identity_fields_known_states_and_exact_read_bound_remain_valid() {
    let mut zero = stat_record(999, 0, b'S');
    zero = replace_stat_field(&zero, 4, b"0");
    assert_eq!(stat_group(&zero).unwrap(), (999, 0));
    assert_eq!(
        stat_group(&replace_stat_field(&zero, 2, b"")).unwrap(),
        (999, 0)
    );
    for state in b"RSDZTtXxKWPI" {
        assert_eq!(
            stat_group(&replace_stat_field(&zero, 3, &[*state])).unwrap(),
            (999, 0)
        );
    }
    let end = zero.iter().rposition(|byte| *byte == b')').unwrap();
    let comm_len = end - zero.iter().position(|byte| *byte == b'(').unwrap() - 1;
    let bounded = replace_stat_field(&zero, 2, &vec![b'c'; 4095 - zero.len() + comm_len]);
    assert_eq!(bounded.len(), 4095);
    assert_eq!(stat_group(&bounded).unwrap(), (999, 0));
    let root = root();
    std::fs::create_dir(root.join("999")).unwrap();
    let path = root.join("999/stat");
    std::fs::write(&path, &bounded).unwrap();
    assert_eq!(member_group(&path).unwrap(), Some(0));
    let over = replace_stat_field(&bounded, 2, &vec![b'c'; 4096 - zero.len() + comm_len]);
    assert_eq!(over.len(), 4096);
    std::fs::write(&path, &over).unwrap();
    assert!(member_group(&path).is_err());
    println!("BOUNDS zero PPID/pgrp/session and known state bytes accepted; 4095-byte full record accepted, 4096 rejected");
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

struct OwnedFixture {
    child: Option<Child>,
    admission: Arc<Lease>,
    proof_control: ProofControl,
}

impl OwnedFixture {
    fn spawn(command: &mut Command) -> Self {
        // Direct-child probes need explicit WNOWAIT/reap access. Keep their
        // cleanup authority alive from before spawn through group absence.
        let admission = Arc::new(Lease::new());
        let proof_control = ProofControl::default();
        let child = command.spawn().unwrap();
        Self {
            child: Some(child),
            admission,
            proof_control,
        }
    }
}

impl Drop for OwnedFixture {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            terminate_and_reap(child, &self.admission, &self.proof_control);
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Identity {
    group: i32,
    start: u64,
}

fn identity(pid: u32) -> Identity {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let fields: Vec<_> = stat
        .rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .collect();
    Identity {
        group: fields[2].parse().unwrap(),
        start: fields[19].parse().unwrap(),
    }
}

fn descendant_from_stdout(child: &mut Child) -> u32 {
    let mut stdout = child.stdout.take().unwrap();
    nonblocking(stdout.as_raw_fd()).unwrap();
    let (sender, frames) = sync_channel(MAX_QUEUE_FRAMES);
    let mut decoder = FrameDecoder::default();
    let mut buffer = [0; 1024];
    let mut descendant = None;
    tests::until(|| {
        match stdout.read(&mut buffer) {
            Ok(0) => panic!("fixture exited before publishing its descendant"),
            Ok(count) => decoder.push(&buffer[..count], &sender).unwrap(),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("fixture stdout: {error}"),
        }
        for frame in frames.try_iter() {
            let value: serde_json::Value = serde_json::from_str(&frame).unwrap();
            if value["type"] == "notify" {
                descendant = value["message"].as_str().unwrap().parse::<u32>().ok();
            }
        }
        descendant.is_some()
    });
    descendant.unwrap()
}

#[test]
fn reserved_leader_identity_survives_group_signals_and_only_its_child_is_reaped() {
    let root = root();
    let fixture = compile(&root);
    let cpu = cpu();
    let mut leader = OwnedFixture::spawn(
        Command::new(fixture)
            .args(["inherited", root.to_str().unwrap(), &cpu.to_string()])
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    );
    // This separate owned group is a canary, not an authority from the proc scan.
    let sentinel = OwnedFixture::spawn(
        Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null()),
    );
    let child = leader.child.as_mut().unwrap();
    let descendant = descendant_from_stdout(child);
    let pid = child.id();
    let leader_id = identity(pid);
    let descendant_id = identity(descendant);
    let sentinel_pid = sentinel.child.as_ref().unwrap().id();
    let sentinel_id = identity(sentinel_pid);
    let leader_exit = pidfd(pid);
    let descendant_exit = pidfd(descendant);
    let sentinel_exit = pidfd(sentinel_pid);
    assert_eq!(leader_id.group, i32::try_from(pid).unwrap());
    assert_eq!(descendant_id.group, leader_id.group);
    assert_eq!(sentinel_id.group, i32::try_from(sentinel_pid).unwrap());
    assert_ne!(sentinel_id.group, leader_id.group);
    assert!(!exit_ready(&leader_exit));
    assert!(!exit_ready(&descendant_exit));
    let pressure = load(cpu);
    std::fs::write(root.join("release-leader"), b"go").unwrap();
    tests::until(|| child_exited(pid).unwrap());
    assert!(exit_ready(&leader_exit));
    assert_eq!(identity(pid), leader_id);
    assert!(!exit_ready(&descendant_exit));
    signal_reserved_group(child, libc::SIGTERM).unwrap();
    std::thread::sleep(STOP_GRACE);

    let mut probe = GroupQuiescence::new(Path::new("/proc"));
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut signals = 0;
    loop {
        signal_reserved_group(child, libc::SIGKILL).unwrap();
        signals += 1;
        // WNOWAIT must still identify the original child after every group KILL;
        // its PID cannot be recycled while its original start identity remains.
        assert!(child_exited(pid).unwrap());
        assert_eq!(identity(pid), leader_id);
        assert_eq!(identity(sentinel_pid), sentinel_id);
        assert!(!exit_ready(&sentinel_exit), "foreign group was signalled");
        if probe.poll(leader_id.group, member_exited).unwrap() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "group observation did not progress"
        );
        std::thread::sleep(Duration::from_millis(IO_INTERVAL_MS as u64));
    }
    assert!(
        exit_ready(&descendant_exit),
        "group quiet before descendant exit"
    );
    assert_eq!(identity(pid), leader_id);
    let mut child = leader.child.take().unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(
        child_exited(pid).unwrap_err().raw_os_error(),
        Some(libc::ECHILD)
    );
    assert_eq!(
        signal_reserved_group(&child, libc::SIGKILL)
            .unwrap_err()
            .raw_os_error(),
        Some(libc::ECHILD)
    );
    assert_eq!(identity(sentinel_pid), sentinel_id);
    assert!(!exit_ready(&sentinel_exit));
    println!("reserved leader={pid} identity={leader_id:?}, descendant={descendant} identity={descendant_id:?}, sentinel={sentinel_pid} identity={sentinel_id:?}, group_kills={signals}; descendant pidfd ready before direct reap; original leader reaped; later group signal refused with ECHILD; sentinel unchanged and live");
    drop(pressure);
}
