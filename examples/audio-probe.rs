//! Actual adapter consumer for owned null-sink acceptance, not General UI.
use among::audio::{Audio, AudioError, Notice, Request, Sound};
use std::{
    cell::Cell,
    error::Error,
    fs,
    path::PathBuf,
    process::Command,
    rc::Rc,
    time::{Duration, Instant},
};

fn spin(duration: Duration) {
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

struct Evidence(PathBuf);
impl Drop for Evidence {
    fn drop(&mut self) {
        if let Ok(maps) = fs::read("/proc/self/maps") {
            let _ = fs::write(self.0.join("exit-maps.txt"), maps);
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    if !std::path::Path::new("/.dockerenv").exists()
        || ["/dev/snd", "/dev/dsp", "/dev/audio"]
            .iter()
            .any(|name| std::path::Path::new(name).exists())
    {
        return Err("audio probe requires an owned device-free container".into());
    }
    let socket = format!("unix:{}/pulse.sock", std::env::var("XDG_RUNTIME_DIR")?);
    if std::env::var("PULSE_SERVER").as_deref() != Ok(socket.as_str()) {
        return Err("probe must use its owned runtime socket".into());
    }
    let info = Command::new("pactl").arg("info").output()?;
    let sinks = Command::new("pactl")
        .args(["list", "short", "sinks"])
        .output()?;
    let modules = Command::new("pactl")
        .args(["list", "short", "modules"])
        .output()?;
    if !info.status.success() || !sinks.status.success() || !modules.status.success() {
        return Err("private null-sink inspection failed; refusing playback".into());
    }
    let info = String::from_utf8(info.stdout)?;
    let sinks = String::from_utf8(sinks.stdout)?;
    let modules = String::from_utf8(modules.stdout)?;
    if !info.contains(&socket)
        || !info.contains("Default Sink: among_audit")
        || sinks.lines().count() != 1
        || !sinks.contains("among_audit\tmodule-null-sink.c")
        || modules.lines().count() != 2
        || !modules.contains("module-native-protocol-unix")
        || !modules.contains("module-null-sink")
    {
        return Err("private null-sink identity not confirmed; refusing playback".into());
    }
    let mut args = std::env::args_os().skip(1);
    let scenario = args.next().ok_or("scenario required")?;
    let output = PathBuf::from(args.next().ok_or("new output directory required")?);
    fs::create_dir(&output)?;
    let _evidence = Evidence(output.clone());
    fs::write(
        output.join("process-context.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "compiled_arch":std::env::consts::ARCH,
            "executable":fs::read_link("/proc/self/exe")?,
            "confinement":fs::read_to_string("/proc/self/attr/current")?,
            "snap":std::env::var("SNAP").ok(),
            "snap_revision":std::env::var("SNAP_REVISION").ok(),
            "scope":"external audio library probe; not General controls"
        }))?,
    )?;
    fs::write(
        output.join("mountinfo.txt"),
        fs::read("/proc/self/mountinfo")?,
    )?;
    let input = args
        .next()
        .map(PathBuf::from)
        .ok_or("bounded fixture WAV required")?;
    let root = args.next().map(PathBuf::from);
    let keys = [
        "GTK_PATH",
        "GTK_MEDIA",
        "GST_PLUGIN_SYSTEM_PATH_1_0",
        "GST_PLUGIN_PATH_1_0",
        "GST_PLUGIN_SCANNER_1_0",
        "GST_REGISTRY_1_0",
        "GST_REGISTRY_UPDATE",
        "GST_REGISTRY_FORK",
        "LD_LIBRARY_PATH",
        "LD_PRELOAD",
    ];
    let before: Vec<_> = keys.iter().map(std::env::var_os).collect();
    if let Some(root) = root {
        // Single-threaded startup; no application workers or host processes yet.
        unsafe {
            among::audio::runtime::initialize_packaged(&root)?;
        }
    } else {
        gtk::init()?;
    }
    let after: Vec<_> = keys.iter().map(std::env::var_os).collect();
    if before != after {
        return Err("bundle discovery leaked environment overrides".into());
    }
    fs::write(
        output.join("environment-restored.txt"),
        format!("{keys:?}\n{after:?}\n"),
    )?;
    let child = Command::new("/bin/cat").arg("/proc/self/maps").output()?;
    fs::write(output.join("host-child-maps.txt"), &child.stdout)?;
    let mut audio = Audio::new()?;
    assert!(matches!(Audio::new(), Err(AudioError::AlreadyOwned)));
    let selected = Rc::new(Cell::new(false));
    let seen = selected.clone();
    audio.select_local(input.clone(), move |result| {
        eprintln!("selection={result:?}");
        seen.set(result.is_ok());
    })?;
    if audio.select_local(input, |_| {}).err() != Some(AudioError::Busy) {
        return Err("file read was not bounded to one in flight".into());
    }
    let start = Instant::now();
    while !selected.get() && start.elapsed() < Duration::from_secs(5) {
        spin(Duration::from_millis(10));
    }
    if !selected.get() {
        return Err("selection did not finish".into());
    }
    let scenario = scenario.to_str().ok_or("scenario encoding")?;
    match scenario {
        "stale-owner-id" => {
            audio.set_event_admitted(true);
            let Request::Started(old) = audio.event()? else {
                return Err("event not admitted".into());
            };
            audio.shutdown();
            drop(audio);
            audio = Audio::new()?;
            audio.set_event_admitted(true);
            let Request::Started(current) = audio.event()? else {
                return Err("new owner event not admitted".into());
            };
            assert_ne!(old, current);
            audio.cancel_event(old);
            assert!(audio.is_active());
        }
        "stale-event-cancel" => {
            audio.set_event_admitted(true);
            let Request::Started(old) = audio.event()? else {
                return Err("event not admitted".into());
            };
            assert!(matches!(audio.preview()?, Request::Started(_)));
            audio.cancel_event(old);
            assert!(audio.is_active());
        }
        "preview-off" | "snapshot" | "cancel" | "shutdown" | "replace" | "failure" => {
            audio.set_event_admitted(false);
            assert_eq!(audio.event()?, Request::Suppressed);
            if scenario == "snapshot" {
                let path = output.join("owned-selection.wav");
                fs::write(&path, Sound::builtin()?.bytes())?;
                let snapshot = Sound::read_local(&path)?;
                audio.select(snapshot)?;
                fs::remove_file(&path)?;
                fs::write(&path, b"replaced after validation")?;
            }
            assert!(matches!(audio.preview()?, Request::Started(_)));
            audio.set_event_admitted(false);
            assert!(
                audio.is_active(),
                "event disable must not cancel explicit preview"
            );
            if matches!(scenario, "cancel" | "shutdown" | "replace") {
                spin(Duration::from_secs(3));
                fs::write(
                    output.join("before-cancel-maps.txt"),
                    fs::read("/proc/self/maps")?,
                )?;
                match scenario {
                    "cancel" => audio.cancel(),
                    "shutdown" => {
                        audio.shutdown();
                        assert_eq!(audio.preview().err(), Some(AudioError::Closed));
                    }
                    "replace" => {
                        audio.select(Sound::builtin()?)?;
                        assert!(!audio.is_active());
                        assert!(matches!(audio.preview()?, Request::Started(_)));
                    }
                    _ => return Err("unexpected cancellation scenario".into()),
                }
            }
        }
        "event-enabled" | "event-revoke" => {
            audio.set_event_admitted(true);
            assert!(matches!(audio.event()?, Request::Started(_)));
            for _ in 0..10_000 {
                assert_eq!(audio.event()?, Request::Busy);
            }
            if scenario == "event-revoke" {
                spin(Duration::from_secs(3));
                audio.set_event_admitted(false);
                assert!(!audio.is_active());
                assert_eq!(audio.event()?, Request::Suppressed);
            }
        }
        "event-disabled" | "event-ineligible" => {
            audio.set_event_admitted(false);
            for _ in 0..10_000 {
                assert_eq!(audio.event()?, Request::Suppressed);
            }
            assert!(!audio.is_active());
        }
        "stale-selection" => {
            let called = Rc::new(Cell::new(false));
            let callback = called.clone();
            let file = output.join("obsolete.wav");
            fs::write(&file, Sound::builtin()?.bytes())?;
            audio.select_local(file, move |_| callback.set(true))?;
            audio.cancel();
            audio.select(Sound::builtin()?)?;
            assert!(!audio.is_active());
            assert!(matches!(audio.preview()?, Request::Started(_)));
            spin(Duration::from_millis(500));
            assert!(!called.get());
        }
        _ => return Err("unknown audio probe scenario".into()),
    }
    spin(Duration::from_secs(4));
    fs::write(output.join("maps.txt"), fs::read("/proc/self/maps")?)?;
    let notices = audio.drain_notices();
    assert!(notices.len() <= 32);
    fs::write(output.join("notices.txt"), format!("{notices:#?}\n"))?;
    println!(
        "scenario={scenario} selected={:?} active={} notices={notices:?}",
        audio.selected_info(),
        audio.is_active()
    );
    let failed = notices
        .iter()
        .any(|notice| matches!(notice, Notice::Failed { .. }));
    audio.shutdown();
    if scenario == "failure" {
        if !failed {
            return Err("expected decoder failure was not observed".into());
        }
    } else if failed {
        return Err("native media reported a failure; no sound success is claimed".into());
    }
    Ok(())
}
