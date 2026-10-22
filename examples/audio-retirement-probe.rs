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
fn wait(mut condition: impl FnMut() -> bool, limit: Duration) -> Result<(), Box<dyn Error>> {
    let started = Instant::now();
    while !condition() {
        if started.elapsed() >= limit {
            return Err("retirement observation exceeded its bound".into());
        }
        pump(Duration::from_millis(5));
    }
    Ok(())
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
    let heartbeat = output.join("heartbeat.txt");
    let timer = glib::timeout_add_local(Duration::from_millis(10), move || {
        let now = Instant::now();
        gap.set(gap.get().max(now.duration_since(previous.replace(now))));
        count.set(count.get() + 1);
        let _ = fs::write(&heartbeat, count.get().to_string());
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
    fs::write(runtime.join("audio-native-stall"), mode)?;
    if scenario == "late-expiry" {
        fs::write(runtime.join("audio-deny-kill"), b"trusted EPERM injection")?;
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
        fs::copy(
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
    fs::write(
        output.join("first-decoder-state.txt"),
        format!("{:?}", audio.decoder_snapshot()),
    )?;
    if scenario == "stall-quarantine" {
        fs::write(runtime.join("audio-deny-kill"), b"trusted EPERM injection")?;
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
        fs::write(
            output.join("quarantined-state.txt"),
            format!("{:?}", audio.decoder_snapshot()),
        )?;
        fs::remove_file(runtime.join("audio-deny-kill"))?;
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
        fs::write(
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
        fs::write(output.join("late-refusal.txt"), b"native constructor returned after expiry; pidfd confirmed helper exit; no play call occurred\n")?;
        fs::remove_file(runtime.join("audio-deny-kill"))?;
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
    save(&audio, &output)?;
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
    if scenario == "stall-retire-shutdown" {
        drop(audio);
        audio = Audio::new()?;
        audio.select(sound)?;
    }
    let Request::Started(current) = audio.preview()? else {
        return Err("decoder slot did not recover after confirmed retirement".into());
    };
    assert_ne!(old, current);
    audio.cancel_event(old);
    assert!(audio.is_active());
    pump(Duration::from_secs(3));
    let recovery = audio.drain_notices();
    assert!(
        recovery
            .iter()
            .any(|notice| matches!(notice,Notice::DecoderEnded{id} if *id==current)),
        "real output operation did not retire successfully after recovery"
    );
    assert!(!recovery
        .iter()
        .any(|notice| matches!(notice, Notice::Failed { .. })));
    assert!(
        max_gap.get() < Duration::from_millis(250),
        "GTK heartbeat exceeded its bound"
    );
    assert!(ticks.get() > first_ticks + 20);
    if scenario == "late-expiry" {
        let plays = fs::read_to_string(runtime.join("audio-play-invocations"))?;
        assert_eq!(
            plays.lines().count(),
            1,
            "late operation was replayed alongside explicit recovery"
        );
        fs::write(output.join("play-invocations.txt"), plays)?;
    }
    fs::write(output.join("parent-maps.txt"), fs::read("/proc/self/maps")?)?;
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
    save(&audio, &output)?;
    fs::write(
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
