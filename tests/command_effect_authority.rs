use among::notifications::{
    commands::{self, Configuration, Outcome, OwnedDirectory, Reason, Runtime},
    Authorization, Channel, EffectAuthorization, Eligibility, Event, Focus, Host, Policy,
    ProcessEffectAuthorization, Source,
};
use among::plugin_notifications::Backend;
use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::{fs::DirBuilderExt, net::UnixStream, process::CommandExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
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

fn wait(mut condition: impl FnMut() -> bool, description: &str) {
    let end = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < end, "timed out: {description}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

struct Reader {
    child: Child,
    input: UnixStream,
    output: BufReader<UnixStream>,
    effect: UnixStream,
    log: File,
}

impl Reader {
    fn new(process: &ProcessEffectAuthorization, root: &Path) -> Self {
        let descriptors = process.duplicate_fds().unwrap();
        let binding: String = process
            .binding()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        fs::write(root.join("binding.bin"), process.binding()).unwrap();
        let (input, child_input) = UnixStream::pair().unwrap();
        input
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        input
            .set_write_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        let output = BufReader::new(input.try_clone().unwrap());
        let (effect, child_effect) = UnixStream::pair().unwrap();
        effect
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let raw: Vec<_> = descriptors
            .as_slice()
            .iter()
            .map(AsRawFd::as_raw_fd)
            .collect();
        let list = raw
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let effect_raw = child_effect.as_raw_fd();
        let executable = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("examples/effect-authority-helper");
        assert!(
            executable.is_file(),
            "build the unchanged real effect helper first"
        );
        let log = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(root.join("exec-reader.log"))
            .unwrap();
        let mut command = Command::new(executable);
        command
            .args([binding, list, effect_raw.to_string()])
            .stdin(Stdio::from(OwnedFd::from(child_input.try_clone().unwrap())))
            .stdout(Stdio::from(OwnedFd::from(child_input)))
            .stderr(Stdio::from(log.try_clone().unwrap()));
        unsafe {
            command.pre_exec(move || {
                for fd in raw.iter().copied().chain(std::iter::once(effect_raw)) {
                    if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        // the bounded wrapper, not a cache of raw duplicate numbers, owns spawn.
        drop(descriptors);
        drop(child_effect);
        let mut reader = Self {
            child,
            input,
            output,
            effect,
            log,
        };
        assert!(reader.read().starts_with("READY "));
        assert_eq!(reader.send("exec-check"), "SCANNER CLOSED");
        assert_eq!(reader.send("check"), "CURRENT true");
        reader
    }

    fn read(&mut self) -> String {
        let mut line = String::new();
        assert!(self.output.read_line(&mut line).unwrap() > 0);
        self.log.write_all(line.as_bytes()).unwrap();
        line.trim_end().to_owned()
    }

    fn send(&mut self, message: &str) -> String {
        writeln!(self.input, "{message}").unwrap();
        self.read()
    }

    fn refused(&mut self) {
        assert_eq!(self.send("check"), "CURRENT false");
        assert_eq!(self.send("play"), "DENIED");
        let mut byte = [0];
        match self.effect.read(&mut byte) {
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            other => panic!("revoked migrated Runtime emitted a synthetic effect: {other:?}"),
        }
    }

    fn positive(&mut self) {
        assert_eq!(self.send("play"), "PLAYED");
        let mut byte = [0];
        self.effect.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [1]);
        assert_eq!(self.send("play"), "DENIED");
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        let _ = writeln!(self.input, "quit");
        assert!(self.child.wait().unwrap().success());
    }
}

struct Case {
    core: Rc<RefCell<Host>>,
    backend: Backend,
    runtime: Option<Runtime>,
    source: Source,
    channel: Channel,
}

impl Case {
    fn new(root: &Path, executable: &Path, mode: &str) -> Self {
        let core = Rc::new(RefCell::new(Host::new().unwrap()));
        let source = core.borrow_mut().attach(17).unwrap();
        let mut backend = Backend::new(core.clone(), Focus::default());
        assert!(backend.bind("actual-runtime-effect-test"));
        let mut runtime = Runtime::new(
            Path::new(env!("CARGO_BIN_EXE_among")),
            OwnedDirectory::open(root).unwrap(),
        )
        .unwrap();
        let channel = runtime
            .register(
                &mut core.borrow_mut(),
                Configuration::new(executable, vec![mode.into(), root.display().to_string()])
                    .unwrap(),
            )
            .unwrap();
        runtime
            .set_policy(
                &mut core.borrow_mut(),
                channel,
                Policy {
                    enabled: true,
                    eligibility: Eligibility::Always,
                },
            )
            .unwrap();
        Self {
            core,
            backend,
            runtime: Some(runtime),
            source,
            channel,
        }
    }

    fn issue(&mut self) -> (Event, Authorization, EffectAuthorization) {
        let now = Instant::now();
        let event = self
            .core
            .borrow_mut()
            .bell(self.source, Focus::default(), now)
            .unwrap();
        self.backend
            .observe_host_event(event, Some(10), Focus::default())
            .unwrap();
        let job = self
            .core
            .borrow()
            .pending_jobs(self.channel)
            .find(|job| job.event() == event)
            .unwrap();
        let authorization = self.core.borrow().authorization(job).unwrap();
        let delivery = self.backend.authorize_delivery(job, now).unwrap();
        let effect = delivery.effect_authorization(&authorization, now).unwrap();
        (event, authorization, effect)
    }

    fn poll(&mut self) {
        let now = Instant::now();
        self.runtime
            .as_mut()
            .unwrap()
            .poll_authorized(&self.core, now, |job| {
                self.backend.authorize_delivery(job, now)
            });
    }

    fn completion(&mut self, event: Event) -> commands::Completion {
        let mut completion = None;
        wait(
            || {
                self.poll();
                completion = self
                    .core
                    .borrow()
                    .command_completions()
                    .find(|result| result.job.event() == event)
                    .copied();
                completion.is_some()
            },
            "actual command service completion",
        );
        completion.unwrap()
    }
}

#[test]
fn migrated_runtime_remove_and_drop_revoke_exec_readers_and_actual_service_without_owner_poll() {
    let parent = std::env::var_os("AMONG_COMMAND_TEST_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    fs::create_dir_all(&parent).unwrap();
    let root = directory(
        &parent,
        &format!(
            "actual-runtime-effect-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ),
    );
    println!(
        "actual Runtime/service + exec reader evidence: {}",
        root.display()
    );
    let executable = support::compile_fixture(&root);
    for mutation in ["remove", "drop-borrowed", "remove-without-owner-cleanup"] {
        let output = directory(&root, mutation);
        let mut case = Case::new(&output, &executable, "hang");
        let (_, authorization, effect) = case.issue();
        let process = effect.export_process().unwrap();
        let mut reader = Reader::new(&process, &directory(&output, "old-reader"));
        wait(
            || {
                case.poll();
                !paths(&output, "started-").is_empty()
            },
            "real Runtime dispatched stubborn command before removal",
        );
        let pids: Vec<u32> = paths(&output, "started-")
            .into_iter()
            .chain(paths(&output, "supervisor-"))
            .map(|path| fs::read_to_string(path).unwrap().parse().unwrap())
            .collect();
        assert_eq!(pids.len(), 2);
        assert!(authorization.is_current());
        assert!(effect.is_current(Instant::now()));
        if mutation == "remove" {
            case.runtime
                .as_mut()
                .unwrap()
                .remove(&mut case.core.borrow_mut(), case.channel);
        } else {
            let core = case.core.clone();
            let held = core.borrow();
            if mutation == "drop-borrowed" {
                drop(case.runtime.take());
            } else {
                // a foreign cleanup core cannot revoke this job: this isolates
                // Runtime::remove's real Registration writer, not Host::remove.
                let mut unrelated = Host::new().unwrap();
                case.runtime
                    .as_mut()
                    .unwrap()
                    .remove(&mut unrelated, case.channel);
            }
            assert!(!held.is_current(effect.job()));
        }
        assert!(!authorization.is_current());
        assert!(!effect.is_current(Instant::now()));
        reader.refused();
        // no Runtime/Backend/GUI propagation poll after the writer. the owned
        // service must retire and retain its reservation through actual reap.
        wait(
            || {
                commands::active_count() == 0
                    && pids
                        .iter()
                        .all(|pid| !Path::new(&format!("/proc/{pid}")).exists())
            },
            "direct registration writer -> actual service reap without owner poll",
        );
        assert!(case.core.borrow().is_live(case.source));
        println!("PASS actual Runtime {mutation}: core flag/retained capability/exec reader revoked; service processes reaped without owner propagation");
        let recovery = directory(&output, "recovery");
        let mut fresh = Case::new(&recovery, &executable, "record");
        let (event, _, effect) = fresh.issue();
        let mut reader = Reader::new(
            &effect.export_process().unwrap(),
            &directory(&recovery, "reader"),
        );
        reader.positive();
        let completion = fresh.completion(event);
        assert_eq!(completion.outcome, Outcome::Exited(0));
        assert!(completion.reaped);
        assert_eq!(paths(&recovery, "eof-").len(), 1);
        let payload: serde_json::Value =
            serde_json::from_slice(&fs::read(paths(&recovery, "payload-").pop().unwrap()).unwrap())
                .unwrap();
        assert_eq!(payload["kind"], "terminal-bell");
        assert_eq!(payload["source"]["pane"], 17);
        println!("PASS separate explicit fresh Runtime {mutation} recovery: actual stdin/EOF/Exited(0), one synthetic byte and denied replay");
    }
    let output = directory(&root, "spawn-failure-recovery");
    let mut case = Case::new(&output, &output.join("does-not-exist"), "record");
    let (event, old, _) = case.issue();
    let completion = case.completion(event);
    assert_eq!(completion.outcome, Outcome::NotStarted(Reason::SpawnFailed));
    assert!(completion.reaped && paths(&output, "started-").is_empty());
    case.runtime
        .as_mut()
        .unwrap()
        .replace(
            &mut case.core.borrow_mut(),
            case.channel,
            Configuration::new(
                &executable,
                vec!["record".into(), output.display().to_string()],
            )
            .unwrap(),
        )
        .unwrap();
    assert!(!old.is_current());
    case.poll();
    assert!(
        paths(&output, "started-").is_empty(),
        "configuration replacement replayed failed work"
    );
    let (event, _, _) = case.issue();
    assert_eq!(case.completion(event).outcome, Outcome::Exited(0));
    assert_eq!(paths(&output, "eof-").len(), 1);
    println!("PASS actual Runtime service spawn refusal, replacement revocation/no replay and explicit fresh stdin/EOF recovery");
}
