use among::notifications::commands::{
    self, Configuration, Outcome, OwnedDirectory, Reason, Runtime, DEADLINE, MAX_OUTPUT,
};
use among::notifications::{Channel, Eligibility, Focus, Host, Policy, Source};
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[path = "support/notification_command.rs"]
mod support;

fn directory(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    path
}

fn paths(root: &Path, prefix: &str) -> Vec<PathBuf> {
    fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(prefix)
        })
        .collect()
}

fn pids(root: &Path) -> Vec<u32> {
    paths(root, "started-")
        .into_iter()
        .chain(paths(root, "child-"))
        .map(|path| fs::read_to_string(path).unwrap().parse().unwrap())
        .collect()
}

fn gone(pid: u32) -> bool {
    !Path::new(&format!("/proc/{pid}")).exists()
}

fn family_gone(root: &Path) -> bool {
    pids(root)
        .into_iter()
        .chain(
            paths(root, "supervisor-")
                .into_iter()
                .map(|path| fs::read_to_string(path).unwrap().parse().unwrap()),
        )
        .all(gone)
}

fn wait(mut condition: impl FnMut() -> bool, limit: Duration, description: &str) {
    let end = Instant::now() + limit;
    while !condition() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(condition(), "timed out: {description}");
}

fn enabled() -> Policy {
    Policy {
        enabled: true,
        eligibility: Eligibility::Always,
    }
}

fn config(fixture: &Path, root: &Path, mode: &str) -> Configuration {
    Configuration::new(fixture, vec![mode.into(), root.to_str().unwrap().into()]).unwrap()
}

fn poll_until(
    runtime: &mut Runtime,
    host: &mut Host,
    channel: Channel,
    limit: Duration,
) -> commands::Completion {
    let end = Instant::now() + limit;
    loop {
        runtime.poll(host, Instant::now());
        if let Some(completion) = host
            .command_completions()
            .rev()
            .find(|completion| completion.job.channel() == channel)
        {
            return *completion;
        }
        assert!(
            Instant::now() < end,
            "completion did not arrive for {channel:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn new_runtime(root: &Path) -> Runtime {
    Runtime::new(
        Path::new(env!("CARGO_BIN_EXE_among")),
        OwnedDirectory::open(root).unwrap(),
    )
    .unwrap()
}

fn start(
    runtime: &mut Runtime,
    host: &mut Host,
    fixture: &Path,
    root: &Path,
    mode: &str,
    source: Source,
) -> Channel {
    let channel = runtime.register(host, config(fixture, root, mode)).unwrap();
    runtime.set_policy(host, channel, enabled()).unwrap();
    host.bell(source, Focus::default(), Instant::now()).unwrap();
    runtime.poll(host, Instant::now());
    channel
}

#[test]
fn real_commands_enforce_data_policy_limits_cancellation_and_reaping() {
    let parent = std::env::var_os("AMONG_COMMAND_TEST_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("target/notification-command-tests")
        });
    fs::create_dir_all(&parent).unwrap();
    let root = directory(
        &parent,
        &format!(
            "run-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ),
    );
    println!("command evidence: {}", root.display());
    let fixture = support::compile_fixture(&root);
    let cwd = directory(&root, "owned-cwd");
    let mut runtime = new_runtime(&cwd);
    let mut host = Host::new().unwrap();
    let unrelated_channel = host.register_channel(Policy::DELIVERY_DEFAULT).unwrap();
    runtime.remove(&mut host, unrelated_channel);
    assert!(
        host.set_channel_policy(unrelated_channel, Policy::DELIVERY_DEFAULT)
            .is_ok(),
        "command runtime removed another adapter's channel"
    );
    host.remove_channel(unrelated_channel);
    let source = host.attach(1).unwrap();
    let record = directory(&root, "literal");
    let injection = root.join("MUST-NOT-EXIST");
    let literals = vec![
        format!("$(touch {})", injection.display()),
        "; | & > < `false` * ~ $HOME".into(),
        "'quoted' \"literal\"\nnewline".into(),
    ];
    let mut argv = vec!["record".into(), record.to_str().unwrap().into()];
    argv.extend(literals.clone());
    assert!(Configuration::new(Path::new("relative-executable"), vec![]).is_err());
    assert!(Configuration::new(&fixture, vec!["bad\0argument".into()]).is_err());
    assert!(Configuration::new(&fixture, vec!["x".repeat(commands::MAX_CONFIGURATION)]).is_err());
    let literal_config = Configuration::new(&fixture, argv).unwrap();
    let invalid_decoded: Configuration =
        serde_json::from_str(r#"{"executable":"relative","arguments":[]}"#).unwrap();
    assert!(runtime.register(&mut host, invalid_decoded).is_err());
    let channel = runtime.register(&mut host, literal_config).unwrap();
    host.bell(source, Focus::default(), Instant::now()).unwrap();
    runtime.poll(&mut host, Instant::now());
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        paths(&record, "started-").is_empty(),
        "inspection/off default executed code"
    );
    runtime
        .set_policy(
            &mut host,
            channel,
            Policy {
                enabled: true,
                eligibility: Eligibility::ApplicationBackground,
            },
        )
        .unwrap();
    host.bell(
        source,
        Focus {
            application_active: true,
            viewed: None,
        },
        Instant::now(),
    )
    .unwrap();
    runtime.poll(&mut host, Instant::now());
    assert!(
        paths(&record, "started-").is_empty(),
        "ineligible job executed"
    );
    runtime.set_policy(&mut host, channel, enabled()).unwrap();
    host.bell(source, Focus::default(), Instant::now()).unwrap();
    host.detach(source);
    runtime.poll(&mut host, Instant::now());
    assert!(
        paths(&record, "started-").is_empty(),
        "stale source executed"
    );
    let source = host.attach(1).unwrap();
    // synthetic credentials and a non-CLOEXEC descriptor must not reach the child.
    std::env::set_var("AMONG_COMMAND_SECRET", "synthetic-never-inherit");
    std::env::set_var("AWS_SECRET_ACCESS_KEY", "synthetic-never-inherit");
    let secret_path = root.join("synthetic-private-fd");
    let secret = fs::File::create(&secret_path).unwrap();
    unsafe {
        libc::fcntl(secret.as_raw_fd(), libc::F_SETFD, 0);
    }
    let moved_cwd = root.join("owned-cwd-renamed");
    fs::rename(&cwd, &moved_cwd).unwrap();
    std::os::unix::fs::symlink(&record, &cwd).unwrap();
    host.bell(source, Focus::default(), Instant::now()).unwrap();
    let complete = poll_until(&mut runtime, &mut host, channel, Duration::from_secs(5));
    assert_eq!(complete.outcome, Outcome::Exited(0));
    assert!(complete.reaped);
    let pid = pids(&record)[0];
    assert!(gone(pid), "completed leader was not reaped");
    assert!(family_gone(&record), "supervisor was not reaped");
    assert!(!injection.exists());
    assert_eq!(
        fs::read_to_string(record.join(format!("argv-{pid}.bin"))).unwrap(),
        literals.join("\0")
    );
    assert!(record.join(format!("eof-{pid}")).is_file());
    let payload = fs::read(record.join(format!("payload-{pid}.json"))).unwrap();
    assert!(payload.len() <= commands::MAX_PAYLOAD);
    let payload: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(
        payload
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["event", "kind", "schema", "source"]
    );
    assert_eq!(payload["kind"], "terminal-bell");
    assert_eq!(payload["schema"], 1);
    assert_eq!(
        payload["event"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["id", "session"]
    );
    assert_eq!(
        payload["source"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["generation", "pane", "session"]
    );
    assert_eq!(payload["source"]["generation"], source.generation());
    let env = fs::read_to_string(record.join(format!("env-{pid}.bin"))).unwrap();
    assert_eq!(env.split('\0').count(), 3);
    assert_eq!(
        env,
        format!(
            "HOME={}\0LANG=C.UTF-8\0PATH=/usr/bin:/bin",
            moved_cwd.display()
        )
    );
    assert!(!fs::read_to_string(record.join(format!("fds-{pid}.txt")))
        .unwrap()
        .contains(secret_path.to_str().unwrap()));
    assert_eq!(
        fs::read_to_string(record.join(format!("cwd-{pid}.txt"))).unwrap(),
        moved_cwd.to_str().unwrap()
    );
    runtime.remove(&mut host, channel);
    println!("PASS inert invalid/off/ineligible/stale requests; literal argv, bounded identity-only JSON, actual EOF, exact environment, no inherited secret fd, fd-anchored owned cwd");

    for (mode, expected) in [
        ("exit7", Outcome::Exited(7)),
        ("signal", Outcome::Signaled(10)),
        ("combined", Outcome::Uncertain(Reason::OutputLimit)),
        ("inherited", Outcome::Exited(0)),
    ] {
        let output = directory(&root, mode);
        let channel = start(&mut runtime, &mut host, &fixture, &output, mode, source);
        let result = poll_until(&mut runtime, &mut host, channel, Duration::from_secs(5));
        assert_eq!(result.outcome, expected, "mode={mode}");
        assert!(result.reaped);
        if mode == "combined" {
            assert_eq!(result.output_bytes, MAX_OUTPUT);
        }
        if mode == "inherited" {
            assert!(
                pids(&output).len() >= 3,
                "inherited pipes were not exercised by the full tree"
            );
        }
        assert!(family_gone(&output), "unreaped process after {mode}");
        runtime.remove(&mut host, channel);
    }
    println!("PASS local failure is isolated; 20-KiB stdout + 20-KiB stderr hits combined 32-KiB cap; exited leader's inherited-pipe descendants stop and reap");

    let tree = directory(&root, "source-revocation");
    let channel = start(&mut runtime, &mut host, &fixture, &tree, "tree", source);
    wait(
        || pids(&tree).len() >= 3,
        Duration::from_secs(5),
        "stubborn descendant tree",
    );
    let tree_pids = pids(&tree);
    host.detach(source); // worker lease must notice even without a UI poll
    wait(
        || tree_pids.iter().copied().all(gone),
        Duration::from_secs(4),
        "revoked tree reaping",
    );
    let result = poll_until(&mut runtime, &mut host, channel, Duration::from_secs(2));
    assert_eq!(result.outcome, Outcome::Uncertain(Reason::Revoked));
    assert!(result.reaped);
    runtime.remove(&mut host, channel);
    let source = host.attach(1).unwrap();
    println!("PASS source generation revocation independently cancels active TERM-ignoring leader/child/grandchild and reaps all of them");

    let old_config = directory(&root, "replaced-old-generation");
    let replacement = directory(&root, "replacement-literal-config");
    let channel = start(
        &mut runtime,
        &mut host,
        &fixture,
        &old_config,
        "tree",
        source,
    );
    wait(
        || pids(&old_config).len() >= 3,
        Duration::from_secs(5),
        "old configuration attempt",
    );
    host.bell(source, Focus::default(), Instant::now()).unwrap(); // queued under old configuration
    runtime
        .replace(&mut host, channel, config(&fixture, &replacement, "record"))
        .unwrap();
    assert_eq!(host.pending_count(Some(channel)), 0);
    let revoked = poll_until(&mut runtime, &mut host, channel, Duration::from_secs(5));
    assert_eq!(revoked.outcome, Outcome::Uncertain(Reason::Revoked));
    assert!(pids(&old_config).into_iter().all(gone));
    assert!(
        pids(&replacement).is_empty(),
        "configuration replacement replayed an old job"
    );
    let new_event = host.bell(source, Focus::default(), Instant::now()).unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        runtime.poll(&mut host, Instant::now());
        if let Some(result) = host
            .command_completions()
            .find(|result| result.job.event() == new_event)
        {
            assert_eq!(result.outcome, Outcome::Exited(0));
            break;
        }
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(10));
    }
    runtime.remove(&mut host, channel);
    let targetless = directory(&root, "targetless");
    let channel = runtime
        .register(&mut host, config(&fixture, &targetless, "record"))
        .unwrap();
    runtime
        .set_policy(
            &mut host,
            channel,
            Policy {
                enabled: true,
                eligibility: Eligibility::OriginatingTerminalUnfocused,
            },
        )
        .unwrap();
    host.application_event(Focus::default(), Instant::now())
        .unwrap();
    runtime.poll(&mut host, Instant::now());
    assert_eq!(host.pending_count(Some(channel)), 0);
    assert!(pids(&targetless).is_empty());
    runtime.set_policy(&mut host, channel, enabled()).unwrap();
    host.application_event(Focus::default(), Instant::now())
        .unwrap();
    assert_eq!(
        poll_until(&mut runtime, &mut host, channel, Duration::from_secs(5)).outcome,
        Outcome::Exited(0)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(paths(&targetless, "payload-").pop().unwrap()).unwrap())
            .unwrap();
    assert_eq!(value["kind"], "application");
    assert!(value["source"].is_null());
    runtime.remove(&mut host, channel);
    let missing = runtime
        .register(
            &mut host,
            Configuration::new(&root.join("not-an-executable"), vec![]).unwrap(),
        )
        .unwrap();
    runtime.set_policy(&mut host, missing, enabled()).unwrap();
    host.bell(source, Focus::default(), Instant::now()).unwrap();
    assert_eq!(
        poll_until(&mut runtime, &mut host, missing, Duration::from_secs(5)).outcome,
        Outcome::NotStarted(Reason::SpawnFailed)
    );
    runtime.remove(&mut host, missing);
    let text = root.join("executable-text-without-interpreter");
    let shell_marker = root.join("NO-IMPLICIT-SHELL");
    fs::write(&text, format!("touch '{}'\n", shell_marker.display())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&text, fs::Permissions::from_mode(0o700)).unwrap();
    let invalid_format = runtime
        .register(&mut host, Configuration::new(&text, vec![]).unwrap())
        .unwrap();
    runtime
        .set_policy(&mut host, invalid_format, enabled())
        .unwrap();
    host.bell(source, Focus::default(), Instant::now()).unwrap();
    assert_eq!(
        poll_until(
            &mut runtime,
            &mut host,
            invalid_format,
            Duration::from_secs(5)
        )
        .outcome,
        Outcome::NotStarted(Reason::SpawnFailed)
    );
    assert!(!shell_marker.exists(), "ENOEXEC implicitly invoked a shell");
    runtime.remove(&mut host, invalid_format);
    println!("PASS configuration generation replacement kills old active work and clears old queued work; new configuration acts only on a new event; targetless policy/JSON and known spawn failure have real boundaries");

    let timeout = directory(&root, "real-ten-second-timeout");
    let started = Instant::now();
    let channel = start(&mut runtime, &mut host, &fixture, &timeout, "hang", source);
    wait(
        || !pids(&timeout).is_empty(),
        Duration::from_secs(5),
        "real hanging command start",
    );
    let result = poll_until(
        &mut runtime,
        &mut host,
        channel,
        DEADLINE + Duration::from_secs(4),
    );
    assert_eq!(result.outcome, Outcome::Uncertain(Reason::Deadline));
    assert!(result.elapsed >= DEADLINE && result.elapsed < DEADLINE + Duration::from_secs(3));
    assert!(started.elapsed() >= DEADLINE);
    assert!(result.reaped && family_gone(&timeout));
    runtime
        .set_policy(&mut host, channel, Policy::DELIVERY_DEFAULT)
        .unwrap();
    runtime.set_policy(&mut host, channel, enabled()).unwrap();
    let no_replay = Instant::now();
    while no_replay.elapsed() < Duration::from_secs(3) {
        runtime.poll(&mut host, Instant::now());
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(paths(&timeout, "started-").len(), 1);
    runtime.remove(&mut host, channel);
    println!("PASS actual ten-second deadline, forced cleanup, uncertain external effects and no automatic retry/re-enable replay ({:?})", result.elapsed);

    let first = directory(&root, "global-first");
    let second = directory(&root, "global-second");
    let third = directory(&root, "global-queued");
    let a = start(&mut runtime, &mut host, &fixture, &first, "hang", source);
    let b = start(&mut runtime, &mut host, &fixture, &second, "hang", source);
    let mut other = new_runtime(&moved_cwd);
    let mut other_host = Host::new().unwrap();
    let other_source = other_host.attach(10).unwrap();
    let c = start(
        &mut other,
        &mut other_host,
        &fixture,
        &third,
        "record",
        other_source,
    );
    wait(
        || !pids(&first).is_empty() && !pids(&second).is_empty(),
        Duration::from_secs(5),
        "two actual concurrent commands",
    );
    for _ in 0..10 {
        runtime.poll(&mut host, Instant::now());
        other.poll(&mut other_host, Instant::now());
        assert_eq!(commands::active_count(), 2);
        assert!(
            pids(&third).is_empty(),
            "global concurrency exceeded across runtimes"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let second_source = host.attach(2).unwrap();
    host.bell(second_source, Focus::default(), Instant::now())
        .unwrap();
    runtime.poll(&mut host, Instant::now());
    assert_eq!(pids(&first).len(), 1, "channel ran two concurrent commands");
    runtime
        .set_policy(&mut host, a, Policy::DELIVERY_DEFAULT)
        .unwrap();
    runtime.remove(&mut host, b);
    let completed = poll_until(&mut other, &mut other_host, c, Duration::from_secs(5));
    assert_eq!(completed.outcome, Outcome::Exited(0));
    assert!(family_gone(&first) && family_gone(&second));
    runtime.remove(&mut host, a);
    other.shutdown(&mut other_host);
    println!("PASS at most two commands globally across runtimes / one per channel; disable/removal cancels active and queued work; another channel progresses");

    let flood = directory(&root, "bounded-fair-flood");
    let channel = runtime
        .register(&mut host, config(&fixture, &flood, "record"))
        .unwrap();
    runtime.set_policy(&mut host, channel, enabled()).unwrap();
    for _ in 0..100_000 {
        host.bell(source, Focus::default(), Instant::now()).unwrap();
    }
    host.bell(second_source, Focus::default(), Instant::now())
        .unwrap();
    assert_eq!(host.pending_count(Some(channel)), 2);
    let until = Instant::now() + Duration::from_secs(5);
    while paths(&flood, "finished-").len() < 2 {
        runtime.poll(&mut host, Instant::now());
        assert!(Instant::now() < until, "quiet source starved");
        assert!(commands::active_count() <= 2);
        std::thread::sleep(Duration::from_millis(10));
    }
    let sources: Vec<_> = paths(&flood, "payload-")
        .into_iter()
        .map(|path| {
            let value: serde_json::Value =
                serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            value["source"]["pane"].as_u64().unwrap()
        })
        .collect();
    assert!(sources.contains(&1) && sources.contains(&2));
    runtime.remove(&mut host, channel);
    wait(
        || commands::active_count() == 0,
        Duration::from_secs(4),
        "flood cleanup",
    );
    let many = directory(&root, "many-sources-and-channels");
    for _ in 0..4 {
        let channel = runtime
            .register(&mut host, config(&fixture, &many, "record"))
            .unwrap();
        runtime.set_policy(&mut host, channel, enabled()).unwrap();
    }
    for pane in 100..200 {
        let source = host.attach(pane).unwrap();
        host.bell(source, Focus::default(), Instant::now()).unwrap();
    }
    assert_eq!(host.pending_count(None), 64);
    let end = Instant::now() + Duration::from_secs(20);
    while host.pending_count(None) != 0 || commands::active_count() != 0 {
        runtime.poll(&mut host, Instant::now());
        assert!(host.pending_count(None) <= 64 && commands::active_count() <= 2);
        assert!(host.diagnostics().len() <= 32);
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(10));
    }
    runtime.poll(&mut host, Instant::now());
    assert_eq!(paths(&many, "finished-").len(), 64);
    assert_eq!(host.diagnostics().len(), 32);
    assert!(host
        .command_completions()
        .all(|result| result.outcome == Outcome::Exited(0)));
    let count = host.event_count();
    runtime.shutdown(&mut host);
    runtime.poll(&mut host, Instant::now());
    assert_eq!(
        host.event_count(),
        count,
        "completion diagnostics recursively notified"
    );
    assert_eq!(host.pending_count(None), 0);
    println!("PASS 100000-event source flood coalesces and quiet source executes; global64/per-channel16 queue drains through real processes, shared diagnostic history <=32, no diagnostic recursion");

    let drop_output = directory(&root, "drop-active-and-pending");
    let mut dropped = new_runtime(&moved_cwd);
    let mut drop_host = Host::new().unwrap();
    let source = drop_host.attach(999).unwrap();
    let channel = start(
        &mut dropped,
        &mut drop_host,
        &fixture,
        &drop_output,
        "tree",
        source,
    );
    wait(
        || pids(&drop_output).len() >= 3,
        Duration::from_secs(5),
        "drop's active tree",
    );
    drop_host
        .bell(source, Focus::default(), Instant::now())
        .unwrap();
    assert_eq!(drop_host.pending_count(Some(channel)), 1);
    let start = Instant::now();
    drop(dropped);
    assert!(start.elapsed() < Duration::from_millis(50));
    assert_eq!(drop_host.pending_count(Some(channel)), 0);
    wait(
        || commands::active_count() == 0 && family_gone(&drop_output),
        Duration::from_secs(4),
        "drop revocation and complete helper/tree reaping",
    );
    assert!(drop_host.take_ready(channel, Instant::now()).is_none());
    assert!(drop_host.set_channel_policy(channel, enabled()).is_err());
    let io_threads = fs::read_dir("/proc/self/task")
        .unwrap()
        .filter(|entry| {
            fs::read_to_string(entry.as_ref().unwrap().path().join("comm"))
                .unwrap_or_default()
                .starts_with("among-command")
        })
        .count();
    assert_eq!(io_threads, 1, "per-attempt IO workers accumulated");
    println!("PASS dropping the runtime immediately revokes active and queued work without joining; helper/leader/descendants all disappear from /proc; one shared IO thread after many attempts, not one abandoned worker per command");
}
