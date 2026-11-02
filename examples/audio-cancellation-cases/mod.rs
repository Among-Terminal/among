//! cancellation before a first play, with independent silent/recovery runs.
use super::{pump, wait};
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

struct Release(PathBuf);
impl Drop for Release {
    fn drop(&mut self) {
        let _ = fs::write(self.0.join("audio-fork-release"), b"release owned fixture");
        let _ = fs::write(
            self.0.join("audio-native-stall-release"),
            b"release owned fixture",
        );
        let _ = fs::remove_file(self.0.join("audio-deny-kill"));
    }
}

/// Fixture and evidence files written by this probe on the GTK thread. Under
/// writeback pressure such a write blocks in the kernel, and the heartbeat would
/// charge that delay to Among. The time is measured here so the bound applies to
/// the application alone, and the raw gap stays on record beside it.
#[derive(Default)]
struct OwnWork {
    total: Rc<Cell<Duration>>,
    longest: Cell<Duration>,
    longest_end: Cell<Option<Instant>>,
}

impl OwnWork {
    fn timed<T>(&self, work: impl FnOnce() -> T) -> T {
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

    fn write(&self, path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
        self.timed(|| fs::write(path, bytes))
    }

    fn copy(&self, from: impl AsRef<Path>, to: impl AsRef<Path>) -> std::io::Result<u64> {
        self.timed(|| fs::copy(from, to))
    }

    fn remove_file(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        self.timed(|| fs::remove_file(path))
    }
}

pub(super) fn run(
    scenario: &str,
    output: &Path,
    input: &Path,
    root: Option<&Path>,
    runtime: &Path,
) -> Result<(), Box<dyn Error>> {
    let (case, recovery) = scenario
        .strip_suffix("-recovery")
        .map_or((scenario, false), |case| (case, true));
    if !matches!(
        case,
        "cancel-before-spawn"
            | "cancel-late-spawn"
            | "cancel-constructor"
            | "cancel-pending-permit"
            | "cancel-late-spawn-event-revoke"
            | "cancel-late-spawn-source-replace"
            | "cancel-late-spawn-shutdown"
            | "cancel-late-spawn-owner-drop"
    ) {
        return Err("unknown cancellation scenario".into());
    }
    among::audio::initialize_execution(root)?;
    let _exit = super::ExecutionExit(runtime.to_path_buf());
    let _release = Release(runtime.to_path_buf());
    let child = std::process::Command::new("/bin/cat")
        .arg("/proc/self/maps")
        .output()?;
    if !child.status.success() {
        return Err("host-child observation failed".into());
    }
    fs::write(output.join("host-child-maps.txt"), child.stdout)?;
    gtk::init()?;
    let window = gtk::Window::builder()
        .title("Owned pre-play cancellation probe")
        .default_width(320)
        .default_height(120)
        .build();
    window.present();
    let ticks = Rc::new(Cell::new(0u64));
    let gap = Rc::new(Cell::new(Duration::ZERO));
    let last = Rc::new(Cell::new(Instant::now()));
    let (count, maximum, previous) = (ticks.clone(), gap.clone(), last.clone());
    let own = OwnWork::default();
    let net_gap = Rc::new(Cell::new(Duration::ZERO));
    let (spent, net_maximum, seen) = (
        own.total.clone(),
        net_gap.clone(),
        Cell::new(Duration::ZERO),
    );
    // The end of the largest gap, on this clock, places a failure against the
    // phase marks written next to the bound below.
    let beat_start = Instant::now();
    let worst_at = Rc::new(Cell::new(Duration::ZERO));
    let worst = worst_at.clone();
    let timer = glib::timeout_add_local(Duration::from_millis(10), move || {
        let now = Instant::now();
        let elapsed = now.duration_since(previous.replace(now));
        if elapsed > maximum.get() {
            maximum.set(elapsed);
            worst.set(now.duration_since(beat_start));
        }
        let work = spent.get();
        let net = elapsed.saturating_sub(work.saturating_sub(seen.replace(work)));
        net_maximum.set(net_maximum.get().max(net));
        count.set(count.get() + 1);
        glib::ControlFlow::Continue
    });
    let phases = std::cell::RefCell::new(String::new());
    let mark = |label: &str| {
        phases
            .borrow_mut()
            .push_str(&format!("{} {label}\n", beat_start.elapsed().as_millis()));
    };
    let sound = Sound::read_local(input)?;
    let mut audio = Audio::new()?;
    audio.select(sound.clone())?;
    pump(Duration::from_millis(100));
    let before_spawn = case == "cancel-before-spawn";
    let denied = case == "cancel-constructor";
    let event = case == "cancel-late-spawn-event-revoke";
    let owner_drop = case == "cancel-late-spawn-owner-drop";
    own.write(runtime.join("audio-native-stall"), b"late")?;
    if !denied {
        own.write(
            runtime.join("audio-fork-stall"),
            if before_spawn { "before" } else { "after" },
        )?;
    }
    if event {
        audio.set_event_admitted(true);
    }
    let admitted = Instant::now();
    let Request::Started(old) = (if event {
        audio.event()?
    } else {
        audio.preview()?
    }) else {
        return Err("initial request was not admitted".into());
    };
    if !denied {
        wait(
            || runtime.join("audio-fork-entered").exists(),
            Duration::from_secs(5),
        )?;
        assert!(audio
            .decoder_snapshot()
            .is_some_and(|state| state.pid.is_none()));
    }
    if !before_spawn {
        wait(
            || runtime.join("audio-native-stall-entered").exists(),
            Duration::from_secs(5),
        )?;
        own.copy(
            runtime.join("audio-native-stall-entered"),
            output.join("injected-call.txt"),
        )?;
    }
    if case == "cancel-pending-permit" {
        own.write(
            runtime.join("audio-native-stall-release"),
            b"complete real construction before cancellation",
        )?;
        wait(
            || runtime.join("audio-native-returned").exists(),
            Duration::from_secs(3),
        )?;
        pump(Duration::from_millis(150));
        assert!(
            !runtime.join("audio-play-invocations").exists(),
            "helper played before spawn ownership/grant"
        );
        own.copy(
            runtime.join("audio-native-returned"),
            output.join("constructed-before-permit.txt"),
        )?;
    }
    if denied {
        own.write(runtime.join("audio-deny-kill"), b"trusted EPERM fixture")?;
    }
    let old_snapshot = audio.decoder_snapshot();
    let cancel = Instant::now();
    match case {
        "cancel-late-spawn-event-revoke" => audio.set_event_admitted(false),
        "cancel-late-spawn-source-replace" => audio.select(sound.clone())?,
        "cancel-late-spawn-shutdown" => audio.shutdown(),
        "cancel-late-spawn-owner-drop" => {
            drop(audio);
            audio = Audio::new()?;
            audio.select(sound.clone())?;
        }
        _ => audio.cancel(),
    }
    let gui_latency = cancel.elapsed();
    assert!(gui_latency < Duration::from_millis(100));
    if owner_drop {
        pump(Duration::from_millis(600));
    } else {
        wait(
            || {
                audio
                    .decoder_snapshot()
                    .is_some_and(|state| state.phase == DecoderPhase::Quarantined)
            },
            Duration::from_secs(1),
        )?;
    }
    assert!(!runtime.join("audio-play-invocations").exists());
    assert!(
        admitted.elapsed() < Duration::from_secs(8),
        "fixture did not release before the original deadline"
    );
    own.write(
        runtime.join("audio-native-stall-release"),
        b"return real constructor before original expiry",
    )?;
    if before_spawn {
        own.write(
            runtime.join("audio-fork-release"),
            b"execute the real fork after cancellation",
        )?;
    }
    mark("native-call-released");
    pump(Duration::from_millis(1600));
    mark("expiry-window-pumped");
    let plays_before_recovery =
        fs::read_to_string(runtime.join("audio-play-invocations")).unwrap_or_default();
    own.write(output.join("before-recovery.txt"), format!("old={old} elapsed_ms={} plays={plays_before_recovery:?} old_snapshot={old_snapshot:?} current={:?}\n",admitted.elapsed().as_millis(),audio.decoder_snapshot()))?;
    // This assertion precedes any second Preview and any recovery capture.
    assert!(
        plays_before_recovery.is_empty(),
        "cancelled request reached real play before expiry"
    );
    if !denied {
        own.write(
            runtime.join("audio-fork-release"),
            b"return the real child handle",
        )?;
    } else {
        own.remove_file(runtime.join("audio-deny-kill"))?;
    }
    if owner_drop {
        wait(
            || {
                fs::read_to_string(runtime.join("audio-fork-real"))
                    .ok()
                    .and_then(|value| value.parse::<u32>().ok())
                    .is_some_and(|pid| !Path::new(&format!("/proc/{pid}")).exists())
            },
            Duration::from_secs(1),
        )?;
    } else {
        wait(
            || {
                audio
                    .decoder_snapshot()
                    .is_some_and(|state| state.phase == DecoderPhase::Finished)
            },
            Duration::from_secs(1),
        )?;
    }
    let retired = audio.decoder_snapshot();
    if let Some(pid) = retired.as_ref().and_then(|state| state.pid) {
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }
    if !denied {
        own.copy(
            runtime.join("audio-fork-real"),
            output.join("actual-fork-pid.txt"),
        )?;
    }
    pump(Duration::from_millis(50));
    let first_notices = audio.drain_notices();
    assert!(
        !first_notices
            .iter()
            .any(|notice| matches!(notice, Notice::DecoderEnded { .. })),
        "cancelled first operation falsely ended normally"
    );
    own.write(
        output.join("cancelled-complete.txt"),
        format!("{retired:?}\n{first_notices:?}\n"),
    )?;
    mark("first-operation-retired");
    let mut recovery_notices = Vec::new();
    if recovery {
        for name in ["audio-fork-stall", "audio-native-stall"] {
            match own.remove_file(runtime.join(name)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        if case == "cancel-late-spawn-shutdown" {
            drop(audio);
            audio = Audio::new()?;
            audio.select(sound)?;
        }
        if event {
            audio.set_event_admitted(true);
        }
        let Request::Started(current) = (if event {
            audio.event()?
        } else {
            audio.preview()?
        }) else {
            return Err("explicit recovery was not admitted after confirmed retirement".into());
        };
        assert_ne!(old, current);
        audio.cancel_event(old);
        pump(Duration::from_secs(3));
        recovery_notices = audio.drain_notices();
        assert!(recovery_notices
            .iter()
            .any(|notice| matches!(notice,Notice::DecoderEnded{id} if *id==current)));
        assert!(!recovery_notices
            .iter()
            .any(|notice| matches!(notice, Notice::Failed { .. })));
        let plays = fs::read_to_string(runtime.join("audio-play-invocations"))?;
        assert_eq!(
            plays.lines().count(),
            1,
            "a cancelled operation played alongside recovery"
        );
        own.write(output.join("recovery-play-invocations.txt"), plays)?;
    } else {
        pump(Duration::from_millis(500));
        assert!(!runtime.join("audio-play-invocations").exists());
    }
    let supervisors = fs::read_dir("/proc/self/task")?
        .filter_map(Result::ok)
        .filter(|entry| {
            fs::read_to_string(entry.path().join("comm"))
                .is_ok_and(|name| name.starts_with("among-audio-sup"))
        })
        .count();
    assert_eq!(supervisors, 1);
    let temporary = super::operation_directories()?;
    assert_eq!(temporary, 0);
    mark("finished");
    own.write(
        output.join("heartbeat.txt"),
        format!(
            "ticks={} max_gap_ms={} max_gap_end_ms={} net_gap_ms={} own_work_ms={} longest_own_ms={} longest_own_end_ms={}\n{}",
            ticks.get(),
            gap.get().as_millis(),
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
        gap.get().as_millis(),
        worst_at.get().as_millis()
    );
    assert!(ticks.get() > 100);
    super::save(&audio, output)?;
    own.write(
        output.join("retirement.json"),
        serde_json::to_vec_pretty(
            &serde_json::json!({"scenario":scenario,"gui_latency_us":gui_latency.as_micros(),"max_gtk_gap_ms":net_gap.get().as_millis(),"raw_gtk_gap_ms":gap.get().as_millis(),"gtk_ticks":ticks.get(),"supervisor_threads":supervisors,"operation_temp_dirs_after":temporary,"cancelled_plays":plays_before_recovery,"recovery":recovery,"first_notices":format!("{first_notices:?}"),"recovery_notices":format!("{recovery_notices:?}"),"injection":"real fork observation and real native-constructor call-site barriers; EPERM only in cancel-constructor"}),
        )?,
    )?;
    audio.shutdown();
    timer.remove();
    window.close();
    Ok(())
}
