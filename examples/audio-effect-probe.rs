//! real backend issuance and the actual audio helper, not a synthetic effect.
#[path = "audio-effect-hooks/mod.rs"]
mod effect_hooks;
#[path = "audio-fork-hook/mod.rs"]
mod fork_hook;
#[path = "audio-stall-hooks/mod.rs"]
mod hooks;

use among::audio::{Audio, DecoderPhase, Notice, Request, Sound};
use among::notifications::{
    self as core, EffectAuthorization, Eligibility, Focus, Host, Policy, Registration, Source,
};
use among::plugin_notifications::{Backend, EventInput, Owner, Target};
use std::{
    cell::RefCell,
    error::Error,
    fs,
    os::fd::{AsRawFd, FromRawFd},
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn checked<T, E: std::fmt::Debug>(value: std::result::Result<T, E>, context: &str) -> Result<T> {
    value.map_err(|error| format!("{context}: {error:?}").into())
}

struct Fixture {
    host: Rc<RefCell<Host>>,
    backend: Backend,
    source: Source,
    channel: core::Channel,
    registration: Option<Registration>,
    origin: core::OriginLease,
    effect: EffectAuthorization,
    received: Instant,
}

impl Fixture {
    fn new(targeted: bool, age: Duration, output: &Path) -> Result<Self> {
        let host = Rc::new(RefCell::new(checked(Host::new(), "new host")?));
        let source = checked(host.borrow_mut().attach(17), "attach source")?;
        let channel = checked(
            host.borrow_mut().register_channel(Policy {
                enabled: true,
                eligibility: Eligibility::Always,
            }),
            "register channel",
        )?;
        let registration = Some(
            host.borrow()
                .registration(channel)
                .ok_or("owned registration refused")?,
        );
        let mut backend = Backend::new(host.clone(), Focus::default());
        if !backend.bind("audio-effect-owned-session") {
            return Err("backend bind refused".into());
        }
        let owner = Owner {
            identity: "github:12/audio-effect-probe".into(),
            generation: "owned-instance".into(),
        };
        checked(
            backend.register_identity(&owner.identity),
            "register managed identity",
        )?;
        let origin = checked(
            backend.admit(owner.clone(), false, None),
            "admit managed origin",
        )?;
        let received = Instant::now()
            .checked_sub(age)
            .ok_or("invalid original received-at")?;
        let event = checked(
            backend.produce(
                &owner,
                EventInput {
                    sequence: 1,
                    target: if targeted {
                        Target::Pane { reference: 17 }
                    } else {
                        Target::Application
                    },
                    source_generation: targeted.then(|| source.generation().to_string()),
                    message: "owned actual audio probe".into(),
                    causal_job: None,
                },
                targeted.then_some(10),
                received,
            ),
            "managed production",
        )?;
        let now = Instant::now();
        let job = host
            .borrow_mut()
            .take_ready(channel, now)
            .ok_or("no exact ready job")?;
        assert_eq!(job.event(), event);
        let delivery = backend
            .authorize_delivery(job, now)
            .ok_or("trusted backend issuance refused")?;
        let authorization = host
            .borrow()
            .authorization(job)
            .ok_or("trusted core issuance refused")?;
        let effect = checked(
            delivery.effect_authorization(&authorization, now),
            "owned effect conversion",
        )?;
        assert_eq!(effect.job(), job);
        assert_eq!(effect.expires_at(), received + Duration::from_secs(10));
        fs::write(
            output.join("issued-job.txt"),
            format!(
                "{job:?}\noriginal_age_ms={}\noriginal_expiry={:?}\ntargeted={targeted}\n",
                received.elapsed().as_millis(),
                effect.expires_at()
            ),
        )?;
        Ok(Self {
            host,
            backend,
            source,
            channel,
            registration,
            origin,
            effect,
            received,
        })
    }

    fn revoke(&mut self, scope: &str) -> Result<()> {
        match scope {
            "origin" => {
                let _unpolled_backend = &self.backend;
                self.origin.revoke();
            }
            "source" => self.host.borrow_mut().detach(self.source),
            "source-reuse" => {
                checked(
                    self.host.borrow_mut().attach(17),
                    "replace source generation",
                )?;
            }
            "policy" => {
                checked(
                    self.host
                        .borrow_mut()
                        .set_channel_policy(self.channel, Policy::DELIVERY_DEFAULT),
                    "disable policy",
                )?;
            }
            "off-on" => {
                checked(
                    self.host
                        .borrow_mut()
                        .set_channel_policy(self.channel, Policy::DELIVERY_DEFAULT),
                    "policy off",
                )?;
                checked(
                    self.host.borrow_mut().set_channel_policy(
                        self.channel,
                        Policy {
                            enabled: true,
                            eligibility: Eligibility::Always,
                        },
                    ),
                    "policy on",
                )?;
            }
            "configuration" => checked(
                self.host.borrow_mut().cancel_channel_work(self.channel),
                "configuration revision",
            )?,
            "registration" => self
                .registration
                .as_ref()
                .ok_or("registration missing")?
                .revoke(),
            "registration-drop" => drop(self.registration.take()),
            "session" => {
                self.host.replace(checked(Host::new(), "replace session")?);
            }
            "expiry" => wait_without_gui(
                || Instant::now() >= self.effect.expires_at(),
                Duration::from_secs(4),
            )?,
            _ => return Err("unknown terminal scope".into()),
        }
        assert!(!self.effect.is_current(Instant::now()));
        Ok(())
    }
}

fn pump() {
    for _ in 0..100 {
        if !glib::MainContext::default().iteration(false) {
            break;
        }
    }
}

fn wait_without_gui(mut condition: impl FnMut() -> bool, limit: Duration) -> Result<()> {
    let start = Instant::now();
    while !condition() {
        if start.elapsed() >= limit {
            return Err("owned effect observation exceeded its bound".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn wait_retired(audio: &Audio, id: u64, output: &Path) -> Result<Vec<Notice>> {
    let start = Instant::now();
    let mut notices = Vec::new();
    loop {
        pump();
        notices.extend(audio.drain_notices());
        if audio
            .decoder_snapshot()
            .is_some_and(|state| state.phase == DecoderPhase::Finished)
            && notices.iter().any(|notice| matches!(notice,
                Notice::DecoderEnded { id: observed } | Notice::Failed { id: observed, .. } | Notice::Cancelled { id: observed }
                if *observed == id))
        {
            return Ok(notices);
        }
        if start.elapsed() >= Duration::from_secs(2) {
            fs::write(
                output.join("retirement-timeout.txt"),
                format!(
                    "snapshot={:?}\nnotices={notices:?}\n",
                    audio.decoder_snapshot()
                ),
            )?;
            return Err("confirmed retirement exceeded its bound".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn plays(root: &Path) -> Result<String> {
    match fs::read_to_string(root.join("audio-play-invocations")) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

fn child_pid(root: &Path, delayed_spawn: bool) -> Result<u32> {
    let text = fs::read_to_string(root.join(if delayed_spawn {
        "audio-fork-real"
    } else {
        "audio-native-returned"
    }))?;
    Ok(text.trim().parse()?)
}

fn observe_live_helper(pid: u32, output: &Path) -> Result<fs::File> {
    // read-only evidence for the actual child produced by our owned operation;
    // the PID is never used as cancellation authority.
    fs::write(
        output.join("helper-cmdline.bin"),
        fs::read(format!("/proc/{pid}/cmdline"))?,
    )?;
    fs::write(
        output.join("helper-maps.txt"),
        fs::read(format!("/proc/{pid}/maps"))?,
    )?;
    let mut descriptors = String::new();
    for entry in fs::read_dir(format!("/proc/{pid}/fd"))? {
        let entry = entry?;
        let observation = (|| -> std::io::Result<String> {
            Ok(format!(
                "{} -> {}\n{}",
                entry.file_name().to_string_lossy(),
                fs::read_link(entry.path())?.display(),
                fs::read_to_string(format!(
                    "/proc/{pid}/fdinfo/{}",
                    entry.file_name().to_string_lossy()
                ))?
            ))
        })();
        match observation {
            Ok(text) => descriptors.push_str(&text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                descriptors.push_str(&format!(
                    "{} transient native descriptor closed during observation\n",
                    entry.file_name().to_string_lossy()
                ))
            }
            Err(error) => return Err(error.into()),
        }
    }
    fs::write(output.join("helper-fdinfo.txt"), descriptors)?;
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as i32, 0) } as i32;
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}

fn wait_helper_exit(handle: &fs::File) -> Result<()> {
    let start = Instant::now();
    loop {
        let mut descriptor = libc::pollfd {
            fd: handle.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let status = unsafe { libc::poll(&mut descriptor, 1, 0) };
        if status < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if status == 1 && descriptor.revents & libc::POLLIN != 0 {
            return Ok(());
        }
        if start.elapsed() >= Duration::from_secs(2) {
            return Err("helper did not independently observe revoked scope while supervisor remained parked".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

struct Release(PathBuf);
impl Drop for Release {
    fn drop(&mut self) {
        for name in [
            "audio-fork-release",
            "audio-native-stall-release",
            "audio-supervisor-release",
        ] {
            if let Err(error) = fs::write(self.0.join(name), b"release owned test barrier") {
                eprintln!("probe cleanup release failed: {error}");
            }
        }
    }
}
struct Exit;
impl Drop for Exit {
    fn drop(&mut self) {
        if !among::audio::finish_execution_shutdown() {
            eprintln!("owned effect probe exit: cleanup remains uncertain");
        }
    }
}

fn recovery(
    audio: &Audio,
    sound: Sound,
    targeted: bool,
    output: &Path,
    root: &Path,
    old_pid: u32,
    old_id: u64,
) -> Result<()> {
    let recovery_output = output.join("fresh-recovery");
    fs::create_dir(&recovery_output)?;
    fs::write(
        output.join("fresh-recovery-await-quiet"),
        b"observe a new quiet PCM streak before fresh issuance",
    )?;
    wait_without_gui(
        || output.join("fresh-recovery-quiet-observed").is_file(),
        Duration::from_secs(4),
    )?;
    let fixture = Fixture::new(targeted, Duration::ZERO, &recovery_output)?;
    audio.select(sound)?;
    audio.set_event_admitted(true);
    let Request::Started(new_id) = audio.event_authorized(fixture.effect.clone())? else {
        return Err("fresh-authority explicit recovery was not admitted".into());
    };
    let start = Instant::now();
    let mut notices = Vec::new();
    loop {
        pump();
        notices.extend(audio.drain_notices());
        if audio
            .decoder_snapshot()
            .is_some_and(|state| state.phase == DecoderPhase::Finished)
            && notices.iter().any(|notice| {
                matches!(notice,
                Notice::DecoderEnded { id } | Notice::Failed { id, .. } | Notice::Cancelled { id }
                if *id == new_id)
            })
        {
            break;
        }
        if start.elapsed() >= Duration::from_secs(10) {
            fs::write(
                recovery_output.join("timeout-diagnostics.txt"),
                format!(
                    "id={new_id} snapshot={:?}\nnotices={notices:?}\nplays={}\n",
                    audio.decoder_snapshot(),
                    plays(root)?
                ),
            )?;
            return Err("fresh-authority recovery failed its original event bound".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let snapshot = audio
        .decoder_snapshot()
        .ok_or("missing recovery snapshot")?;
    notices.extend(audio.drain_notices());
    fs::write(
        recovery_output.join("actual-helper.txt"),
        format!(
            "id={new_id} pid={:?}\n{snapshot:?}\n{notices:?}\nplays={}\n",
            snapshot.pid,
            plays(root)?
        ),
    )?;
    let pid = snapshot.pid.ok_or("missing actual recovery PID")?;
    assert_ne!(new_id, old_id);
    assert_ne!(pid, old_pid);
    assert!(notices
        .iter()
        .any(|notice| matches!(notice, Notice::DecoderEnded { id } if *id == new_id)));
    assert!(!notices
        .iter()
        .any(|notice| matches!(notice, Notice::Failed { .. })));
    Ok(())
}

fn main() -> Result<()> {
    if !Path::new("/.dockerenv").is_file()
        || ["/dev/snd", "/dev/dsp", "/dev/audio", "/dev/dri"]
            .iter()
            .any(|path| Path::new(path).exists())
    {
        return Err("owned device-free container required".into());
    }
    // mirror the application: import restores inherited FD CLOEXEC before any
    // child-producing probe guard or native scanner can run in the helper.
    if let Some(code) = among::audio::decoder_dispatch() {
        return if code == glib::ExitCode::SUCCESS {
            Ok(())
        } else {
            Err("real audio helper refused/failed".into())
        };
    }
    let runtime = hooks::guard()?;
    let args: Vec<_> = std::env::args_os().collect();
    let name = args
        .get(1)
        .and_then(|value| value.to_str())
        .ok_or("scenario required")?;
    let output = PathBuf::from(args.get(2).ok_or("new output required")?);
    let input = PathBuf::from(args.get(3).ok_or("WAV fixture required")?);
    let payload = args.get(4).filter(|value| *value != "-").map(PathBuf::from);
    fs::create_dir(&output)?;
    let (case, recover) = name
        .strip_suffix("-recovery")
        .map_or((name, false), |case| (case, true));
    let (case, targeted) = case
        .strip_suffix("-targetless")
        .map_or((case, true), |case| (case, false));
    let (stage, scope) = case
        .split_once('-')
        .ok_or("stage-scope scenario required")?;
    if !matches!(stage, "spawn" | "pending" | "construct" | "active") {
        return Err("unknown actual boundary stage".into());
    }
    if !targeted && matches!(scope, "source" | "source-reuse") {
        return Err("targetless authority has no source scope".into());
    }
    let age = if scope == "expiry" {
        Duration::from_secs(7)
    } else {
        Duration::ZERO
    };
    let mut fixture = Fixture::new(targeted, age, &output)?;
    gtk::init()?;
    among::audio::initialize_execution(payload.as_deref())?;
    let _exit = Exit;
    let _release = Release(runtime.clone());
    let audio = Audio::new()?;
    let sound = Sound::read_local(&input)?;
    audio.select(sound)?;
    audio.set_event_admitted(true);
    if matches!(stage, "spawn" | "pending") {
        fs::write(runtime.join("audio-fork-stall"), b"after")?;
    } else {
        fs::write(
            runtime.join("audio-supervisor-park"),
            b"park actual supervisor after real ownership/grant",
        )?;
    }
    if stage != "active" {
        fs::write(runtime.join("audio-native-stall"), b"late")?;
    }
    let admitted = Instant::now();
    let Request::Started(id) = audio.event_authorized(fixture.effect.clone())? else {
        return Err("owned event admission failed".into());
    };
    let delayed = matches!(stage, "spawn" | "pending");
    wait_without_gui(
        || {
            runtime
                .join(if delayed {
                    "audio-fork-entered"
                } else {
                    "audio-supervisor-parked"
                })
                .exists()
        },
        Duration::from_secs(5),
    )?;
    if stage != "active" {
        wait_without_gui(
            || runtime.join("audio-native-stall-entered").exists(),
            Duration::from_secs(5),
        )?;
    }
    if stage == "pending" || stage == "active" {
        if stage == "pending" {
            fs::write(
                runtime.join("audio-native-stall-release"),
                b"complete real construction before scope revoke",
            )?;
        }
        wait_without_gui(
            || runtime.join("audio-native-returned").exists(),
            Duration::from_secs(3),
        )?;
    }
    // Constructor injection enters before native return, so its own entered
    // record is the trusted known-child observation when return is still parked.
    let pid = if delayed {
        child_pid(&runtime, true)?
    } else if stage == "active" {
        child_pid(&runtime, false)?
    } else {
        let text = fs::read_to_string(runtime.join("audio-native-stall-entered"))?;
        text.split_whitespace()
            .find_map(|field| field.strip_prefix("helper="))
            .ok_or("missing actual constructor child")?
            .parse()?
    };
    let handle = observe_live_helper(pid, &output)?;
    if stage == "active" {
        wait_without_gui(
            || runtime.join("audio-play-invocations").exists(),
            Duration::from_secs(3),
        )?;
        fs::write(
            output.join("active-await-observed-pcm"),
            b"recorder must observe actual tone before revoke",
        )?;
        wait_without_gui(
            || output.join("active-pcm-observed").exists(),
            Duration::from_secs(4),
        )?;
        assert_eq!(plays(&runtime)?.lines().count(), 1);
        fs::write(
            output.join("active-helper-maps.txt"),
            fs::read(format!("/proc/{pid}/maps"))?,
        )?;
    } else {
        assert!(plays(&runtime)?.is_empty());
    }
    fixture.revoke(scope)?;
    fs::write(output.join("scope-revoked.txt"), format!("scope={scope}\noriginal_age_ms={}\noperation_age_ms={}\nhelper={pid}\nowner_gui_unpolled=true\nsupervisor_still_parked=true\n", fixture.received.elapsed().as_millis(), admitted.elapsed().as_millis()))?;
    assert!(admitted.elapsed() < Duration::from_secs(13));
    if stage != "active" {
        fs::write(
            runtime.join("audio-native-stall-release"),
            b"release actual constructor before operation expiry",
        )?;
    }
    wait_helper_exit(&handle)?;
    assert_eq!(
        plays(&runtime)?.lines().count(),
        usize::from(stage == "active")
    );
    fs::write(output.join("independent-helper-exit.txt"), format!("helper={pid} pidfd_exit=true scope={scope} supervisor_parked=true operation_age_ms={}\n", admitted.elapsed().as_millis()))?;
    // Capacity is checked only after independent helper exit, so this explicit
    // cancellation cannot be the cause of the already-observed direct refusal.
    assert_eq!(audio.preview()?, Request::Busy);
    fs::write(
        runtime.join(if delayed {
            "audio-fork-release"
        } else {
            "audio-supervisor-release"
        }),
        b"allow real ownership observation and confirmed reaping",
    )?;
    let notices = wait_retired(&audio, id, &output)?;
    fs::write(
        output.join("retired.txt"),
        format!("{:?}\n{notices:?}\n", audio.decoder_snapshot()),
    )?;
    assert!(!notices
        .iter()
        .any(|notice| matches!(notice, Notice::DecoderEnded { .. })));
    if recover {
        let recovery_sound = Sound::read_local(&input.with_file_name("tone-recovery.wav"))?;
        recovery(&audio, recovery_sound, targeted, &output, &runtime, pid, id)?;
    }
    audio.shutdown();
    Ok(())
}
