//! Real output from the application itself, driven through General.
//!
//! A test binary cannot host the decoder helper, so its bells and Preview are
//! silent even when the test passes. This check runs the actual executable
//! with saved General preferences and scripted real controls, records the
//! private sink's monitor, and judges the waveform.

use crate::{audio_capture, process::OwnedProcess, sessions, storage, Result};
use nix::sys::signal::Signal;
use serde_json::{json, Value};
use std::{
    fs::{self, File},
    os::unix::fs::DirBuilderExt,
    path::Path,
    time::Duration,
};

const RATE: u32 = 48_000;
const BELL: &str = "type:printf~'\\007'";

/// What a capture must show. The built-in alert carries 660 and 880 Hz; the
/// local fixture carries only 880 Hz, so 660 Hz tells the two sources apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expected {
    Silence,
    Local,
    Builtin,
    /// The local fixture, cut short: less than this many seconds of it.
    LocalCutBelow(u32),
}

struct Scenario {
    name: &'static str,
    /// The saved `audio` object; `None` leaves General unwritten.
    audio: Option<Value>,
    steps: &'static [&'static str],
    expected: Expected,
}

/// A PCM16 mono sine fixture.
pub fn sine_wav(seconds: u32, frequency: f64) -> Vec<u8> {
    let frames = RATE * seconds;
    let mut data = Vec::with_capacity(frames as usize * 2);
    for frame in 0..frames {
        let phase = f64::from(frame) * frequency * std::f64::consts::TAU / f64::from(RATE);
        data.extend(((phase.sin() * 8000.0) as i16).to_le_bytes());
    }
    let mut wav = b"RIFF".to_vec();
    wav.extend((36 + data.len() as u32).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(RATE.to_le_bytes());
    wav.extend((RATE * 2).to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend((data.len() as u32).to_le_bytes());
    wav.extend(data);
    wav
}

fn magnitude(samples: &[f64], frequency: f64) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for (index, sample) in samples.iter().enumerate() {
        let angle = index as f64 * frequency * std::f64::consts::TAU / f64::from(RATE);
        re += sample * angle.cos();
        im += sample * angle.sin();
    }
    re.hypot(im) / samples.len().max(1) as f64
}

/// Judges one S16LE capture against its expectation.
pub fn judge(bytes: &[u8], expected: Expected) -> Result<Value> {
    if bytes.len() < 19_200 {
        return Err("capture lacks a nonempty baseline".into());
    }
    let samples: Vec<f64> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| f64::from(i16::from_le_bytes([b[0], b[1]])))
        .collect();
    // The span from first to last sound. Dropping each zero sample instead would
    // shift the phase at every zero crossing and hide the tone being measured.
    let first = samples.iter().position(|sample| *sample != 0.0);
    let last = samples.iter().rposition(|sample| *sample != 0.0);
    let active = match (first, last) {
        (Some(first), Some(last)) => &samples[first..=last],
        _ => &[],
    };
    let tone_880 = magnitude(active, 880.0);
    let tone_660 = magnitude(active, 660.0);
    let seconds = active.len() as f64 / f64::from(RATE);
    let result = json!({"bytes":bytes.len(),"nonzero":active.len(),"active_seconds":seconds,
        "magnitude_660":tone_660,"magnitude_880":tone_880,"expected":format!("{expected:?}")});
    let fine = match expected {
        Expected::Silence => active.is_empty(),
        Expected::Local => active.len() > 1000 && tone_880 > 500.0 && tone_660 < tone_880 * 0.1,
        Expected::Builtin => active.len() > 1000 && tone_660 > tone_880 * 0.3,
        Expected::LocalCutBelow(limit) => {
            active.len() > 1000
                && tone_880 > 500.0
                && tone_660 < tone_880 * 0.1
                && seconds < f64::from(limit)
        }
    };
    if !fine {
        return Err(format!("captured waveform differs from expectation: {result}").into());
    }
    Ok(result)
}

fn audio(enabled: bool, eligibility: &str, source: &Value) -> Option<Value> {
    Some(json!({"enabled":enabled,"eligibility":eligibility,"source":source}))
}

fn scenarios(short: &Path, long: &Path, missing: &Path) -> Vec<Scenario> {
    let local = |path: &Path| json!({"local":path});
    vec![
        Scenario {
            name: "preview-off",
            audio: None,
            steps: &["preferences", "activate:notification-audio-preview"],
            expected: Expected::Builtin,
        },
        Scenario {
            name: "event",
            audio: audio(true, "always", &local(short)),
            steps: &[BELL],
            expected: Expected::Local,
        },
        Scenario {
            name: "event-disabled",
            audio: audio(false, "always", &local(short)),
            steps: &[BELL],
            expected: Expected::Silence,
        },
        Scenario {
            name: "event-ineligible",
            audio: audio(true, "application-background", &local(short)),
            steps: &[BELL],
            expected: Expected::Silence,
        },
        Scenario {
            name: "source-change",
            audio: audio(true, "always", &local(short)),
            steps: &["preferences", "activate:notification-audio-builtin", BELL],
            expected: Expected::Builtin,
        },
        Scenario {
            name: "file-removed",
            audio: audio(true, "always", &local(missing)),
            steps: &[BELL],
            expected: Expected::Silence,
        },
        Scenario {
            name: "pane-removed",
            audio: audio(true, "always", &local(long)),
            steps: &["split", BELL, "close"],
            expected: Expected::LocalCutBelow(4),
        },
        // SIGTERM during a long sound ends the application; a decoder surviving
        // its owner would keep playing past the bound.
        Scenario {
            name: "shutdown",
            audio: audio(true, "always", &local(long)),
            steps: &[BELL],
            expected: Expected::LocalCutBelow(4),
        },
    ]
}

fn run_one(binary: &Path, scratch: &Path, scenario: &Scenario) -> Result<Value> {
    let mut harness = sessions::fresh(binary, scratch, &format!("audio-{}", scenario.name))?;
    let base = harness.base.clone();
    fs::DirBuilder::new()
        .mode(0o700)
        .create(base.join("runtime"))?;
    let socket = format!("unix:{}", audio_capture::owned_socket(&base)?.display());
    for (key, value) in [
        ("XDG_RUNTIME_DIR", base.join("runtime").into_os_string()),
        ("PULSE_SERVER", socket.into()),
        ("PULSE_SINK", "among_audit".into()),
        ("GSK_RENDERER", "cairo".into()),
    ] {
        harness.env.insert(key.into(), value);
    }
    let general = base.join("config/among/general.json");
    if let Some(audio) = &scenario.audio {
        fs::write(
            &general,
            serde_json::to_vec(&json!({"version":1,"notifications":{"audio":audio}}))?,
        )?;
    }
    fs::write(base.join("steps"), scenario.steps.join("\n") + "\n")?;
    harness
        .env
        .insert("AMONG_SCRIPT".into(), base.join("steps").into_os_string());
    let mut pulse = audio_capture::private_server(&base, &harness.env, "debug")?;
    let pcm = base.join("capture.s16le");
    let mut recorder = OwnedProcess::spawn_split(
        Path::new("/usr/bin/parec"),
        &[
            "--device=among_audit.monitor",
            "--format=s16le",
            "--rate=48000",
            "--channels=1",
            "--raw",
            "--latency-msec=20",
            "--process-time-msec=10",
        ],
        &harness.env,
        &base,
        &File::create(&pcm)?,
        &File::create(base.join("recorder.log"))?,
    )?;
    std::thread::sleep(Duration::from_millis(500));
    harness.start(scenario.name)?;
    harness.wait(30, |this| {
        Ok(this.text()?.contains("among: script finished after"))
    })?;
    if scenario.name == "shutdown" {
        harness.stop(false)?;
        std::thread::sleep(Duration::from_secs(5));
    } else {
        std::thread::sleep(Duration::from_secs(3));
        harness.stop(false)?;
    }
    std::thread::sleep(Duration::from_millis(500));
    recorder.signal(Signal::SIGINT)?;
    recorder.wait(Duration::from_secs(5))?;
    if !pulse.is_running()? {
        return Err(format!("{}: the audio server died", scenario.name).into());
    }
    pulse.cleanup()?;
    let log = fs::read_to_string(base.join("pulse.log"))?;
    let copied = log.matches("Negotiated SHM: no").count();
    let verdict = judge(&fs::read(&pcm)?, scenario.expected);
    let written = fs::read(&general).ok();
    fs::write(
        base.join("analysis.json"),
        serde_json::to_vec_pretty(&json!({"scenario":scenario.name,
            "capture":verdict.as_ref().ok(),"error":verdict.as_ref().err().map(ToString::to_string),
            "copied_connections":copied}))?,
    )?;
    let capture = verdict.map_err(|error| format!("{}: {error}", scenario.name))?;
    if scenario.expected != Expected::Silence && copied == 0 {
        return Err(format!("{}: helper negotiated shared memory", scenario.name).into());
    }
    match scenario.name {
        "preview-off" if written.is_some() => {
            return Err("Preview wrote General preferences".into())
        }
        "source-change" => {
            let saved = storage::parse_json(&written.ok_or("General disappeared")?)?;
            if saved["notifications"]["audio"]["source"] != "builtin" {
                return Err("source change was not saved".into());
            }
        }
        _ => {}
    }
    println!("PASS {}: {capture}", scenario.name);
    Ok(
        json!({"scenario":scenario.name,"capture":capture,"copied_connections":copied,
        "evidence":base}),
    )
}

/// Runs every General audio scenario on the current owned display.
pub fn run(binary: &Path, scratch: &Path) -> Result<()> {
    audio_capture::require_container()?;
    fs::create_dir_all(scratch)?;
    let fixtures = storage::private_directory(scratch, "audio-general-fixtures")?;
    let (short, long) = (fixtures.join("short.wav"), fixtures.join("long.wav"));
    fs::write(&short, sine_wav(1, 880.0))?;
    fs::write(&long, sine_wav(8, 880.0))?;
    let missing = fixtures.join("removed.wav");
    let mut results = Vec::new();
    for scenario in scenarios(&short, &long, &missing) {
        results.push(run_one(binary, scratch, &scenario)?);
    }
    fs::write(
        fixtures.join("result.json"),
        serde_json::to_vec_pretty(&json!({"cases":results}))?,
    )?;
    println!(
        "Captured General audio: {}. Focus-driven cancellation is not covered.",
        fixtures.display()
    );
    Ok(())
}
