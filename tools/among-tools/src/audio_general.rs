//! Real output from the application itself, driven through General.
//!
//! A test binary cannot host the decoder helper, so its bells and Preview are
//! silent even when the test passes. This check runs the actual executable
//! with saved General preferences and scripted real controls, records the
//! private sink's monitor, and judges the waveform.

use crate::{audio_capture, process::OwnedProcess, sessions, storage, Result};
use nix::{
    sys::signal::{kill, Signal},
    unistd::Pid,
};
use serde_json::{json, Value};
use std::{
    fs::{self, File},
    os::unix::fs::DirBuilderExt,
    path::Path,
    time::Duration,
};

const RATE: u32 = 48_000;
const BELL: &str = "type:printf~'\\007'";
const ENABLED: &str = "focus:notification-audio-enabled";
const CONDITION: &str = "focus:notification-audio-condition";
const PATH: &str = "focus:notification-audio-path";
const DECODER_FLAG: &[u8] = b"--among-audio-decoder-v2";
/// Holds the script until a sound is audible, so a cancellation that follows
/// cuts a sound that was playing and not one still being prepared.
const SOUNDING: &str = "wait-sound";

/// What a capture must show. The built-in alert carries 660 and 880 Hz; the
/// local fixture carries only 880 Hz, so 660 Hz tells the two sources apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expected {
    Silence,
    Local,
    Builtin,
    /// The local fixture, cut short: less than this many seconds of it.
    LocalCutBelow(u32),
    /// The local fixture cut short and then the built-in alert, all within this
    /// many seconds.
    LocalCutThenBuiltin(u32),
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
        Expected::LocalCutThenBuiltin(limit) => {
            active.len() > 1000 && tone_880 > 50.0 && tone_660 > 50.0 && seconds < f64::from(limit)
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

fn scenarios(short: &Path, long: &Path, missing: &Path, truncated: &Path) -> Vec<Scenario> {
    let local = |path: &Path| json!({"local":path});
    vec![
        Scenario {
            name: "preview-off",
            audio: None,
            steps: &["preferences", "activate:notification-audio-preview"],
            expected: Expected::Builtin,
        },
        // With a working command channel configured, Preview must still reach
        // nothing but the sound; the control scenario shows the same
        // configuration does run on a bell.
        Scenario {
            name: "preview-isolated",
            audio: None,
            steps: &["preferences", "activate:notification-audio-preview"],
            expected: Expected::Builtin,
        },
        Scenario {
            name: "command-control",
            audio: None,
            steps: &[BELL],
            expected: Expected::Silence,
        },
        // The switch and condition below are operated from the keyboard. Only
        // the source is saved ahead, because the file chooser is a portal.
        Scenario {
            name: "event",
            audio: audio(false, "always", &local(short)),
            steps: &["preferences", ENABLED, "keys:space", BELL],
            expected: Expected::Local,
        },
        Scenario {
            name: "event-disabled",
            audio: audio(true, "always", &local(short)),
            steps: &["preferences", ENABLED, "keys:space", BELL],
            expected: Expected::Silence,
        },
        Scenario {
            name: "event-ineligible",
            audio: audio(true, "always", &local(short)),
            steps: &["preferences", CONDITION, "keys:Return End Return", BELL],
            expected: Expected::Silence,
        },
        Scenario {
            name: "source-change",
            audio: audio(true, "always", &local(short)),
            steps: &["preferences", "activate:notification-audio-builtin", BELL],
            expected: Expected::Builtin,
        },
        // The path is typed into General's own entry and accepted with its own
        // button, so the file is read, validated and saved by the real control.
        Scenario {
            name: "source-local",
            audio: audio(true, "always", &json!("builtin")),
            steps: &[
                "preferences",
                PATH,
                "text:{short}",
                "activate:notification-audio-select",
                BELL,
            ],
            expected: Expected::Local,
        },
        // A file the control refuses must leave the accepted sound in place.
        Scenario {
            name: "source-missing",
            audio: audio(true, "always", &json!("builtin")),
            steps: &[
                "preferences",
                PATH,
                "text:{missing}",
                "activate:notification-audio-select",
                BELL,
            ],
            expected: Expected::Builtin,
        },
        Scenario {
            name: "source-truncated",
            audio: audio(true, "always", &json!("builtin")),
            steps: &[
                "preferences",
                PATH,
                "text:{truncated}",
                "activate:notification-audio-select",
                BELL,
            ],
            expected: Expected::Builtin,
        },
        // The bell rings behind Preferences, where its terminal is not viewed.
        // Escape returns focus to that terminal, which ends the eligibility.
        Scenario {
            name: "focus-return",
            audio: audio(true, "always", &local(long)),
            steps: &[
                "preferences",
                CONDITION,
                "keys:Return Home Down Return",
                BELL,
                SOUNDING,
                "keys:Escape",
            ],
            expected: Expected::LocalCutBelow(5),
        },
        Scenario {
            name: "file-removed",
            audio: audio(true, "always", &local(missing)),
            steps: &[BELL],
            expected: Expected::Silence,
        },
        // The header promises more samples than the file holds, so a partly
        // copied sound must be refused rather than played cut short.
        Scenario {
            name: "file-truncated",
            audio: audio(true, "always", &local(truncated)),
            steps: &[BELL],
            expected: Expected::Silence,
        },
        // A decoder that dies after admission must stop the sound, leave the
        // application running and give its single slot back for the next sound.
        Scenario {
            name: "decoder-killed",
            audio: audio(true, "always", &local(long)),
            steps: &[
                "preferences",
                BELL,
                "kill-decoder",
                "activate:notification-audio-builtin",
                "activate:notification-audio-preview",
            ],
            expected: Expected::LocalCutThenBuiltin(8),
        },
        Scenario {
            name: "pane-removed",
            audio: audio(true, "always", &local(long)),
            steps: &["split", BELL, SOUNDING, "close"],
            expected: Expected::LocalCutBelow(5),
        },
        // SIGTERM during a long sound ends the application; a decoder surviving
        // its owner would keep playing past the bound.
        Scenario {
            name: "shutdown",
            audio: audio(true, "always", &local(long)),
            steps: &[BELL, SOUNDING],
            expected: Expected::LocalCutBelow(5),
        },
    ]
}

/// Writes the steps the application runs itself, and returns the ones only this
/// harness can perform. Each of those becomes a pause the application holds
/// until the harness has acted.
fn script(steps: &[&'static str]) -> (String, Vec<&'static str>) {
    let mut script = String::new();
    let mut external = Vec::new();
    for step in steps {
        if step.starts_with("keys:")
            || step.starts_with("text:")
            || *step == "kill-decoder"
            || *step == SOUNDING
        {
            script.push_str(&format!("await:{}\n", external.len()));
            external.push(*step);
        } else {
            script.push_str(step);
            script.push('\n');
        }
    }
    (script, external)
}

fn decoders(application: u32) -> Result<Vec<u32>> {
    let (mut pending, mut found) = (vec![application], Vec::new());
    while let Some(pid) = pending.pop() {
        // A child belongs to the thread that spawned it, and the decoder comes
        // from a supervisor thread, so every task of the process is read.
        let Ok(tasks) = fs::read_dir(format!("/proc/{pid}/task")) else {
            continue;
        };
        for task in tasks {
            let Ok(children) = fs::read_to_string(task?.path().join("children")) else {
                continue;
            };
            for child in children.split_whitespace() {
                let child: u32 = child.parse()?;
                pending.push(child);
                if fs::read(format!("/proc/{child}/cmdline"))
                    .is_ok_and(|line| line.split(|byte| *byte == 0).any(|arg| arg == DECODER_FLAG))
                {
                    found.push(child);
                }
            }
        }
    }
    Ok(found)
}

fn wait_for_sound(harness: &mut sessions::Harness, application: u32) -> Result<()> {
    harness.wait(15, |_| Ok(!decoders(application)?.is_empty()))?;
    // Let the sound reach the sink first, so the capture shows it being cut.
    std::thread::sleep(Duration::from_secs(1));
    Ok(())
}

fn kill_decoder(harness: &mut sessions::Harness, application: u32) -> Result<()> {
    wait_for_sound(harness, application)?;
    for decoder in decoders(application)? {
        kill(Pid::from_raw(decoder as i32), Signal::SIGKILL)?;
    }
    // Retirement needs a moment to be confirmed before the slot is free again.
    std::thread::sleep(Duration::from_secs(2));
    Ok(())
}

/// Fixture files a typed step can name as `{short}`, `{missing}` and so on.
struct Fixtures<'a> {
    short: &'a Path,
    long: &'a Path,
    missing: &'a Path,
    truncated: &'a Path,
}

impl Fixtures<'_> {
    fn fill(&self, text: &str) -> String {
        [
            ("{short}", self.short),
            ("{long}", self.long),
            ("{missing}", self.missing),
            ("{truncated}", self.truncated),
        ]
        .iter()
        .fold(text.to_string(), |text, (name, path)| {
            text.replace(name, &path.to_string_lossy())
        })
    }
}

fn perform(
    harness: &mut sessions::Harness,
    steps: &Path,
    application: u32,
    external: &[&str],
    fixtures: &Fixtures,
) -> Result<()> {
    for (index, step) in external.iter().enumerate() {
        let paused = format!("among: script awaits {index}\n");
        harness.wait(30, |this| Ok(this.text()?.contains(&paused)))?;
        if let Some(keys) = step.strip_prefix("keys:") {
            for key in keys.split(' ') {
                harness.xdo(&["key", "--clearmodifiers", key])?;
                std::thread::sleep(Duration::from_millis(250));
            }
        } else if let Some(text) = step.strip_prefix("text:") {
            harness.xdo(&[
                "type",
                "--clearmodifiers",
                "--delay",
                "5",
                &fixtures.fill(text),
            ])?;
            std::thread::sleep(Duration::from_millis(250));
        } else if *step == SOUNDING {
            wait_for_sound(harness, application)?;
        } else {
            kill_decoder(harness, application)?;
        }
        fs::write(steps.with_extension(index.to_string()), b"")?;
    }
    Ok(())
}

fn run_one(
    binary: &Path,
    scratch: &Path,
    scenario: &Scenario,
    fixtures: &Fixtures,
) -> Result<Value> {
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
    // Two scenarios also configure the command channel, to show that Preview
    // reaches no other channel and that the same configuration does run on a bell.
    let marker = base.join("command-marker");
    let command = matches!(scenario.name, "preview-isolated" | "command-control").then(|| {
        json!({"enabled":true,"eligibility":"always","configuration":{
            "executable":"/bin/sh","arguments":["-c","cat > \"$0\"",marker]}})
    });
    let initial = if scenario.audio.is_some() || command.is_some() {
        let mut notifications = serde_json::Map::new();
        if let Some(audio) = &scenario.audio {
            notifications.insert("audio".into(), audio.clone());
        }
        if let Some(command) = &command {
            notifications.insert("command".into(), command.clone());
        }
        let bytes = serde_json::to_vec(&json!({"version":1,"notifications":notifications}))?;
        fs::write(&general, &bytes)?;
        Some(bytes)
    } else {
        None
    };
    let (steps, external) = script(scenario.steps);
    fs::write(base.join("steps"), steps)?;
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
    let application = harness.start(scenario.name)?;
    perform(
        &mut harness,
        &base.join("steps"),
        application,
        &external,
        fixtures,
    )?;
    harness.wait(30, |this| {
        Ok(this.text()?.contains("among: script finished after"))
    })?;
    if scenario.name == "shutdown" {
        harness.stop(false)?;
        std::thread::sleep(Duration::from_secs(5));
    } else {
        // Longer than a cut sound's bound: stopping the application early would
        // cut the sound itself and hide a cancellation that never happened.
        std::thread::sleep(Duration::from_secs(6));
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
    let saved = written
        .as_ref()
        .map(|bytes| storage::parse_json(bytes))
        .transpose()?
        .map(|document| document["notifications"]["audio"].clone());
    let saved = saved.as_ref().ok_or("General disappeared");
    match scenario.name {
        "preview-off" if written.is_some() => {
            return Err("Preview wrote General preferences".into())
        }
        "event" if saved?["enabled"] != true => {
            return Err("the switch did not enable audio".into())
        }
        "event-disabled" if saved?["enabled"] != false => {
            return Err("the switch did not disable audio".into())
        }
        "event-ineligible" if saved?["eligibility"] != "application-background" => {
            return Err("the condition did not reach Among in background".into())
        }
        "focus-return" if saved?["eligibility"] != "originating-terminal-unfocused" => {
            return Err("the condition did not reach Originating terminal unfocused".into())
        }
        "source-change" if saved?["source"] != "builtin" => {
            return Err("source change was not saved".into())
        }
        "preview-isolated" if marker.exists() || written != initial => {
            return Err("Preview reached the command channel or rewrote General".into())
        }
        "command-control" => {
            let payload = storage::parse_json(&fs::read(&marker).map_err(|_| {
                "a bell with the same command configuration ran no command".to_string()
            })?)?;
            if payload["kind"] != "terminal-bell" {
                return Err("the command received another payload kind".into());
            }
        }
        "source-local" if saved?["source"] != json!({"local":fixtures.short}) => {
            return Err("the typed path was not saved as the local source".into())
        }
        "source-missing" | "source-truncated" if saved?["source"] != "builtin" => {
            return Err("a refused file replaced the accepted sound".into())
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
    let truncated = fixtures.join("truncated.wav");
    let whole = sine_wav(1, 880.0);
    fs::write(&truncated, &whole[..whole.len() / 2])?;
    let mut results = Vec::new();
    let paths = Fixtures {
        short: &short,
        long: &long,
        missing: &missing,
        truncated: &truncated,
    };
    for scenario in scenarios(&short, &long, &missing, &truncated) {
        results.push(run_one(binary, scratch, &scenario, &paths)?);
    }
    fs::write(
        fixtures.join("result.json"),
        serde_json::to_vec_pretty(&json!({"cases":results}))?,
    )?;
    println!("Captured General audio: {}.", fixtures.display());
    Ok(())
}
