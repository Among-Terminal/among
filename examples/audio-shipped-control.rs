//! invokes the artifact's shipped Among, never a linked replacement decoder.
#[allow(dead_code)]
#[path = "../src/audio/authorization.rs"]
mod authorization;
use among::notifications::{
    self, EffectAuthorization, Eligibility, Focus, Host, Policy, Registration,
};
use among::plugin_notifications::{Backend, EventInput, Owner, Target};
use std::{
    cell::RefCell,
    error::Error,
    fs::{self, File},
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::process::CommandExt,
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
    origin: notifications::OriginLease,
    _backend: Backend,
    _registration: Registration,
}
impl Issued {
    fn new(targeted: bool, output: &Path) -> Result<Self> {
        let host = Rc::new(RefCell::new(checked(Host::new())?));
        let source = checked(host.borrow_mut().attach(17))?;
        let channel = checked(host.borrow_mut().register_channel(Policy {
            enabled: true,
            eligibility: Eligibility::Always,
        }))?;
        let registration = host
            .borrow()
            .registration(channel)
            .ok_or("registration absent")?;
        let mut backend = Backend::new(host.clone(), Focus::default());
        if !backend.bind("actual-shipped-audio-helper") {
            return Err("actual backend bind refused".into());
        }
        let owner = Owner {
            identity: "github:12/actual-owned-helper".into(),
            generation: "one-private-instance".into(),
        };
        checked(backend.register_identity(&owner.identity))?;
        let origin = checked(backend.admit(owner.clone(), false, None))?;
        let received = Instant::now();
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
                message: "actual artifact owned effect".into(),
                causal_job: None,
            },
            targeted.then_some(10),
            received,
        ))?;
        let now = Instant::now();
        let job = host
            .borrow_mut()
            .take_ready(channel, now)
            .ok_or("ready exact job absent")?;
        let delivery = backend
            .authorize_delivery(job, now)
            .ok_or("private backend issuance refused")?;
        let authorization = host
            .borrow()
            .authorization(job)
            .ok_or("private core issuance refused")?;
        let effect = checked(delivery.effect_authorization(&authorization, now))?;
        fs::write(
            output.join("privately-issued-job.txt"),
            format!(
                "job={job:?}\neffect_job={:?}\nexpires={:?}\n",
                effect.job(),
                effect.expires_at()
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
struct Owned {
    child: Child,
    reaped: bool,
}
impl Drop for Owned {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        let _ = self.child.kill();
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(2) {
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    self.reaped = true;
                    return;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(error) => {
                    eprintln!("owned shipped helper cleanup: {error}");
                    return;
                }
            }
        }
        eprintln!("owned shipped helper cleanup remains uncertain");
    }
}
fn nonblocking(fd: i32) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
fn drain(pipe: &mut impl Read, bytes: &mut Vec<u8>) -> Result<()> {
    loop {
        let mut chunk = [0_u8; 16384];
        match pipe.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(count) => {
                bytes.extend_from_slice(&chunk[..count]);
                if bytes.len() > 1024 * 1024 {
                    return Err("actual shipped helper protocol exceeded finite budget".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => (),
            Err(error) => return Err(error.into()),
        }
    }
}
fn main() -> Result<()> {
    if !Path::new("/.dockerenv").is_file()
        || ["/dev/snd", "/dev/dsp", "/dev/audio", "/dev/dri"]
            .iter()
            .any(|path| Path::new(path).exists())
    {
        return Err("device-free owned container required".into());
    }
    let args: Vec<_> = std::env::args_os().collect();
    let name = args
        .get(1)
        .and_then(|arg| arg.to_str())
        .ok_or("actual case required")?;
    let output = PathBuf::from(args.get(2).ok_or("new output required")?);
    let input = PathBuf::from(args.get(3).ok_or("sealed WAV fixture required")?);
    let payload = PathBuf::from(args.get(4).ok_or("actual payload required")?).canonicalize()?;
    fs::create_dir(&output)?;
    let (case, targeted) = name
        .strip_suffix("-targetless")
        .map_or((name, true), |case| (case, false));
    if !matches!(case, "actual-normal" | "actual-active-revoke") {
        return Err("unknown shipped helper case".into());
    }
    let runtime = std::env::var("XDG_RUNTIME_DIR")?;
    if std::env::var("PULSE_SERVER")? != format!("unix:{runtime}/pulse.sock") {
        return Err("private capture socket absent".into());
    }
    let issued = Issued::new(targeted, &output)?;
    let process_effect = checked(issued.effect.export_process())?;
    let descriptors = checked(process_effect.duplicate_fds())?;
    let operation = authorization::Authorization::new_bound(4721, process_effect.binding())?;
    let binding = process_effect.binding();
    let binding: String = binding.iter().map(|byte| format!("{byte:02x}")).collect();
    let fd = unsafe {
        libc::memfd_create(
            c"actual-shipped-audio-input".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut sealed = unsafe { File::from_raw_fd(fd) };
    use std::io::Write;
    sealed.write_all(&fs::read(if case == "actual-active-revoke" {
        input
    } else {
        input.with_file_name("tone-recovery.wav")
    })?)?;
    if unsafe {
        libc::fcntl(
            fd,
            libc::F_ADD_SEALS,
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut clock = std::mem::MaybeUninit::<libc::timespec>::uninit();
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, clock.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let clock = unsafe { clock.assume_init() };
    let deadline = clock.tv_sec as u64 * 1_000_000_000 + clock.tv_nsec as u64 + 13_000_000_000;
    let operation_fd = operation.fd();
    let raw: Vec<_> = descriptors
        .as_slice()
        .iter()
        .map(AsRawFd::as_raw_fd)
        .collect();
    let mut command = Command::new(payload.join("usr/bin/among"));
    command
        .arg("--among-audio-decoder-v2")
        .arg(&payload)
        .arg(deadline.to_string())
        .arg("4721")
        .arg(operation_fd.to_string())
        .arg(binding)
        .arg(
            raw.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
        );
    command
        .stdin(Stdio::from(sealed))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    unsafe {
        command.pre_exec(move || {
            for &fd in std::iter::once(&operation_fd).chain(raw.iter()) {
                if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut owned = Owned {
        child: command.spawn()?,
        reaped: false,
    };
    // retain the actual EffectDescriptors budget wrapper through spawn return.
    drop(descriptors);
    let mut stdout = owned
        .child
        .stdout
        .take()
        .ok_or("actual stdout pipe absent")?;
    let mut stderr = owned
        .child
        .stderr
        .take()
        .ok_or("actual stderr pipe absent")?;
    nonblocking(stdout.as_raw_fd())?;
    nonblocking(stderr.as_raw_fd())?;
    fs::write(
        output.join("actual-executable.txt"),
        format!(
            "exe={}\nhelper={}\n",
            fs::read_link(format!("/proc/{}/exe", owned.child.id()))?.display(),
            owned.child.id()
        ),
    )?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let start = Instant::now();
    let mut revoked = false;
    let mut active_marked = false;
    assert!(operation.grant());
    let status = loop {
        drain(&mut stdout, &mut out)?;
        drain(&mut stderr, &mut err)?;
        if case == "actual-active-revoke"
            && !active_marked
            && out.windows(9).any(|window| window == b"\"state\":2")
        {
            fs::write(
                output.join("active-await-observed-pcm"),
                b"actual shipped Among output before direct core revoke",
            )?;
            active_marked = true;
        }
        if active_marked && !revoked && output.join("active-pcm-observed").is_file() {
            issued.origin.revoke();
            revoked = true;
        }
        if let Some(status) = owned.child.try_wait()? {
            owned.reaped = true;
            break status;
        }
        if start.elapsed() >= Duration::from_secs(14) {
            fs::write(output.join("timeout-protocol.txt"), &out)?;
            fs::write(output.join("timeout-stderr.txt"), &err)?;
            return Err("actual shipped helper exceeded operation bound".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    drain(&mut stdout, &mut out)?;
    drain(&mut stderr, &mut err)?;
    fs::write(output.join("actual-production-protocol.jsonl"), &out)?;
    fs::write(output.join("actual-production-stderr.txt"), &err)?;
    fs::write(
        output.join("actual-production-exit.txt"),
        format!(
            "status={status} revoked={revoked} elapsed_ms={}\n",
            start.elapsed().as_millis()
        ),
    )?;
    let normal = case == "actual-normal";
    let messages: Vec<serde_json::Value> = std::str::from_utf8(&out)?
        .lines()
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()?;
    if normal {
        assert!(
            status.success(),
            "actual shipped stderr: {}",
            String::from_utf8_lossy(&err)
        );
        assert_eq!(
            messages
                .iter()
                .map(|message| message["state"].as_u64())
                .collect::<Vec<_>>(),
            vec![Some(1), Some(2), Some(3)]
        );
        fs::write(
            output.join("decoder-maps.txt"),
            messages[2]["maps"]
                .as_str()
                .ok_or("actual shipped maps absent")?,
        )?;
    } else {
        assert!(!status.success());
        let expected = "Audio event authority was revoked or expired";
        assert!(
            String::from_utf8_lossy(&err).contains(expected),
            "actual shipped stderr: {}",
            String::from_utf8_lossy(&err)
        );
        assert!(revoked);
        let active = messages
            .iter()
            .find(|message| message["state"] == 2)
            .ok_or("actual shipped active protocol absent")?;
        fs::write(
            output.join("decoder-maps.txt"),
            active["maps"].as_str().ok_or("actual active maps absent")?,
        )?;
        assert!(!messages.iter().any(|message| message["state"] == 3));
    }
    Ok(())
}
