use among::notifications::{
    self as core, EffectAuthorityError, EffectAuthorization, Eligibility, Focus, Host, Policy,
    ProcessEffectAuthorization, Registration, Source,
};
use among::plugin_notifications::{Backend, DeliveryAuthorization, EventInput, Owner, Target};
use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::{net::UnixStream, process::CommandExt};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Barrier,
};
use std::time::{Duration, Instant};

static NEXT: AtomicU64 = AtomicU64::new(0);
fn owner() -> Owner {
    Owner {
        identity: "github:12/effect".into(),
        generation: "issued-generation".into(),
    }
}
fn evidence(name: &str) -> PathBuf {
    let root = std::env::var_os("AMONG_CHECK_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/effect-authority-evidence")
        });
    let root = root.join(format!(
        "effect-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}
struct Fixture {
    core: Rc<RefCell<Host>>,
    backend: Option<Backend>,
    source: Source,
    channel: core::Channel,
    registration: Option<Registration>,
    origin: core::OriginLease,
    job: core::Job,
    delivery: DeliveryAuthorization,
    effect: EffectAuthorization,
}
impl Fixture {
    fn new(managed: bool, targeted: bool, received: Instant) -> Self {
        let core = Rc::new(RefCell::new(Host::new().unwrap()));
        let source = core.borrow_mut().attach(17).unwrap();
        let channel = core
            .borrow_mut()
            .register_channel(Policy {
                enabled: true,
                eligibility: Eligibility::Always,
            })
            .unwrap();
        let registration = Some(core.borrow().registration(channel).unwrap());
        let mut backend = Backend::new(core.clone(), Focus::default());
        assert!(backend.bind("effect-session"));
        backend.register_identity(&owner().identity).unwrap();
        let origin = backend.admit(owner(), false, None).unwrap();
        let event = if managed {
            backend
                .produce(
                    &owner(),
                    EventInput {
                        sequence: 1,
                        target: if targeted {
                            Target::Pane { reference: 17 }
                        } else {
                            Target::Application
                        },
                        source_generation: targeted.then(|| source.generation().to_string()),
                        message: "synthetic event".into(),
                        causal_job: None,
                    },
                    targeted.then_some(10),
                    received,
                )
                .unwrap()
        } else {
            let event = if targeted {
                core.borrow_mut()
                    .bell(source, Focus::default(), received)
                    .unwrap()
            } else {
                core.borrow_mut()
                    .application_event(Focus::default(), received)
                    .unwrap()
            };
            backend
                .observe_host_event(event, targeted.then_some(10), Focus::default())
                .unwrap();
            event
        };
        let job = core.borrow_mut().take_ready(channel, received).unwrap();
        assert_eq!(job.event(), event);
        let delivery = backend.authorize_delivery(job, received).unwrap();
        let authorization = core.borrow().authorization(job).unwrap();
        let effect = delivery
            .effect_authorization(&authorization, received)
            .unwrap();
        Self {
            core,
            backend: Some(backend),
            source,
            channel,
            registration,
            origin,
            job,
            delivery,
            effect,
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
struct Helper {
    child: Child,
    input: UnixStream,
    output: BufReader<UnixStream>,
    effect: UnixStream,
    log: File,
}
impl Helper {
    fn spawn(binding: &[u8], fds: impl AsRef<[OwnedFd]>, name: &str) -> (Self, String) {
        let root = evidence(name);
        std::fs::write(root.join("binding.bin"), binding).unwrap();
        let log = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(root.join("helper.log"))
            .unwrap();
        let helper = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("examples/effect-authority-helper");
        assert!(
            helper.is_file(),
            "build --example effect-authority-helper first"
        );
        let (input, socket) = UnixStream::pair().unwrap();
        input
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        input
            .set_write_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        let output = BufReader::new(input.try_clone().unwrap());
        let (effect, effect_child) = UnixStream::pair().unwrap();
        effect
            .set_read_timeout(Some(Duration::from_millis(30)))
            .unwrap();
        let raw: Vec<_> = fds.as_ref().iter().map(AsRawFd::as_raw_fd).collect();
        let list = raw
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let effect_raw = effect_child.as_raw_fd();
        let mut command = Command::new(helper);
        command
            .arg(hex(binding))
            .arg(list)
            .arg(effect_raw.to_string())
            .stdin(Stdio::from(OwnedFd::from(socket.try_clone().unwrap())))
            .stdout(Stdio::from(OwnedFd::from(socket)))
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
        drop(fds);
        drop(effect_child);
        let mut helper = Self {
            child,
            input,
            output,
            effect,
            log,
        };
        let line = helper.read();
        println!("helper evidence: {}", root.display());
        (helper, line)
    }
    fn new(capability: &ProcessEffectAuthorization, name: &str) -> Self {
        let (helper, ready) = Self::spawn(
            capability.binding(),
            capability.duplicate_fds().unwrap(),
            name,
        );
        assert!(ready.starts_with("READY "), "{ready}");
        assert!(ready.contains("read-only/cloexec"));
        helper
    }
    fn read(&mut self) -> String {
        let mut line = String::new();
        assert!(
            self.output.read_line(&mut line).unwrap() > 0,
            "helper exited without response"
        );
        self.log.write_all(line.as_bytes()).unwrap();
        line.trim_end().to_owned()
    }
    fn send(&mut self, message: &str) -> String {
        writeln!(self.input, "{message}").unwrap();
        self.read()
    }
    fn silent(&mut self) {
        let mut byte = [0];
        match self.effect.read(&mut byte) {
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) => {}
            Ok(0) => (),
            other => panic!("unexpected synthetic effect: {other:?}"),
        }
    }
    fn positive(&mut self) {
        assert_eq!(self.send("exec-check"), "SCANNER CLOSED");
        assert_eq!(self.send("check"), "CURRENT true");
        assert_eq!(self.send("play"), "PLAYED");
        let mut byte = [0];
        self.effect.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [1]);
        assert_eq!(
            self.send("play"),
            "DENIED",
            "one operation claim is not replayable"
        );
    }
}

#[test]
fn revoke_between_precheck_and_claim_or_between_claim_and_final_validation_never_plays() {
    for after_claim in [false, true] {
        let fixture = Fixture::new(true, true, Instant::now());
        let process = fixture.effect.export_process().unwrap();
        let mut helper = Helper::new(&process, "claim-ordering");
        assert_eq!(helper.send("check"), "CURRENT true");
        if after_claim {
            assert_eq!(helper.send("claim"), "CLAIMED");
        }
        fixture.core.borrow_mut().detach(fixture.source);
        assert_eq!(
            helper.send(if after_claim { "finish" } else { "play" }),
            "DENIED"
        );
        assert_eq!(
            helper.send("play"),
            "DENIED",
            "failed claim must not be regranted"
        );
        helper.silent();
    }
    let fixture = Fixture::new(true, true, Instant::now());
    let process = fixture.effect.export_process().unwrap();
    let mut helper = Helper::new(&process, "after-start-no-rollback");
    helper.positive();
    fixture.origin.revoke();
    assert_eq!(helper.send("check"), "CURRENT false");
    assert_eq!(helper.send("play"), "DENIED");
    helper.silent(); // the earlier one-byte effect is not described as undone
}

#[test]
fn privately_issued_snapshot_survives_cache_eviction_but_not_origin_revoke_or_expiry() {
    let mut fixture = Fixture::new(true, true, Instant::now());
    let core_authorization = fixture.core.borrow().authorization(fixture.job).unwrap();
    let now = Instant::now();
    for pane in 100..180 {
        let source = fixture.core.borrow_mut().attach(pane).unwrap();
        let event = fixture
            .core
            .borrow_mut()
            .bell(source, Focus::default(), now)
            .unwrap();
        fixture
            .backend
            .as_mut()
            .unwrap()
            .observe_host_event(event, Some(10), Focus::default())
            .unwrap();
    }
    assert!(fixture
        .backend
        .as_ref()
        .unwrap()
        .envelope(fixture.job.event(), now)
        .is_none());
    let effect = fixture
        .delivery
        .effect_authorization(&core_authorization, now)
        .unwrap();
    Helper::new(&effect.export_process().unwrap(), "owned-after-eviction").positive();
    fixture.origin.revoke();
    assert!(matches!(
        fixture
            .delivery
            .effect_authorization(&core_authorization, Instant::now()),
        Err(EffectAuthorityError::Revoked)
    ));
}
impl Drop for Helper {
    fn drop(&mut self) {
        let _ = writeln!(self.input, "quit");
        for _ in 0..20 {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = writeln!(self.log, "retired owned synthetic helper after timeout");
    }
}

#[test]
fn each_scope_revokes_an_already_execd_helper_without_any_backend_or_supervisor_poll() {
    for mutation in [
        "source-detach",
        "source-reuse",
        "policy-off-on",
        "configuration",
        "channel-remove",
        "registration-cancel",
        "registration-drop",
        "host-session-drop",
        "origin-direct",
        "origin-replacement",
        "origin-retire",
        "backend-revoke-borrowed-core",
        "backend-uninstall",
        "backend-drop",
    ] {
        let mut fixture = Fixture::new(true, true, Instant::now());
        let process = fixture.effect.export_process().unwrap();
        assert_eq!(process.duplicate_fds().unwrap().len(), 6);
        assert!(process.binding().len() <= core::MAX_EFFECT_BINDING);
        let mut helper = Helper::new(&process, mutation);
        assert_eq!(helper.send("check"), "CURRENT true");
        match mutation {
            "source-detach" => fixture.core.borrow_mut().detach(fixture.source),
            "source-reuse" => {
                fixture.core.borrow_mut().attach(17).unwrap();
            }
            "policy-off-on" => {
                fixture
                    .core
                    .borrow_mut()
                    .set_channel_policy(fixture.channel, Policy::DELIVERY_DEFAULT)
                    .unwrap();
                fixture
                    .core
                    .borrow_mut()
                    .set_channel_policy(
                        fixture.channel,
                        Policy {
                            enabled: true,
                            eligibility: Eligibility::Always,
                        },
                    )
                    .unwrap();
            }
            "configuration" => fixture
                .core
                .borrow_mut()
                .cancel_channel_work(fixture.channel)
                .unwrap(),
            "channel-remove" => {
                fixture.core.borrow_mut().remove_channel(fixture.channel);
                fixture
                    .core
                    .borrow_mut()
                    .register_channel(Policy {
                        enabled: true,
                        eligibility: Eligibility::Always,
                    })
                    .unwrap();
            }
            "registration-cancel" => fixture.registration.as_ref().unwrap().revoke(),
            "registration-drop" => drop(fixture.registration.take()),
            "host-session-drop" => {
                fixture.core.replace(Host::new().unwrap());
            }
            "origin-direct" => {
                let borrowed = fixture.backend.as_ref().unwrap();
                let lease = fixture.origin.clone();
                std::thread::spawn(move || lease.revoke()).join().unwrap();
                assert!(borrowed
                    .envelope(fixture.job.event(), Instant::now())
                    .is_none());
            }
            "origin-replacement" => {
                let replacement = Owner {
                    identity: owner().identity,
                    generation: "replacement".into(),
                };
                fixture
                    .backend
                    .as_mut()
                    .unwrap()
                    .admit(replacement, false, None)
                    .unwrap();
            }
            "origin-retire" => fixture
                .core
                .borrow_mut()
                .retire_origin(fixture.job.event().origin().unwrap()),
            "backend-revoke-borrowed-core" => {
                let held = fixture.core.borrow();
                fixture.backend.as_mut().unwrap().revoke(&owner().identity);
                assert!(!fixture.origin.is_current());
                drop(held);
            }
            "backend-uninstall" => fixture
                .backend
                .as_mut()
                .unwrap()
                .uninstall(&owner().identity),
            "backend-drop" => drop(fixture.backend.take()),
            _ => unreachable!(),
        }
        // only the specific mutation ran; no backend housekeep, GUI tick, audio
        // supervisor, propagation callback or polling copier caused revocation.
        assert!(!fixture.effect.is_current(Instant::now()));
        assert!(!process.is_current_monotonic(core::monotonic_now_ns().unwrap()));
        assert_eq!(helper.send("check"), "CURRENT false", "{mutation}");
        assert_eq!(helper.send("play"), "DENIED", "{mutation}");
        helper.silent();
        let recovery = Fixture::new(true, true, Instant::now());
        Helper::new(&recovery.effect.export_process().unwrap(), "fresh-recovery").positive();
        assert_eq!(
            helper.send("check"),
            "CURRENT false",
            "a fresh lifetime revived old authority"
        );
    }
}

#[test]
fn host_snapshots_are_explicit_and_managed_targetless_does_not_lose_its_origin() {
    fn send_sync<T: Clone + Send + Sync + 'static>() {}
    send_sync::<EffectAuthorization>();
    for (managed, targeted, scopes) in [
        (false, false, 3),
        (false, true, 4),
        (true, false, 4),
        (true, true, 5),
    ] {
        let fixture = Fixture::new(managed, targeted, Instant::now());
        assert_eq!(fixture.effect.job(), fixture.job);
        assert_eq!(
            fixture.effect.expires_at(),
            fixture.job.event().received_at() + core::EVENT_LIFETIME
        );
        let export = fixture.effect.export_process().unwrap();
        assert_eq!(export.duplicate_fds().unwrap().len(), scopes + 1);
        assert_eq!(export.job_identity().origin.is_some(), managed);
        assert_eq!(export.job_identity().source.is_some(), targeted);
        Helper::new(&export, "positive-kind").positive();
    }
}

#[test]
fn unrelated_valid_core_jobs_and_missing_or_expired_backend_metadata_refuse_issuance() {
    let fixture = Fixture::new(true, true, Instant::now());
    let foreign = Fixture::new(true, true, Instant::now());
    let authorization = foreign.core.borrow().authorization(foreign.job).unwrap();
    assert!(matches!(
        fixture
            .delivery
            .effect_authorization(&authorization, Instant::now()),
        Err(EffectAuthorityError::JobMismatch)
    ));
    let other = fixture
        .core
        .borrow_mut()
        .register_channel(Policy {
            enabled: true,
            eligibility: Eligibility::Always,
        })
        .unwrap();
    let now = Instant::now();
    let unknown = fixture
        .core
        .borrow_mut()
        .application_event(Focus::default(), now)
        .unwrap();
    let unrelated = fixture.core.borrow_mut().take_ready(other, now).unwrap();
    assert_eq!(unrelated.event(), unknown);
    assert!(fixture
        .backend
        .as_ref()
        .unwrap()
        .authorize_delivery(unrelated, now)
        .is_none());
    let authorization = fixture.core.borrow().authorization(unrelated).unwrap();
    assert!(matches!(
        fixture.delivery.effect_authorization(&authorization, now),
        Err(EffectAuthorityError::JobMismatch)
    ));
    let original = fixture.core.borrow().authorization(fixture.job).unwrap();
    assert!(matches!(
        fixture
            .delivery
            .effect_authorization(&original, fixture.effect.expires_at()),
        Err(EffectAuthorityError::Expired)
    ));
}

#[test]
fn original_expiry_is_not_reset_by_export_preparation_or_exec() {
    let received = Instant::now() - Duration::from_secs(9);
    let fixture = Fixture::new(true, false, received);
    let first = fixture.effect.export_process().unwrap();
    let deadline = first.expires_at_monotonic_ns();
    let mut helper = Helper::new(&first, "original-expiry");
    assert_eq!(helper.send("claim"), "CLAIMED");
    std::thread::sleep(Duration::from_millis(50));
    let second = fixture.effect.export_process().unwrap();
    assert_eq!(second.expires_at_monotonic_ns(), deadline);
    assert_eq!(
        fixture.effect.expires_at(),
        received + Duration::from_secs(10)
    );
    let now = core::monotonic_now_ns().unwrap();
    assert!(deadline > now && deadline - now < 1_000_000_000);
    std::thread::sleep(Duration::from_nanos(deadline - now) + Duration::from_millis(5));
    assert!(!first.is_current_monotonic(deadline));
    assert_eq!(helper.send("check"), "CURRENT false");
    assert_eq!(helper.send("finish"), "DENIED");
    assert_eq!(helper.send("play"), "DENIED");
    helper.silent();
    assert!(matches!(
        fixture.effect.export_process(),
        Err(EffectAuthorityError::Expired)
    ));
}

#[test]
fn export_racing_each_direct_revocation_is_never_live_after_the_mutation_completes() {
    for turn in 0..40 {
        let fixture = Fixture::new(true, true, Instant::now());
        let barrier = Arc::new(Barrier::new(2));
        let start = barrier.clone();
        let effect = fixture.effect.clone();
        let worker = std::thread::spawn(move || {
            start.wait();
            effect.export_process()
        });
        barrier.wait();
        match turn % 4 {
            0 => fixture.origin.revoke(),
            1 => fixture.registration.as_ref().unwrap().revoke(),
            2 => fixture.core.borrow_mut().detach(fixture.source),
            _ => fixture
                .core
                .borrow_mut()
                .cancel_channel_work(fixture.channel)
                .unwrap(),
        }
        match worker.join().unwrap() {
            Ok(process) => {
                assert!(!process.is_current_monotonic(core::monotonic_now_ns().unwrap()));
                assert!(process.duplicate_fds().is_err());
            }
            Err(EffectAuthorityError::Revoked) => (),
            other => panic!("unexpected export/revoke result: {other:?}"),
        }
        assert!(!fixture.effect.is_current(Instant::now()));
    }
}

fn duplicate(fd: &OwnedFd) -> OwnedFd {
    fd.try_clone().unwrap()
}
fn readonly_file(size: i64, seals: bool) -> OwnedFd {
    let raw = unsafe {
        libc::memfd_create(
            c"invalid-effect-fixture".as_ptr(),
            libc::MFD_ALLOW_SEALING | libc::MFD_CLOEXEC,
        )
    };
    assert!(raw >= 0);
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    assert_eq!(unsafe { libc::ftruncate(fd.as_raw_fd(), size) }, 0);
    if seals {
        assert_eq!(
            unsafe {
                libc::fcntl(
                    fd.as_raw_fd(),
                    libc::F_ADD_SEALS,
                    libc::F_SEAL_GROW
                        | libc::F_SEAL_SHRINK
                        | libc::F_SEAL_FUTURE_WRITE
                        | libc::F_SEAL_SEAL,
                )
            },
            0
        );
    }
    OpenOptions::new()
        .read(true)
        .open(format!("/proc/self/fd/{}", fd.as_raw_fd()))
        .unwrap()
        .into()
}

#[test]
fn exec_import_rejects_wrong_identity_nonce_deadline_layout_and_descriptor_sets() {
    let fixture = Fixture::new(true, true, Instant::now());
    let process = fixture.effect.export_process().unwrap();
    for (name, offset) in [
        ("magic", 0),
        ("version", 8),
        ("count", 10),
        ("scope-mask", 11),
        ("kind", 12),
        ("reserved", 13),
        ("session", 16),
        ("event", 24),
        ("channel", 32),
        ("revision", 40),
        ("source", 48),
        ("source-generation", 56),
        ("origin", 64),
        ("deadline", 72),
        ("capability-nonce", 80),
        ("manifest-inode", 104),
        ("time-namespace", 120),
        ("scope-order", 128),
        ("scope-nonce", 152),
    ] {
        let mut binding = process.binding().to_vec();
        binding[offset] ^= 1;
        let (mut helper, reply) = Helper::spawn(&binding, process.duplicate_fds().unwrap(), name);
        assert!(reply.starts_with("REFUSED "), "{name}: {reply}");
        helper.silent();
    }
    for name in [
        "missing",
        "extra",
        "reordered",
        "duplicate",
        "unrelated",
        "wrong-size",
        "unsealed",
        "writable",
        "fd-reuse",
    ] {
        let held = process.duplicate_fds().unwrap();
        let mut fds: Vec<_> = held.as_slice().iter().map(duplicate).collect();
        match name {
            "missing" => {
                fds.pop();
            }
            "extra" => fds.push(duplicate(&fds[1])),
            "reordered" => fds.swap(1, 2),
            "duplicate" => fds[2] = duplicate(&fds[1]),
            "unrelated" => fds[1] = File::open("/dev/null").unwrap().into(),
            "wrong-size" => fds[1] = readonly_file(17, true),
            "unsealed" => fds[1] = readonly_file(8192, false),
            "writable" => {
                fds[1] = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(format!("/proc/self/fd/{}", fds[1].as_raw_fd()))
                    .unwrap()
                    .into()
            }
            "fd-reuse" => {
                let fresh = Fixture::new(true, true, Instant::now());
                let fresh = fresh
                    .effect
                    .export_process()
                    .unwrap()
                    .duplicate_fds()
                    .unwrap();
                // replace the object behind each exact old numeric descriptor.
                // old binding must not authorize the reused numbers after exec.
                for (old, replacement) in fds.iter().zip(fresh.as_slice()) {
                    assert_eq!(
                        unsafe {
                            libc::dup3(replacement.as_raw_fd(), old.as_raw_fd(), libc::O_CLOEXEC)
                        },
                        old.as_raw_fd()
                    );
                }
            }
            _ => unreachable!(),
        }
        let (mut helper, reply) = Helper::spawn(process.binding(), fds, name);
        assert!(reply.starts_with("REFUSED "), "{name}: {reply}");
        helper.silent();
    }
    Helper::new(&process, "positive-after-bad-imports").positive();
}

#[test]
fn inherited_scopes_are_read_only_resize_sealed_and_not_writable_via_reopen() {
    let fixture = Fixture::new(true, true, Instant::now());
    let process = fixture.effect.export_process().unwrap();
    let handles = process.duplicate_fds().unwrap();
    for fd in handles.as_slice() {
        assert_ne!(
            unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
        assert_eq!(
            unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) } & libc::O_ACCMODE,
            libc::O_RDONLY
        );
        let writer = OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!("/proc/self/fd/{}", fd.as_raw_fd()))
            .unwrap();
        let byte = [1u8];
        assert_eq!(
            unsafe { libc::pwrite(writer.as_raw_fd(), byte.as_ptr().cast(), 1, 0) },
            -1
        );
        assert_eq!(unsafe { libc::ftruncate(writer.as_raw_fd(), 0) }, -1);
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                4096,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                writer.as_raw_fd(),
                0,
            )
        };
        assert_eq!(mapping, libc::MAP_FAILED);
    }
    assert!(fixture.effect.is_current(Instant::now()));
    Helper::new(&process, "positive-after-read-only-check").positive();
}

#[test]
fn isolated_descriptor_exhaustion_refuses_partial_operations_and_keeps_revoke_infallible() {
    const CHILD: &str = "AMONG_EFFECT_EXHAUSTION_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "isolated_descriptor_exhaustion_refuses_partial_operations_and_keeps_revoke_infallible", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        let root = evidence("exhaustion");
        std::fs::write(root.join("child.stdout"), &output.stdout).unwrap();
        std::fs::write(root.join("child.stderr"), &output.stderr).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        return;
    }
    let fixture = Fixture::new(true, true, Instant::now());
    let process = fixture.effect.export_process().unwrap();
    let pending_import: Vec<_> = {
        let held = process.duplicate_fds().unwrap();
        held.as_slice().iter().map(duplicate).collect()
    };
    let mut sets = Vec::new();
    loop {
        match process.duplicate_fds() {
            Ok(set) => sets.push(set),
            Err(EffectAuthorityError::Exhausted) => break,
            other => panic!("unexpected budget outcome: {other:?}", other = other.err()),
        }
    }
    let retained = sets.len();
    assert!(retained > 600 && retained < core::MAX_EFFECT_HANDLES / 6);
    let mut last_owners = Vec::new();
    while let Ok(host) = Host::new() {
        last_owners.push(host);
    }
    assert!(matches!(
        Host::new(),
        Err(core::Failure::AuthorityUnavailable)
    ));
    assert!(matches!(
        fixture.effect.export_process(),
        Err(EffectAuthorityError::Exhausted)
    ));
    assert!(matches!(
        ProcessEffectAuthorization::import_for_helper(process.binding(), pending_import),
        Err(EffectAuthorityError::Exhausted)
    ));
    assert!(process.is_current_monotonic(core::monotonic_now_ns().unwrap()));
    drop(last_owners);
    drop(sets);
    assert_eq!(process.duplicate_fds().unwrap().len(), 6);
    println!("global finite budget refused after {retained} retained six-FD sets; dropping sets recovered capacity");

    let source_replacement = Fixture::new(true, true, Instant::now());
    let old_source = source_replacement.effect.export_process().unwrap();

    let maximum = std::fs::read_dir("/proc/self/fd")
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .parse::<u64>()
                .unwrap()
        })
        .max()
        .unwrap();
    let limit = libc::rlimit {
        rlim_cur: maximum + 16,
        rlim_max: maximum + 16,
    };
    // only this disposable exec child lowers its limits; the runner is untouched.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
    let mut fillers = Vec::new();
    loop {
        let fd = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        if fd < 0 {
            break;
        }
        fillers.push(unsafe { OwnedFd::from_raw_fd(fd) });
    }
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EMFILE)
    );
    assert!(matches!(
        Host::new(),
        Err(core::Failure::AuthorityUnavailable)
    ));
    assert!(fixture.effect.export_process().is_err());
    assert!(matches!(
        fixture.core.borrow_mut().attach(999),
        Err(core::Failure::AuthorityUnavailable)
    ));
    let count = || {
        (0..limit.rlim_cur as i32)
            .filter(|fd| unsafe { libc::fcntl(*fd, libc::F_GETFD) } >= 0)
            .count()
    };
    let full = count();
    assert!(process.duplicate_fds().is_err());
    assert_eq!(count(), full);
    fillers.pop();
    fillers.pop();
    let before = count();
    assert!(
        process.duplicate_fds().is_err(),
        "six-FD dup must refuse with only two free slots"
    );
    assert_eq!(count(), before, "partial dup leaked descriptors");
    assert!(
        fixture.effect.export_process().is_err(),
        "partial read-only export must fail closed"
    );
    assert_eq!(count(), before, "partial export leaked descriptors");
    // refill both slots, then force revision allocation failure after old revoke.
    for _ in 0..2 {
        fillers.push(File::open("/dev/null").unwrap().into());
    }
    assert!(matches!(
        source_replacement
            .core
            .borrow_mut()
            .attach(source_replacement.source.pane()),
        Err(core::Failure::AuthorityUnavailable)
    ));
    assert!(source_replacement
        .core
        .borrow()
        .source(source_replacement.source.pane())
        .is_none());
    assert!(
        !old_source.is_current_monotonic(core::monotonic_now_ns().unwrap()),
        "failed source replacement left old authority live"
    );
    assert!(matches!(
        fixture
            .core
            .borrow_mut()
            .cancel_channel_work(fixture.channel),
        Err(core::Failure::AuthorityUnavailable)
    ));
    assert!(!process.is_current_monotonic(core::monotonic_now_ns().unwrap()));
    fixture.origin.revoke();
    fixture.registration.as_ref().unwrap().revoke();
    assert!(!fixture.origin.is_current());
    assert_eq!(count(), full, "revocation allocated or leaked descriptors");
    drop(fillers);
    let recovery = Fixture::new(true, true, Instant::now());
    assert!(recovery
        .effect
        .export_process()
        .unwrap()
        .validate_after_claim()
        .is_ok());
    assert!(!process.is_current_monotonic(core::monotonic_now_ns().unwrap()));
    println!("EMFILE construction/export/full and partial dup refused without leaks; failed source replacement retired old authority; direct revokes and fresh recovery succeeded");
}
