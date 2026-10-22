//! private null-sink output evidence, never a request/prepared-state assertion.

use crate::{process::OwnedProcess, Result};
use nix::{
    fcntl::{fcntl, FcntlArg, OFlag},
    sys::signal::Signal,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File},
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub fn require_container() -> Result<()> {
    if !Path::new("/.dockerenv").is_file()
        || ["/dev/snd", "/dev/dsp", "/dev/audio"]
            .iter()
            .any(|name| Path::new(name).exists())
    {
        return Err("audio capture requires an owned device-free Docker container".into());
    }
    Ok(())
}

pub fn identity(info: &str, sinks: &str, modules: &str, socket: &str) -> Result<()> {
    let sink: Vec<_> = sinks.lines().collect();
    let modules: Vec<_> = modules
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .collect();
    if !info
        .lines()
        .any(|line| line == format!("Server String: {socket}"))
        || !info.lines().any(|line| line == "Default Sink: among_audit")
        || sink.len() != 1
        || sink[0].split_whitespace().nth(1) != Some("among_audit")
        || sink[0].split_whitespace().nth(2) != Some("module-null-sink.c")
        || modules.len() != 2
        || modules
            .iter()
            .filter(|line| line.get(1) == Some(&"module-null-sink"))
            .count()
            != 1
        || modules
            .iter()
            .filter(|line| line.get(1) == Some(&"module-native-protocol-unix"))
            .count()
            != 1
    {
        return Err("sole private null-sink identity was not confirmed".into());
    }
    Ok(())
}

pub fn analyze(bytes: &[u8], signal: bool) -> Result<Value> {
    if bytes.len() < 19_200 || bytes.len() > 4 * 1024 * 1024 || !bytes.len().is_multiple_of(2) {
        return Err("capture lacks a bounded nonempty S16LE baseline".into());
    }
    let samples: Vec<_> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| f64::from(i16::from_le_bytes([b[0], b[1]])))
        .collect();
    let nonzero = samples.iter().filter(|&&v| v != 0.0).count();
    let energy = |window: &[f64]| window.iter().map(|s| s * s).sum::<f64>() / window.len() as f64;
    let best = samples
        .chunks(4096)
        .max_by(|a, b| energy(a).total_cmp(&energy(b)))
        .ok_or("empty capture")?;
    let (mut re, mut im) = (0.0, 0.0);
    for (i, sample) in best.iter().enumerate() {
        let angle = i as f64 * 880.0 * std::f64::consts::TAU / 48_000.0;
        re += sample * angle.cos();
        im += sample * angle.sin();
    }
    let rms = energy(best).sqrt();
    let magnitude = re.hypot(im) / best.len() as f64;
    let result = json!({"bytes":bytes.len(),"nonzero":nonzero,"max_window_rms":rms,"magnitude_880":magnitude,"expected_signal":signal});
    if (signal && (nonzero < 1000 || rms < 100.0 || magnitude < rms * 0.3))
        || (!signal && nonzero != 0)
    {
        return Err(format!("captured waveform differs from expectation: {result}").into());
    }
    Ok(result)
}

/// clear-stall recovery must produce a second tone, not merely retain the
/// initial pre-stall playback in a successful whole-capture energy measurement.
pub fn recovered_output(bytes: &[u8]) -> Result<Value> {
    analyze(bytes, true)?;
    let mut islands = Vec::new();
    let mut current: Option<(usize, usize)> = None;
    for (frame, sample) in bytes.as_chunks::<2>().0.iter().enumerate() {
        if i32::from(i16::from_le_bytes(*sample)).abs() < 128 {
            continue;
        }
        match current {
            Some((start, end)) if frame - end < 4_800 => current = Some((start, frame)),
            Some(previous) => {
                islands.push(previous);
                current = Some((frame, frame));
            }
            None => current = Some((frame, frame)),
        }
    }
    if let Some(last) = current {
        islands.push(last);
    }
    if islands.len() != 2 {
        return Err("clear-stall capture lacks two distinct initial/recovery tones".into());
    }
    let mut observed = Vec::new();
    for (start, end) in islands {
        observed.push(json!({"start_frame":start,"end_frame":end,"analysis":analyze(&bytes[start*2..(end+1)*2],true)?}));
    }
    Ok(json!({"initial":observed[0],"recovery":observed[1]}))
}

fn text(
    program: &str,
    args: &[&str],
    env: &BTreeMap<OsString, OsString>,
    base: &Path,
    name: &str,
) -> Result<String> {
    let path = base.join(name);
    let log = File::create(&path)?;
    let mut child = OwnedProcess::spawn(Path::new(program), args, env, base, &log)?;
    if !child.wait(Duration::from_secs(10))?.success() {
        return Err(format!("audio prerequisite failed: {name}").into());
    }
    Ok(fs::read_to_string(path)?)
}

pub struct Invocation<'a> {
    pub probe: &'a Path,
    pub scenario: &'a str,
    pub input: &'a Path,
    pub payload: &'a Path,
    pub installed: bool,
    pub expected_failure: Option<&'a str>,
    pub signal: bool,
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn run(request: &Invocation<'_>, output: &Path) -> Result<Value> {
    require_container()?;
    fs::DirBuilder::new().mode(0o700).create(output)?;
    let base = output.canonicalize()?;
    let mut env = BTreeMap::from([
        (
            OsString::from("PATH"),
            OsString::from("/usr/bin:/bin:/snap/bin"),
        ),
        ("LC_ALL".into(), "C.UTF-8".into()),
        ("GDK_BACKEND".into(), "x11".into()),
        ("GSK_RENDERER".into(), "cairo".into()),
        ("GTK_A11Y".into(), "none".into()),
        ("GST_DEBUG".into(), "2,GST_PLUGIN_LOADING:6".into()),
        ("PULSE_SINK".into(), "among_audit".into()),
        ("PULSE_SOURCE".into(), "among_audit.monitor".into()),
    ]);
    for (key, directory) in [
        ("HOME", "home"),
        ("XDG_RUNTIME_DIR", "runtime"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
        ("TMPDIR", "tmp"),
    ] {
        let path = base.join(directory);
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        env.insert(key.into(), path.into_os_string());
    }
    let socket = format!("unix:{}/runtime/pulse.sock", base.display());
    env.insert("PULSE_SERVER".into(), socket.clone().into());
    let conf = base.join("pulse.conf");
    fs::write(&conf, format!("load-module module-native-protocol-unix socket={}/runtime/pulse.sock auth-anonymous=1\nload-module module-null-sink sink_name=among_audit rate=48000 channels=1 channel_map=mono\nset-default-sink among_audit\nset-default-source among_audit.monitor\n",base.display()))?;
    let pulse_log = File::create(base.join("pulse.log"))?;
    let mut pulse = OwnedProcess::spawn(
        Path::new("/usr/bin/pulseaudio"),
        &[
            "-n",
            "--daemonize=no",
            "--use-pid-file=no",
            "--exit-idle-time=-1",
            "--log-level=info",
            "--file",
            utf8(&conf)?,
        ],
        &env,
        &base,
        &pulse_log,
    )?;
    let start = Instant::now();
    while !base.join("runtime/pulse.sock").exists() {
        if start.elapsed() > Duration::from_secs(10) || !pulse.is_running()? {
            return Err("owned audio server failed readiness".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let info = text("/usr/bin/pactl", &["info"], &env, &base, "server-info.txt")?;
    let sinks = text(
        "/usr/bin/pactl",
        &["list", "short", "sinks"],
        &env,
        &base,
        "sinks.txt",
    )?;
    let modules = text(
        "/usr/bin/pactl",
        &["list", "short", "modules"],
        &env,
        &base,
        "modules.txt",
    )?;
    identity(&info, &sinks, &modules, &socket)?;
    let xlog = File::create(base.join("xvfb.log"))?;
    let mut x = OwnedProcess::spawn_capture(
        Path::new("/usr/bin/Xvfb"),
        &[
            "-displayfd",
            "1",
            "-screen",
            "0",
            "800x600x24",
            "-nolisten",
            "tcp",
            "-ac",
        ],
        &env,
        &base,
        &xlog,
    )?;
    let mut pipe = x.take_stdout()?;
    let flags = OFlag::from_bits_truncate(fcntl(&pipe, FcntlArg::F_GETFL)?);
    fcntl(&pipe, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
    env.insert(
        "DISPLAY".into(),
        crate::display::read_display(&mut pipe, Duration::from_secs(30))?.into(),
    );
    fs::write(base.join("environment.txt"), format!("{env:#?}\n"))?;
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
        &env,
        &base,
        &File::create(&pcm)?,
        &File::create(base.join("recorder.log"))?,
    )?;
    std::thread::sleep(Duration::from_millis(500));
    let consumer = base.join("consumer");
    let probe_args = [
        utf8(request.probe)?,
        request.scenario,
        utf8(&consumer)?,
        utf8(request.input)?,
        utf8(request.payload)?,
    ];
    let argv = if request.installed {
        // the installed classic launcher supplies its real namespace/environment;
        // this remains an external library probe, not production General controls.
        vec![
            "--".to_string(),
            "/usr/bin/snap".into(),
            "run".into(),
            "--shell".into(),
            "among".into(),
            "-c".into(),
            format!(
                "exec {}",
                probe_args
                    .iter()
                    .map(|arg| quote(arg))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        ]
    } else {
        std::iter::once("--".to_string())
            .chain(probe_args.iter().map(|arg| arg.to_string()))
            .collect()
    };
    let mut child = OwnedProcess::spawn_split(
        Path::new("/usr/bin/dbus-run-session"),
        &argv.iter().map(String::as_str).collect::<Vec<_>>(),
        &env,
        &base,
        &File::create(base.join("consumer.log"))?,
        &File::create(base.join("errors.log"))?,
    )?;
    let outcome = child.wait(Duration::from_secs(25));
    std::thread::sleep(Duration::from_millis(2000));
    recorder.signal(Signal::SIGINT)?;
    recorder.wait(Duration::from_secs(5))?;
    let bytes = fs::read(&pcm)?;
    let analysis = analyze(&bytes, request.signal);
    fs::write(
        base.join("analysis.json"),
        serde_json::to_vec_pretty(
            &json!({"signal":analysis.as_ref().ok(),"analysis_error":analysis.as_ref().err().map(ToString::to_string),"consumer_exit":outcome.as_ref().ok().map(ToString::to_string)}),
        )?,
    )?;
    let status = outcome?;
    let analysis = analysis?;
    if let Some(expected) = request.expected_failure {
        let diagnostics = format!(
            "{}{}",
            fs::read_to_string(base.join("errors.log"))?,
            fs::read_to_string(base.join("consumer/decoder-errors.txt")).unwrap_or_default()
        );
        if status.success() || !diagnostics.contains(expected) {
            return Err("negative probe did not produce its expected diagnostic failure".into());
        }
    } else if !status.success() {
        return Err("audio consumer failed; capture is not success evidence".into());
    }
    pulse.cleanup()?;
    x.cleanup()?;
    Ok(analysis)
}

pub(crate) fn utf8(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| "audio evidence path is not UTF-8".into())
}

pub fn fixtures(root: &Path) -> Result<Vec<PathBuf>> {
    fs::create_dir(root)?;
    let mut paths = Vec::new();
    for (tag, bits) in [(1u16, 8u16), (1, 16), (1, 24), (1, 32), (3, 32), (3, 64)] {
        let mut data = Vec::new();
        for i in 0..36_000u32 {
            let phase = (i * 880) % 48_000;
            let triangle = if phase < 24_000 {
                i64::from(phase) * 4 - 48_000
            } else {
                144_000 - i64::from(phase) * 4
            };
            let value = triangle * 8000 / 48_000;
            match (tag, bits) {
                (1, 8) => data.push((value / 256 + 128) as u8),
                (1, 16) => data.extend((value as i16).to_le_bytes()),
                (1, 24) => data.extend(&(value as i32 * 256).to_le_bytes()[..3]),
                (1, 32) => data.extend((value as i32 * 65536).to_le_bytes()),
                (3, 32) => data.extend((value as f32 / 32768.0).to_le_bytes()),
                (3, 64) => data.extend((value as f64 / 32768.0).to_le_bytes()),
                _ => return Err("unsupported synthetic fixture".into()),
            }
        }
        let mut wav = b"RIFF".to_vec();
        wav.extend((36 + data.len() as u32).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(tag.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(48_000u32.to_le_bytes());
        wav.extend((48_000 * u32::from(bits / 8)).to_le_bytes());
        wav.extend((bits / 8).to_le_bytes());
        wav.extend(bits.to_le_bytes());
        wav.extend(b"data");
        wav.extend((data.len() as u32).to_le_bytes());
        wav.extend(data);
        let path = root.join(format!("{tag}-{bits}.wav"));
        fs::write(&path, wav)?;
        paths.push(path);
    }
    Ok(paths)
}
