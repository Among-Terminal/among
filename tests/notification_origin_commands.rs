use among::notifications::{
    commands::{self, Configuration, Outcome, OwnedDirectory, Reason, Runtime},
    Eligibility, Event, Focus, Host, Policy, Source,
};
use among::plugin_notifications::{
    Backend, EventInput, OriginLease, Owner, Target, EVENT_LIFETIME,
};
use std::{
    cell::RefCell,
    fs,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[path = "support/notification_command.rs"]
mod support;

fn directory(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    path
}

fn files(root: &Path, prefix: &str) -> Vec<PathBuf> {
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
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct Case {
    core: Rc<RefCell<Host>>,
    backend: Backend,
    runtime: Runtime,
    owner: Owner,
    lease: OriginLease,
    source: Source,
}

impl Case {
    fn new(root: &Path, fixture: &Path, mode: &str) -> Self {
        Self::with_helper(root, fixture, mode, Path::new(env!("CARGO_BIN_EXE_among")))
    }

    fn with_helper(root: &Path, fixture: &Path, mode: &str, helper: &Path) -> Self {
        let core = Rc::new(RefCell::new(Host::new().unwrap()));
        let source = core.borrow_mut().attach(1).unwrap();
        let mut backend = Backend::new(core.clone(), Focus::default());
        assert!(backend.bind("command-origin-test"));
        let owner = Owner {
            identity: "github:12/sample".into(),
            generation: "producer-generation".into(),
        };
        backend.register_identity(&owner.identity).unwrap();
        let lease = backend.admit(owner.clone(), false, None).unwrap();
        let mut runtime = Runtime::new(helper, OwnedDirectory::open(root).unwrap()).unwrap();
        let channel = runtime
            .register(
                &mut core.borrow_mut(),
                Configuration::new(fixture, vec![mode.into(), root.display().to_string()]).unwrap(),
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
            runtime,
            owner,
            lease,
            source,
        }
    }

    fn produce(&mut self, source: Option<Source>, now: Instant) -> Event {
        self.backend
            .produce(
                &self.owner,
                EventInput {
                    sequence: 1,
                    target: source.map_or(Target::Application, |source| Target::Pane {
                        reference: source.pane(),
                    }),
                    source_generation: source.map(|source| source.generation().to_string()),
                    message: "Synthetic event text must not enter command payload".into(),
                    causal_job: None,
                },
                Some(10),
                now,
            )
            .unwrap()
    }

    fn poll(&mut self, now: Instant) {
        self.runtime.poll_authorized(&self.core, now, |job| {
            self.backend.authorize_delivery(job, now)
        });
    }

    fn completion(&mut self, event: Event) -> commands::Completion {
        let mut result = None;
        wait(
            || {
                self.poll(Instant::now());
                result = self
                    .core
                    .borrow()
                    .command_completions()
                    .find(|completion| completion.job.event() == event)
                    .copied();
                result.is_some()
            },
            "source-bound command completion",
        );
        result.unwrap()
    }
}

#[test]
fn real_managed_commands_require_owned_authority_and_revoke_without_gui_polling() {
    let parent = std::env::var_os("AMONG_COMMAND_TEST_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("among-notification-origin-commands"));
    fs::create_dir_all(&parent).unwrap();
    let root = directory(
        &parent,
        &format!(
            "origin-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ),
    );
    println!("origin command evidence: {}", root.display());
    let fixture = support::compile_fixture(&root);
    for targeted in [false, true] {
        let output = directory(&root, if targeted { "pane" } else { "targetless" });
        let mut case = Case::new(&output, &fixture, "record");
        let source = targeted.then_some(case.source);
        let event = case.produce(source, Instant::now());
        let result = case.completion(event);
        assert_eq!(result.outcome, Outcome::Exited(0));
        assert!(result.reaped);
        let payload: serde_json::Value =
            serde_json::from_slice(&fs::read(files(&output, "payload-").pop().unwrap()).unwrap())
                .unwrap();
        assert_eq!(payload["kind"], "managed-notification");
        assert_eq!(
            payload["origin"],
            serde_json::json!({"identity":case.owner.identity,"generation":case.owner.generation})
        );
        assert_eq!(payload["event"]["id"], event.id());
        assert_eq!(payload["event"]["session"], event.session());
        assert_eq!(payload["source"].is_null(), !targeted);
        if targeted {
            assert_eq!(payload["source"]["generation"], case.source.generation());
        }
        assert!(payload.get("message").is_none());
        assert_eq!(payload.as_object().unwrap().len(), 5);
        assert_eq!(files(&output, "eof-").len(), 1);
    }
    println!("PASS actual pane and targetless managed payloads retain authoritative kind/origin and bounded identity-only JSON with real EOF");

    for denial in ["host-only", "missing", "expired", "revoked-before-frame"] {
        let output = directory(&root, denial);
        let mut case = Case::new(&output, &fixture, "record");
        let now = Instant::now();
        let event = case.produce(None, now);
        match denial {
            "host-only" => case.runtime.poll(&mut case.core.borrow_mut(), now),
            "missing" => case.runtime.poll_authorized(&case.core, now, |_| None),
            "expired" => case.poll(now + EVENT_LIFETIME),
            "revoked-before-frame" => case.runtime.poll_authorized(&case.core, now, |job| {
                let delivery = case.backend.authorize_delivery(job, now);
                case.lease.revoke();
                delivery
            }),
            _ => unreachable!(),
        }
        let result = case.completion(event);
        assert_eq!(
            result.outcome,
            Outcome::NotStarted(Reason::Revoked),
            "{denial}"
        );
        assert!(
            files(&output, "started-").is_empty(),
            "{denial} executed code"
        );
        assert_eq!(case.core.borrow().pending_count(None), 0);
        case.poll(Instant::now() + Duration::from_secs(3));
        assert!(files(&output, "started-").is_empty(), "{denial} replayed");
    }
    println!("PASS host-only/missing/expired/revoked authorization fails closed without spawn, host-origin fallback or replay");

    let output = directory(&root, "revoke-before-real-supervisor-grant");
    let helper_source = output.join("delayed-helper.rs");
    let helper = output.join("delayed-helper");
    let observed = output.join("spawn-observed");
    let release = output.join("release");
    fs::write(&helper_source, format!(r#"
        use std::os::unix::process::CommandExt;
        extern "C" {{ fn signal(signal:i32, handler:usize)->usize; }}
        fn main() {{
            unsafe {{ signal(15,1); }}
            std::fs::write({observed:?}, b"actual helper process reached its entrypoint").unwrap();
            let deadline = std::time::Instant::now()+std::time::Duration::from_secs(3);
            while !std::path::Path::new({release:?}).exists() {{
                assert!(std::time::Instant::now()<deadline);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }}
            let error = std::process::Command::new({actual:?}).args(std::env::args_os().skip(1)).exec();
            panic!("actual supervisor exec failed: {{error}}");
        }}
    "#, actual=env!("CARGO_BIN_EXE_among"))).unwrap();
    let built = std::process::Command::new("rustc")
        .args(["--edition=2021"])
        .arg(&helper_source)
        .arg("-o")
        .arg(&helper)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "delayed helper fixture: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    let mut case = Case::with_helper(&output, &fixture, "record", &helper);
    let event = case.produce(None, Instant::now());
    case.poll(Instant::now());
    wait(
        || observed.is_file(),
        "real delayed helper spawn observation",
    );
    case.lease.revoke();
    fs::write(&release, b"continue into unchanged production supervisor").unwrap();
    let completion = case.completion(event);
    assert_eq!(completion.outcome, Outcome::NotStarted(Reason::Revoked));
    assert!(completion.reaped);
    assert!(
        files(&output, "started-").is_empty(),
        "revoked origin received actual execution permission after helper spawn"
    );
    println!("PASS real delayed helper spawn then origin revocation refuses the unchanged supervisor grant and produces no configured-command effect");

    let output = directory(&root, "noisy-and-quiet-origins");
    let mut case = Case::new(&output, &fixture, "record");
    let now = Instant::now();
    let noisy = case.produce(None, now);
    for sequence in 2..100_002 {
        assert_eq!(
            case.backend.produce(
                &case.owner,
                EventInput {
                    sequence,
                    target: Target::Application,
                    source_generation: None,
                    message: "Synthetic noise".into(),
                    causal_job: None
                },
                None,
                now
            ),
            Err(among::plugin_notifications::Rejection::RateLimited)
        );
        assert!(case.core.borrow().pending_count(None) <= 16);
    }
    let quiet_owner = Owner {
        identity: "github:13/sample".into(),
        generation: "quiet-generation".into(),
    };
    case.backend
        .register_identity(&quiet_owner.identity)
        .unwrap();
    let _quiet_lease = case
        .backend
        .admit(quiet_owner.clone(), false, None)
        .unwrap();
    let quiet = case
        .backend
        .produce(
            &quiet_owner,
            EventInput {
                sequence: 1,
                target: Target::Application,
                source_generation: None,
                message: "Synthetic quiet event".into(),
                causal_job: None,
            },
            None,
            Instant::now(),
        )
        .unwrap();
    assert_eq!(case.core.borrow().pending_count(None), 2);
    assert_eq!(case.completion(noisy).outcome, Outcome::Exited(0));
    assert_eq!(case.completion(quiet).outcome, Outcome::Exited(0));
    let origins: std::collections::BTreeSet<_> = files(&output, "payload-")
        .into_iter()
        .map(|path| {
            let payload: serde_json::Value =
                serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            payload["origin"]["identity"].as_str().unwrap().to_string()
        })
        .collect();
    assert_eq!(
        origins,
        [case.owner.identity.clone(), quiet_owner.identity]
            .into_iter()
            .collect()
    );
    assert_eq!(case.core.borrow().pending_count(None), 0);
    println!("PASS 100000 noisy managed attempts do not replace or cooldown-block another targetless origin; both real commands receive their own identity and finish");

    for revoke in ["origin", "source", "channel", "session"] {
        let output = directory(&root, &format!("active-{revoke}"));
        let mut case = Case::new(&output, &fixture, "hang");
        let event = case.produce(Some(case.source), Instant::now());
        case.poll(Instant::now());
        wait(
            || !files(&output, "started-").is_empty(),
            "real effect before independent revocation",
        );
        let job = case.core.borrow().pending_count(None);
        assert_eq!(job, 0);
        match revoke {
            "origin" => case.lease.revoke(),
            "source" => case.core.borrow_mut().detach(case.source),
            "channel" => case.runtime.shutdown(&mut case.core.borrow_mut()),
            "session" => *case.core.borrow_mut() = Host::new().unwrap(),
            _ => unreachable!(),
        }
        // Deliberately no Runtime/Backend/UI poll: the service must own and read
        // the live flags rather than depend on the next GTK shared tick.
        wait(
            || {
                files(&output, "started-")
                    .iter()
                    .chain(files(&output, "supervisor-").iter())
                    .all(|path| {
                        !Path::new(&format!("/proc/{}", fs::read_to_string(path).unwrap())).exists()
                    })
            },
            "worker-side authority revokes and reaps actual effect",
        );
        let result = case.completion(event);
        assert!(
            matches!(
                result.outcome,
                Outcome::Uncertain(Reason::Revoked | Reason::Cancelled)
            ),
            "{revoke}: {:?}",
            result.outcome
        );
        assert!(result.reaped);
    }
    println!("PASS actual running commands stop and reap on independent origin/source/channel/session revocation without a GUI tick; external effects are not rolled back");
}
