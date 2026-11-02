//! actual GTK heartbeat + trusted native-call stalls. Not a production control.
#[path = "audio-cancellation-cases/mod.rs"]
mod cancellation;
#[path = "audio-fork-hook/mod.rs"]
mod fork_hook;
#[path = "audio-stall-hooks/mod.rs"]
mod hooks;
use among::audio::{Audio, DecoderPhase, Notice, Request, Sound};
use gtk::prelude::*;
use std::{
    cell::Cell,
    error::Error,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

/// Fixture and evidence files written by this probe on the GTK thread. Under
/// writeback pressure such a write blocks in the kernel, and the heartbeat would
/// charge that delay to Among. The time is measured here so the bound applies to
/// the application alone, and the raw gap stays on record beside it.
#[derive(Default)]
pub(crate) struct OwnWork {
    pub(crate) total: Rc<Cell<Duration>>,
    pub(crate) longest: Cell<Duration>,
    pub(crate) longest_end: Cell<Option<Instant>>,
}

impl OwnWork {
    pub(crate) fn timed<T>(&self, work: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let result = work();
        let took = started.elapsed();
        self.total.set(self.total.get() + took);
        if took > self.longest.get() {
            self.longest.set(took);
            self.longest_end.set(Some(Instant::now()));
        }
        result
    }

    pub(crate) fn write(
        &self,
        path: impl AsRef<Path>,
        bytes: impl AsRef<[u8]>,
    ) -> std::io::Result<()> {
        self.timed(|| fs::write(path, bytes))
    }

    pub(crate) fn copy(
        &self,
        from: impl AsRef<Path>,
        to: impl AsRef<Path>,
    ) -> std::io::Result<u64> {
        self.timed(|| fs::copy(from, to))
    }

    pub(crate) fn remove_file(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        self.timed(|| fs::remove_file(path))
    }
}

fn pump(duration: Duration) {
    let start = Instant::now();
    while start.elapsed() < duration {
        for _ in 0..100 {
            if !glib::MainContext::default().iteration(false) {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
#[track_caller]
fn wait(mut condition: impl FnMut() -> bool, limit: Duration) -> Result<(), Box<dyn Error>> {
    let caller = std::panic::Location::caller();
    let started = Instant::now();
    while !condition() {
        if started.elapsed() >= limit {
            return Err(format!("retirement observation exceeded its bound at {caller}").into());
        }
        pump(Duration::from_millis(5));
    }
    Ok(())
}

/// Collects notices until this operation has ended. A fixed pump would turn a
/// slow start under disk load into a false report that the sound never retired;
/// the limit is above the operation's own deadline, so a hung decoder still fails.
fn wait_for_end(audio: &Audio, id: u64) -> Result<Vec<Notice>, Box<dyn Error>> {
    let mut notices = Vec::new();
    wait(
        || {
            notices.extend(audio.drain_notices());
            notices
                .iter()
                .any(|notice| matches!(notice, Notice::DecoderEnded { id: ended } if *ended == id))
        },
        Duration::from_secs(20),
    )?;
    Ok(notices)
}
fn save(audio: &Audio, output: &Path) -> Result<(), Box<dyn Error>> {
    if let Some(snapshot) = audio.decoder_snapshot() {
        fs::write(output.join("decoder-maps.txt"), snapshot.maps.as_ref())?;
        fs::write(output.join("decoder-errors.txt"), snapshot.stderr.as_ref())?;
        fs::write(output.join("decoder-state.txt"), format!("{snapshot:?}"))?;
    }
    Ok(())
}
fn operation_directories() -> Result<usize, Box<dyn Error>> {
    Ok(fs::read_dir(std::env::temp_dir())?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .filter(|entry| {
            entry
                .file_name()
                .as_encoded_bytes()
                .starts_with(b"among-audio-operation-")
        })
        .count())
}
struct ExecutionExit(PathBuf);
impl Drop for ExecutionExit {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0.join("audio-deny-kill"));
        let _ = among::audio::finish_execution_shutdown();
    }
}
fn main() -> Result<(), Box<dyn Error>> {
    let runtime = hooks::guard()?;
    if let Some(code) = among::audio::decoder_dispatch() {
        return if code == glib::ExitCode::SUCCESS {
            Ok(())
        } else {
            Err("decoder helper failed".into())
        };
    }
    let args: Vec<_> = std::env::args_os().collect();
    let scenario = args
        .get(1)
        .and_then(|arg| arg.to_str())
        .ok_or("scenario required")?;
    let output = PathBuf::from(args.get(2).ok_or("new output required")?);
    fs::create_dir(&output)?;
    let input = PathBuf::from(args.get(3).ok_or("WAV fixture required")?);
    let root = args.get(4).filter(|arg| *arg != "-").map(PathBuf::from);
    if scenario.starts_with("cancel-") {
        return cancellation::run(scenario, &output, &input, root.as_deref(), &runtime);
    }
    among::audio::initialize_execution(root.as_deref())?;
    let _exit = ExecutionExit(runtime.clone());
    assert_eq!(
        operation_directories()?,
        0,
        "probe temporary root was not fresh"
    );
    let child = std::process::Command::new("/bin/cat")
        .arg("/proc/self/maps")
        .output()?;
    if !child.status.success() {
        return Err("host child environment observation failed".into());
    }
    fs::write(output.join("host-child-maps.txt"), child.stdout)?;
    gtk::init()?;
    let window = gtk::Window::builder()
        .title("Owned decoder retirement probe")
        .default_width(320)
        .default_height(120)
        .build();
    window.present();
    let ticks = Rc::new(Cell::new(0u64));
    let max_gap = Rc::new(Cell::new(Duration::ZERO));
    let last = Rc::new(Cell::new(Instant::now()));
    let (count, gap, previous) = (ticks.clone(), max_gap.clone(), last.clone());
    // The heartbeat measures whether Among blocks the GTK thread, so the tick
    // itself does no file I/O: a per-tick write blocks in the kernel under
    // write-back pressure and then reports the probe's own disk stall as an
    // Among stall. The largest gap and when it ended (ms since the heartbeat
    // started) are kept in memory and written once measurement is over.
    let beat_start = Instant::now();
    let worst_at = Rc::new(Cell::new(Duration::ZERO));
    let worst = worst_at.clone();
    let own = OwnWork::default();
    let net_gap = Rc::new(Cell::new(Duration::ZERO));
    let (spent, net_maximum, seen) = (
        own.total.clone(),
        net_gap.clone(),
        Cell::new(Duration::ZERO),
    );
    let timer = glib::timeout_add_local(Duration::from_millis(10), move || {
        let now = Instant::now();
        let elapsed = now.duration_since(previous.replace(now));
        if elapsed > gap.get() {
            gap.set(elapsed);
            worst.set(now.duration_since(beat_start));
        }
        let work = spent.get();
        let net = elapsed.saturating_sub(work.saturating_sub(seen.replace(work)));
        net_maximum.set(net_maximum.get().max(net));
        count.set(count.get() + 1);
        glib::ControlFlow::Continue
    });
    let sound = Sound::read_local(&input)?;
    let mut audio = Audio::new()?;
    audio.select(sound.clone())?;
    pump(Duration::from_millis(100));
    let mode = match scenario {
        "stall-construct-cancel"
        | "stall-watchdog"
        | "stall-event-revoke"
        | "stall-source-replace"
        | "stall-owner-drop"
        | "stall-quarantine" => "construct",
        "stall-retire-shutdown" => "retire",
        "helper-crash" => "crash",
        "late-expiry" => "late",
        _ => return Err("unknown retirement scenario".into()),
    };
    own.write(runtime.join("audio-native-stall"), mode)?;
    if scenario == "late-expiry" {
        own.write(runtime.join("audio-deny-kill"), b"trusted EPERM injection")?;
    }
    let started = Instant::now();
    let event = scenario == "stall-event-revoke";
    if event {
        audio.set_event_admitted(true);
    }
    let Request::Started(old) = (if event {
        audio.event()?
    } else {
        audio.preview()?
    }) else {
        return Err("initial preview was not admitted".into());
    };
    if mode != "crash" {
        wait(
            || runtime.join("audio-native-stall-entered").is_file(),
            Duration::from_secs(5),
        )?;
        let before = ticks.get();
        pump(Duration::from_millis(if mode == "retire" {
            50
        } else {
            500
        }));
        assert!(
            ticks.get() >= before + 3,
            "native GTK heartbeat stopped during the injected native-call stall"
        );
        own.copy(
            runtime.join("audio-native-stall-entered"),
            output.join("injected-call.txt"),
        )?;
        audio.set_event_admitted(true);
        for _ in 0..10_000 {
            assert_eq!(audio.event()?, Request::Busy);
        }
        if !event {
            audio.set_event_admitted(false);
            assert!(audio.is_active(), "event disable cancelled a preview");
        }
    } else {
        wait(
            || runtime.join("audio-native-stall-claimed").is_file(),
            Duration::from_secs(5),
        )?;
    }
    let old_pid = audio
        .decoder_snapshot()
        .and_then(|snapshot| snapshot.pid)
        .ok_or("decoder PID was not observed")?;
    own.write(
        output.join("first-decoder-state.txt"),
        format!("{:?}", audio.decoder_snapshot()),
    )?;
    if scenario == "stall-quarantine" {
        own.write(runtime.join("audio-deny-kill"), b"trusted EPERM injection")?;
    }
    let cancellation = Instant::now();
    match scenario {
        "stall-construct-cancel" => audio.cancel(),
        "stall-retire-shutdown" => audio.shutdown(),
        "stall-event-revoke" => audio.set_event_admitted(false),
        "stall-source-replace" => audio.select(sound.clone())?,
        "stall-quarantine" => audio.cancel(),
        "stall-owner-drop" => {
            drop(audio);
            audio = Audio::new()?;
            audio.select(sound.clone())?;
        }
        _ => {}
    }
    let gui_latency = cancellation.elapsed();
    assert!(
        gui_latency < Duration::from_millis(100),
        "GTK cancellation/shutdown blocked"
    );
    if scenario == "stall-quarantine" {
        wait(
            || {
                audio
                    .decoder_snapshot()
                    .is_some_and(|state| state.phase == DecoderPhase::Quarantined)
            },
            Duration::from_millis(750),
        )?;
        pump(Duration::from_millis(100));
        assert!(
            Path::new(&format!("/proc/{old_pid}")).exists(),
            "denied cleanup falsely appeared confirmed"
        );
        for _ in 0..10_000 {
            assert_eq!(audio.preview()?, Request::Busy);
        }
        assert_eq!(
            audio.decoder_snapshot().and_then(|snapshot| snapshot.pid),
            Some(old_pid)
        );
        own.write(
            output.join("quarantined-state.txt"),
            format!("{:?}", audio.decoder_snapshot()),
        )?;
        own.remove_file(runtime.join("audio-deny-kill"))?;
    }
    if scenario == "late-expiry" {
        wait(
            || {
                audio
                    .decoder_snapshot()
                    .is_some_and(|state| state.phase == DecoderPhase::Quarantined)
            },
            Duration::from_secs(14),
        )?;
        pump(Duration::from_millis(100));
        assert!(started.elapsed() >= Duration::from_millis(13_500));
        assert!(started.elapsed() < Duration::from_millis(14_200));
        assert!(!runtime.join("audio-play-invocations").exists());
        own.write(
            runtime.join("audio-native-stall-release"),
            b"return after operation expiry",
        )?;
        use std::os::fd::{AsRawFd, FromRawFd};
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, old_pid as i32, 0) } as i32;
        if fd < 0 {
            return Err("could not observe late helper exit".into());
        }
        let handle = unsafe { fs::File::from_raw_fd(fd) };
        wait(
            || {
                let mut poll = libc::pollfd {
                    fd: handle.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                (unsafe { libc::poll(&mut poll, 1, 0) }) == 1 && poll.revents & libc::POLLIN != 0
            },
            Duration::from_secs(2),
        )?;
        assert!(
            !runtime.join("audio-play-invocations").exists(),
            "expired native construction reached play"
        );
        own.write(output.join("late-refusal.txt"), b"native constructor returned after expiry; pidfd confirmed helper exit; no play call occurred\n")?;
        own.remove_file(runtime.join("audio-deny-kill"))?;
    }
    wait(
        || {
            scenario == "stall-owner-drop" && !Path::new(&format!("/proc/{old_pid}")).exists()
                || audio
                    .decoder_snapshot()
                    .is_some_and(|state| state.phase == DecoderPhase::Finished)
        },
        if scenario == "stall-watchdog" {
            Duration::from_secs(14)
        } else {
            Duration::from_millis(500)
        },
    )?;
    let retirement = cancellation.elapsed();
    pump(Duration::from_millis(30));
    let first = audio.drain_notices();
    assert!(
        !first
            .iter()
            .any(|notice| matches!(notice,Notice::DecoderEnded{id} if *id==old)),
        "stale/unfinished helper was falsely reported ended"
    );
    if scenario == "stall-watchdog" {
        assert!(started.elapsed() < Duration::from_millis(13_600));
        assert!(first
            .iter()
            .any(|notice| matches!(notice, Notice::Failed { .. })));
    }
    if matches!(
        scenario,
        "helper-crash" | "late-expiry" | "stall-quarantine"
    ) {
        assert!(first
            .iter()
            .any(|notice| matches!(notice, Notice::Failed { .. })));
        assert!(
            !first
                .iter()
                .any(|notice| matches!(notice, Notice::Cancelled { .. })),
            "uncertain cleanup was reported as confirmed cancellation"
        );
    }
    own.timed(|| save(&audio, &output))?;
    if let Some(pid) = audio.decoder_snapshot().and_then(|snapshot| snapshot.pid) {
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "owned decoder was not reaped"
        );
    }
    let first_ticks = ticks.get();
    // No latent replacement is replayed once retirement finishes.
    pump(Duration::from_millis(100));
    assert!(!audio.is_active());
    // Phase boundaries on the heartbeat's clock, to place a reported gap;
    // kept in memory for the same reason as the heartbeat itself.
    let phases = std::cell::RefCell::new(String::new());
    let mark = |label: &str| {
        phases
            .borrow_mut()
            .push_str(&format!("{} {label}\n", beat_start.elapsed().as_millis()));
    };
    mark("first-retirement-checked");
    if scenario == "stall-retire-shutdown" {
        drop(audio);
        mark("owner-dropped");
        audio = Audio::new()?;
        audio.select(sound)?;
        mark("owner-recreated");
    }
    let Request::Started(current) = audio.preview()? else {
        return Err("decoder slot did not recover after confirmed retirement".into());
    };
    assert_ne!(old, current);
    audio.cancel_event(old);
    assert!(audio.is_active());
    mark("recovery-preview-started");
    let recovery = wait_for_end(&audio, current)?;
    mark("recovery-ended");
    assert!(!recovery
        .iter()
        .any(|notice| matches!(notice, Notice::Failed { .. })));
    own.write(
        output.join("heartbeat.txt"),
        format!(
            "ticks={} max_gap_ms={} max_gap_end_ms={} net_gap_ms={} own_work_ms={} longest_own_ms={} longest_own_end_ms={}\n{}",
            ticks.get(),
            max_gap.get().as_millis(),
            worst_at.get().as_millis(),
            net_gap.get().as_millis(),
            own.total.get().as_millis(),
            own.longest.get().as_millis(),
            own.longest_end.get().map_or(0, |end| end.duration_since(beat_start).as_millis()),
            phases.borrow()
        ),
    )?;
    assert!(
        net_gap.get() < Duration::from_millis(250),
        "GTK heartbeat exceeded its bound: {} ms net of the probe's own file work, largest raw gap {} ms ending {} ms after start",
        net_gap.get().as_millis(),
        max_gap.get().as_millis(),
        worst_at.get().as_millis()
    );
    assert!(ticks.get() > first_ticks + 20);
    if scenario == "late-expiry" {
        let plays = fs::read_to_string(runtime.join("audio-play-invocations"))?;
        assert_eq!(
            plays.lines().count(),
            1,
            "late operation was replayed alongside explicit recovery"
        );
        own.write(output.join("play-invocations.txt"), plays)?;
    }
    own.write(output.join("parent-maps.txt"), fs::read("/proc/self/maps")?)?;
    let supervisors = fs::read_dir("/proc/self/task")?
        .filter_map(Result::ok)
        .filter(|entry| {
            fs::read_to_string(entry.path().join("comm"))
                .is_ok_and(|name| name.starts_with("among-audio-sup"))
        })
        .count();
    assert_eq!(supervisors, 1, "decoder attempts grew supervisor capacity");
    let operation_temp_dirs_after = operation_directories()?;
    assert_eq!(
        operation_temp_dirs_after, 0,
        "retired helper left an owned temporary subtree"
    );
    own.timed(|| save(&audio, &output))?;
    own.write(
        output.join("retirement.json"),
        serde_json::to_vec_pretty(
            &serde_json::json!({"scenario":scenario,"injection":"trusted native-call shim (plus kill EPERM for quarantine), not a discovered codec deadlock","gui_latency_us":gui_latency.as_micros(),"retirement_us":retirement.as_micros(),"elapsed_ms":started.elapsed().as_millis(),"gtk_ticks":ticks.get(),"max_gtk_gap_ms":max_gap.get().as_millis(),"supervisor_threads":supervisors,"operation_temp_dirs_after":operation_temp_dirs_after,"first_notices":format!("{first:?}"),"recovery_notices":format!("{recovery:?}")}),
        )?,
    )?;
    audio.shutdown();
    timer.remove();
    window.close();
    Ok(())
}
