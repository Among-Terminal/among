//! real owned audio operations with external malformed-input and EOF faults.
#[path = "audio-input-hooks/mod.rs"]
mod input_hooks;
#[path = "audio-stall-hooks/mod.rs"]
mod native;
#[path = "audio-effect-hooks/mod.rs"]
mod park;

use among::{
    audio::{Audio, DecoderPhase, DecoderSnapshot, Notice, Request, Sound},
    notifications::{
        self as core, EffectAuthorization, Eligibility, Focus, Host, Policy, Registration,
    },
    plugin_notifications::{Backend, EventInput, Owner, Target},
};
use std::{
    cell::RefCell,
    error::Error,
    fs::{self, File},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::{CommandExt, ExitStatusExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    rc::Rc,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn checked<T, E: std::fmt::Debug>(value: std::result::Result<T, E>) -> Result<T> {
    value.map_err(|error| format!("{error:?}").into())
}

struct Issued {
    effect: EffectAuthorization,
    origin: core::OriginLease,
    // these owned guards keep the actual writers/registration alive, not copied
    // booleans or authority manufactured by an audio fixture.
    _backend: Backend,
    _registration: Registration,
}
impl Issued {
    fn new(targeted: bool, age: Duration, output: &Path) -> Result<Self> {
        let host = Rc::new(RefCell::new(checked(Host::new())?));
        let source = checked(host.borrow_mut().attach(17))?;
        let channel = checked(host.borrow_mut().register_channel(Policy {
            enabled: true,
            eligibility: Eligibility::Always,
        }))?;
        let registration = host
            .borrow()
            .registration(channel)
            .ok_or("registration unavailable")?;
        let mut backend = Backend::new(host.clone(), Focus::default());
        if !backend.bind("audio-input-owned-session") {
            return Err("backend bind refused".into());
        }
        let owner = Owner {
            identity: "github:12/audio-input-probe".into(),
            generation: "owned-instance".into(),
        };
        checked(backend.register_identity(&owner.identity))?;
        let origin = checked(backend.admit(owner.clone(), false, None))?;
        let received = Instant::now()
            .checked_sub(age)
            .ok_or("invalid original age")?;
        checked(backend.produce(
            &owner,
            EventInput {
                sequence: 1,
                target: if targeted {
                    Target::Pane { reference: 17 }
                } else {
                    Target::Application
                },
                source_generation: targeted.then(|| source.generation().to_string()),
                message: "owned input-boundary probe".into(),
                causal_job: None,
            },
            targeted.then_some(10),
            received,
        ))?;
        let now = Instant::now();
        let job = host
            .borrow_mut()
            .take_ready(channel, now)
            .ok_or("exact ready job absent")?;
        let delivery = backend
            .authorize_delivery(job, now)
            .ok_or("trusted backend issuance refused")?;
        let authorization = host
            .borrow()
            .authorization(job)
            .ok_or("trusted core issuance refused")?;
        let effect = checked(delivery.effect_authorization(&authorization, now))?;
        fs::write(
            output.join("issued-job.txt"),
            format!(
                "{job:?}\neffect_job={:?}\noriginal_expiry={:?}\noriginal_age_ms={}\n",
                effect.job(),
                effect.expires_at(),
                received.elapsed().as_millis()
            ),
        )?;
        assert_eq!(effect.job(), job);
        assert_eq!(effect.expires_at(), received + Duration::from_secs(10));
        Ok(Self {
            effect,
            origin,
            _backend: backend,
            _registration: registration,
        })
    }
}

fn pump() {
    for _ in 0..100 {
        if !glib::MainContext::default().iteration(false) {
            break;
        }
    }
}
fn wait(mut ready: impl FnMut() -> bool, bound: Duration) -> Result<()> {
    let start = Instant::now();
    while !ready() {
        if start.elapsed() >= bound {
            return Err("owned input-boundary observation timed out".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}
fn plays(root: &Path) -> Result<String> {
    match fs::read_to_string(root.join("audio-play-invocations")) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}
fn terminal(audio: &Audio, id: u64, output: &Path) -> Result<(DecoderSnapshot, Vec<Notice>)> {
    let start = Instant::now();
    let mut notices = Vec::new();
    loop {
        pump();
        notices.extend(audio.drain_notices());
        let snapshot = audio.decoder_snapshot();
        if snapshot.as_ref().is_some_and(|state| state.phase == DecoderPhase::Finished)
            && notices.iter().any(|notice| matches!(notice, Notice::DecoderEnded { id: observed } | Notice::Cancelled { id: observed } | Notice::Failed { id: observed, .. } if *observed == id)) {
            fs::write(output.join("terminal.txt"), format!("{snapshot:?}\n{notices:?}\n"))?;
            return Ok((snapshot.ok_or("missing actual snapshot")?, notices));
        }
        if start.elapsed() >= Duration::from_secs(10) {
            fs::write(
                output.join("timeout.txt"),
                format!("{snapshot:?}\n{notices:?}\n"),
            )?;
            return Err("actual terminal notice/retirement exceeded original event bound".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn setup(payload: Option<&Path>) -> Result<Audio> {
    gtk::init()?;
    among::audio::initialize_execution(payload)?;
    Ok(Audio::new()?)
}
struct Exit;
impl Drop for Exit {
    fn drop(&mut self) {
        if !among::audio::finish_execution_shutdown() {
            eprintln!("input probe cleanup remains uncertain");
        }
    }
}
struct Release(PathBuf);
impl Drop for Release {
    fn drop(&mut self) {
        for name in ["audio-native-stall-release", "audio-supervisor-release"] {
            if let Err(error) = fs::write(self.0.join(name), b"release owned probe barrier") {
                eprintln!("input probe barrier cleanup: {error}");
            }
        }
    }
}

fn fresh(
    audio: &Audio,
    output: &Path,
    root: &Path,
    input: &Path,
    targeted: bool,
    previous: Option<(u32, u64)>,
) -> Result<()> {
    input_hooks::configure("none")?;
    fs::write(
        output.join("fresh-recovery-await-quiet"),
        b"new quiet PCM before independent fresh issuance",
    )?;
    wait(
        || output.join("fresh-recovery-quiet-observed").is_file(),
        Duration::from_secs(4),
    )?;
    let new = output.join("fresh");
    fs::create_dir(&new)?;
    let issued = Issued::new(targeted, Duration::ZERO, &new)?;
    audio.select(Sound::read_local(
        &input.with_file_name("tone-recovery.wav"),
    )?)?;
    audio.set_event_admitted(true);
    let Request::Started(id) = audio.event_authorized(issued.effect.clone())? else {
        return Err("explicit fresh operation refused".into());
    };
    let (snapshot, notices) = terminal(audio, id, &new)?;
    fs::write(new.join("plays.txt"), plays(root)?)?;
    let pid = snapshot.pid.ok_or("fresh native helper absent")?;
    if let Some((old_pid, old_id)) = previous {
        assert_ne!(pid, old_pid);
        assert_ne!(id, old_id);
    }
    assert!(notices
        .iter()
        .any(|notice| matches!(notice, Notice::DecoderEnded { id: observed } if *observed == id)));
    assert!(!notices
        .iter()
        .any(|notice| matches!(notice, Notice::Failed { .. })));
    Ok(())
}

fn pidfd(pid: u32) -> Result<File> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as i32, 0) } as i32;
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn exited(handle: &File) -> bool {
    let mut descriptor = libc::pollfd {
        fd: handle.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    unsafe { libc::poll(&mut descriptor, 1, 0) == 1 && descriptor.revents & libc::POLLIN != 0 }
}
fn disconnect_owned_reader(pid: u32, output: &Path) -> Result<()> {
    let pipe = fs::read_link(format!("/proc/{pid}/fd/1"))?;
    let mut owned = Vec::new();
    for entry in fs::read_dir("/proc/self/fd")? {
        let entry = entry?;
        let Some(fd) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        if fd >= 3
            && fs::read_link(entry.path()).is_ok_and(|path| path == pipe)
            && unsafe { libc::fcntl(fd, libc::F_GETFL) } & libc::O_ACCMODE == libc::O_RDONLY
        {
            owned.push(fd);
        }
    }
    if owned.len() != 1 {
        return Err("unique own supervisor read endpoint absent".into());
    }
    let null = File::open("/dev/null")?;
    if unsafe { libc::dup2(null.as_raw_fd(), owned[0]) } != owned[0]
        || unsafe { libc::fcntl(owned[0], libc::F_SETFD, libc::FD_CLOEXEC) } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    fs::write(
        output.join("owned-pipe-disconnected.txt"),
        format!(
            "owned_reader={} original_pipe={} helper={pid}\n",
            owned[0],
            pipe.display()
        ),
    )?;
    Ok(())
}

struct OwnedOwner {
    child: Child,
    reaped: bool,
}
impl Drop for OwnedOwner {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        if let Err(error) = self.child.kill() {
            eprintln!("owned test owner kill: {error}");
        }
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    self.reaped = true;
                    return;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(error) => {
                    eprintln!("owned test owner wait: {error}");
                    return;
                }
            }
        }
        eprintln!("owned test owner cleanup remains unconfirmed");
    }
}

fn owner_child(
    active: bool,
    targeted: bool,
    root: &Path,
    output: &Path,
    input: &Path,
    payload: Option<&Path>,
) -> Result<()> {
    let audio = setup(payload)?;
    let _exit = Exit;
    let issued = Issued::new(targeted, Duration::ZERO, output)?;
    audio.select(Sound::read_local(input)?)?;
    audio.set_event_admitted(true);
    if !active {
        fs::write(root.join("audio-native-stall"), b"late")?;
    }
    let Request::Started(id) = audio.event_authorized(issued.effect.clone())? else {
        return Err("owned death fixture admission refused".into());
    };
    let marker = if active {
        "audio-play-invocations"
    } else {
        "audio-native-stall-entered"
    };
    wait(|| root.join(marker).is_file(), Duration::from_secs(5))?;
    let snapshot = audio
        .decoder_snapshot()
        .ok_or("death fixture snapshot absent")?;
    let pid = snapshot.pid.ok_or("death fixture helper absent")?;
    fs::write(
        output.join("helper-before-owner-death.txt"),
        format!("{snapshot:?}\n"),
    )?;
    fs::write(output.join("helper-ready.txt"), pid.to_string())?;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(20) {
        pump();
        std::thread::sleep(Duration::from_millis(5));
    }
    fs::write(
        output.join("owner-was-not-killed.txt"),
        format!("id={id} snapshot={:?}\n", audio.decoder_snapshot()),
    )?;
    Err("controller did not kill its actual owned owner".into())
}

fn abrupt(
    active: bool,
    targeted: bool,
    recover: bool,
    root: &Path,
    output: &Path,
    input: &Path,
    payload: Option<&Path>,
) -> Result<()> {
    let child_output = output.join("owned-owner");
    let mut scene = if active {
        "child-abrupt-active"
    } else {
        "child-abrupt-construct"
    }
    .to_string();
    if !targeted {
        scene.push_str("-targetless");
    }
    let child = Command::new(std::env::current_exe()?)
        .arg(scene)
        .arg(&child_output)
        .arg(input)
        .arg(payload.unwrap_or(Path::new("-")))
        .stdin(Stdio::null())
        .stdout(File::create_new(output.join("owner.stdout"))?)
        .stderr(File::create_new(output.join("owner.stderr"))?)
        .process_group(0)
        .spawn()?;
    let mut owner = OwnedOwner {
        child,
        reaped: false,
    };
    wait(
        || child_output.join("helper-ready.txt").is_file(),
        Duration::from_secs(5),
    )?;
    let pid: u32 = fs::read_to_string(child_output.join("helper-ready.txt"))?.parse()?;
    let handle = pidfd(pid)?;
    fs::write(
        output.join("helper-before-kill-cmdline.bin"),
        fs::read(format!("/proc/{pid}/cmdline"))?,
    )?;
    if active {
        fs::write(
            output.join("active-await-observed-pcm"),
            b"actual output before abrupt owner death",
        )?;
        wait(
            || output.join("active-pcm-observed").is_file(),
            Duration::from_secs(4),
        )?;
    }
    owner.child.kill()?;
    let start = Instant::now();
    let status = loop {
        if let Some(status) = owner.child.try_wait()? {
            owner.reaped = true;
            break status;
        }
        if start.elapsed() >= Duration::from_secs(2) {
            return Err("owned killed process not reaped within bound".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    wait(|| exited(&handle), Duration::from_secs(2))?;
    fs::write(
        output.join("abrupt-owner-and-helper-exit.txt"),
        format!(
            "owner_status={status}\nowner_signal={:?}\nhelper={pid} pidfd_exit=true\nplays={}\n",
            status.signal(),
            plays(root)?
        ),
    )?;
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    assert_eq!(plays(root)?.lines().count(), usize::from(active));
    if recover {
        let audio = setup(payload)?;
        let _exit = Exit;
        fresh(&audio, output, root, input, targeted, None)?;
        audio.shutdown();
    }
    Ok(())
}

fn main() -> Result<()> {
    if !Path::new("/.dockerenv").is_file()
        || ["/dev/snd", "/dev/dsp", "/dev/audio", "/dev/dri"]
            .iter()
            .any(|path| Path::new(path).exists())
    {
        return Err("external input probe requires an owned device-free container".into());
    }
    input_hooks::initialize()?;
    if let Some(code) = among::audio::decoder_dispatch() {
        return if code == glib::ExitCode::SUCCESS {
            Ok(())
        } else {
            Err("actual decoder refused/failed".into())
        };
    }
    let root = native::guard()?;
    let args: Vec<_> = std::env::args_os().collect();
    let name = args
        .get(1)
        .and_then(|value| value.to_str())
        .ok_or("scenario required")?;
    let output = PathBuf::from(args.get(2).ok_or("new output required")?);
    let input = PathBuf::from(args.get(3).ok_or("fixture required")?);
    let payload = args.get(4).filter(|value| *value != "-").map(PathBuf::from);
    fs::create_dir(&output)?;
    let (case, recover) = name
        .strip_suffix("-recovery")
        .map_or((name, false), |value| (value, true));
    let (case, targeted) = case
        .strip_suffix("-targetless")
        .map_or((case, true), |value| (value, false));
    if matches!(case, "child-abrupt-construct" | "child-abrupt-active") {
        return owner_child(
            case == "child-abrupt-active",
            targeted,
            &root,
            &output,
            &input,
            payload.as_deref(),
        );
    }
    if matches!(case, "abrupt-construct" | "abrupt-active") {
        return abrupt(
            case == "abrupt-active",
            targeted,
            recover,
            &root,
            &output,
            &input,
            payload.as_deref(),
        );
    }
    let audio = setup(payload.as_deref())?;
    let _exit = Exit;
    let _release = Release(root.clone());
    let age = if case == "admission-expiry" {
        Duration::from_secs(9)
    } else {
        Duration::ZERO
    };
    let issued = Issued::new(targeted, age, &output)?;
    audio.select(Sound::read_local(&input)?)?;
    audio.set_event_admitted(true);
    if case.starts_with("admission-") {
        match case {
            "admission-origin" => issued.origin.revoke(),
            "admission-expiry" => wait(
                || Instant::now() >= issued.effect.expires_at(),
                Duration::from_secs(2),
            )?,
            _ => return Err("unknown admission case".into()),
        }
        let request = audio.event_authorized(issued.effect.clone())?;
        fs::write(
            output.join("admission.txt"),
            format!(
                "request={request:?} snapshot={:?}\n",
                audio.decoder_snapshot()
            ),
        )?;
        assert_eq!(request, Request::Suppressed);
        assert!(audio.decoder_snapshot().is_none());
        assert!(plays(&root)?.is_empty());
        if recover {
            fresh(&audio, &output, &root, &input, targeted, None)?;
        }
    } else if case == "preview-off" {
        audio.set_event_admitted(false);
        assert_eq!(
            audio.event_authorized(issued.effect.clone())?,
            Request::Suppressed
        );
        audio.select(Sound::read_local(
            &input.with_file_name("tone-recovery.wav"),
        )?)?;
        let Request::Started(id) = audio.preview()? else {
            return Err("explicit Preview while events off refused".into());
        };
        let (_, notices) = terminal(&audio, id, &output)?;
        assert!(notices.iter().any(
            |notice| matches!(notice, Notice::DecoderEnded { id: observed } if *observed == id)
        ));
    } else if let Some(fault) = case.strip_prefix("malformed-") {
        input_hooks::configure(fault)?;
        let Request::Started(id) = audio.event_authorized(issued.effect.clone())? else {
            return Err("owned malformed operation not started".into());
        };
        let (snapshot, notices) = terminal(&audio, id, &output)?;
        let expected = match fault {
            "binding-nonce" | "binding-job" => "audio operation/effect binding mismatch",
            "operation" => "audio start authority belongs to another operation",
            "missing-fd" | "aliased-fd" => "Missing, aliased or invalid audio effect descriptor",
            "reordered-fds" => "invalid effect authority",
            _ => return Err("unknown malformed case".into()),
        };
        fs::write(
            output.join("expected-refusal.txt"),
            format!("expected={expected}\nplays={}\n", plays(&root)?),
        )?;
        assert!(
            snapshot.stderr.contains(expected),
            "actual refusal: {}",
            snapshot.stderr
        );
        assert!(notices.iter().any(
            |notice| matches!(notice, Notice::Failed { id: observed, .. } if *observed == id)
        ));
        assert!(plays(&root)?.is_empty());
        assert!(!root.join("audio-native-returned").exists());
        assert!(issued.effect.is_current(Instant::now()));
        if recover {
            fresh(
                &audio,
                &output,
                &root,
                &input,
                targeted,
                Some((snapshot.pid.ok_or("actual refused helper missing")?, id)),
            )?;
        }
    } else if matches!(case, "eof-construct" | "eof-active") {
        let active = case == "eof-active";
        fs::write(root.join("audio-supervisor-park"), b"park owned supervisor")?;
        if !active {
            fs::write(root.join("audio-native-stall"), b"late")?;
        }
        let Request::Started(id) = audio.event_authorized(issued.effect.clone())? else {
            return Err("owned EOF operation not started".into());
        };
        wait(
            || root.join("audio-supervisor-parked").exists(),
            Duration::from_secs(5),
        )?;
        let snapshot = audio
            .decoder_snapshot()
            .ok_or("owned helper snapshot missing")?;
        let pid = snapshot.pid.ok_or("owned helper PID missing")?;
        let handle = pidfd(pid)?;
        if active {
            wait(
                || root.join("audio-play-invocations").exists(),
                Duration::from_secs(3),
            )?;
            fs::write(
                output.join("active-await-observed-pcm"),
                b"actual PCM before disconnect",
            )?;
            wait(
                || output.join("active-pcm-observed").is_file(),
                Duration::from_secs(4),
            )?;
        } else {
            wait(
                || root.join("audio-native-stall-entered").exists(),
                Duration::from_secs(5),
            )?;
        }
        disconnect_owned_reader(pid, &output)?;
        if !active {
            fs::write(
                root.join("audio-native-stall-release"),
                b"release before expiry",
            )?;
        }
        wait(|| exited(&handle), Duration::from_secs(2))?;
        fs::write(
            output.join("independent-eof-exit.txt"),
            format!(
                "helper={pid} exited=true core_still_live={}\n",
                issued.effect.is_current(Instant::now())
            ),
        )?;
        assert!(issued.effect.is_current(Instant::now()));
        assert_eq!(plays(&root)?.lines().count(), usize::from(active));
        assert_eq!(audio.preview()?, Request::Busy);
        fs::write(
            root.join("audio-supervisor-release"),
            b"confirm real retirement",
        )?;
        let (_, notices) = terminal(&audio, id, &output)?;
        assert!(!notices
            .iter()
            .any(|notice| matches!(notice, Notice::DecoderEnded { .. })));
        if recover {
            fresh(&audio, &output, &root, &input, targeted, Some((pid, id)))?;
        }
    } else {
        return Err("unknown real input boundary".into());
    }
    audio.shutdown();
    Ok(())
}
