//! Matched producer-verbosity control; never uninstrumented package acceptance.
use std::{
    collections::BTreeMap,
    error::Error,
    ffi::OsString,
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::AsRawFd,
        unix::{fs::DirBuilderExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
type Environment = BTreeMap<OsString, OsString>;

struct Process {
    child: Child,
    status: Option<ExitStatus>,
    receipt: PathBuf,
    signals: Vec<i32>,
}
impl Process {
    fn spawn(
        program: &Path,
        args: &[&str],
        env: &Environment,
        stdout: Stdio,
        stderr: &Path,
    ) -> Result<Self> {
        let mut command = Command::new(program);
        command
            .args(args)
            .env_clear()
            .envs(env)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(File::create_new(stderr)?)
            .process_group(0);
        let child = command.spawn()?;
        let process = Self {
            child,
            status: None,
            receipt: stderr.with_extension("exit.json"),
            signals: Vec::new(),
        };
        fs::write(
            stderr.with_extension("process.json"),
            serde_json::to_vec_pretty(
                &serde_json::json!({"pid":process.child.id(),"program":program,"argv":args}),
            )?,
        )?;
        Ok(process)
    }
    fn running(&mut self) -> Result<bool> {
        if self.status.is_none() {
            self.status = self.child.try_wait()?;
        }
        Ok(self.status.is_none())
    }
    fn signal(&mut self, signal: i32) -> Result<()> {
        if !self.running()? {
            return Err("owned process exited before intentional signal".into());
        }
        // Child is still unreaped here; never signal a PID supplied by evidence.
        if unsafe { libc::kill(self.child.id() as i32, signal) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        self.signals.push(signal);
        Ok(())
    }
    fn wait(&mut self, bound: Duration) -> Result<ExitStatus> {
        let start = Instant::now();
        while self.running()? {
            if start.elapsed() >= bound {
                return Err("owned process did not exit within its bound".into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.status
            .ok_or_else(|| "missing actual child status".into())
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if self.status.is_none() {
            match self.child.kill() {
                Ok(()) => self.signals.push(libc::SIGKILL),
                Err(error) => eprintln!("capture cleanup kill: {error}"),
            }
            let start = Instant::now();
            loop {
                match self.child.try_wait() {
                    Ok(Some(status)) => {
                        self.status = Some(status);
                        break;
                    }
                    Ok(None) if start.elapsed() < Duration::from_secs(5) => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Ok(None) => {
                        eprintln!("capture cleanup remains unconfirmed");
                        break;
                    }
                    Err(error) => {
                        eprintln!("capture cleanup wait: {error}");
                        break;
                    }
                }
            }
        }
        let receipt = serde_json::json!({"pid":self.child.id(),"status":self.status.map(|status| status.to_string()),"signals_actually_sent":self.signals,"cleanup_confirmed":self.status.is_some()});
        if let Err(error) = fs::write(&self.receipt, receipt.to_string()) {
            eprintln!("capture exit receipt failed: {error}");
        }
    }
}

fn text(
    program: &str,
    args: &[&str],
    env: &Environment,
    root: &Path,
    name: &str,
) -> Result<String> {
    let output = root.join(name);
    let mut child = Process::spawn(
        Path::new(program),
        args,
        env,
        Stdio::from(File::create_new(&output)?),
        &root.join(format!("{name}.stderr")),
    )?;
    if !child.wait(Duration::from_secs(10))?.success() {
        return Err(format!("private prerequisite failed: {name}").into());
    }
    Ok(fs::read_to_string(output)?)
}
fn utf8(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| "non-UTF8 capture path".into())
}
fn snapshot(pid: u32, root: &Path, name: &str) -> Result<()> {
    fs::write(
        root.join(format!("{name}-cmdline.bin")),
        fs::read(format!("/proc/{pid}/cmdline"))?,
    )?;
    fs::write(
        root.join(format!("{name}-maps.txt")),
        fs::read(format!("/proc/{pid}/maps"))?,
    )?;
    Ok(())
}

fn analyze(bytes: &[u8], signal: bool) -> Result<serde_json::Value> {
    if bytes.len() < 19_200 || bytes.len() > 4 * 1024 * 1024 || !bytes.len().is_multiple_of(2) {
        return Err("capture lacks bounded nonempty S16LE baseline".into());
    }
    let samples: Vec<f64> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| f64::from(i16::from_le_bytes([b[0], b[1]])))
        .collect();
    let nonzero = samples.iter().filter(|&&v| v != 0.0).count();
    let energy = |window: &[f64]| window.iter().map(|v| v * v).sum::<f64>() / window.len() as f64;
    let best = samples
        .chunks(4096)
        .max_by(|a, b| energy(a).total_cmp(&energy(b)))
        .ok_or("empty waveform")?;
    let (mut re, mut im) = (0.0, 0.0);
    for (i, value) in best.iter().enumerate() {
        let angle = i as f64 * 880.0 * std::f64::consts::TAU / 48_000.0;
        re += value * angle.cos();
        im += value * angle.sin();
    }
    let rms = energy(best).sqrt();
    let magnitude = re.hypot(im) / best.len() as f64;
    if (signal && (nonzero < 1000 || rms < 100.0 || magnitude < rms * 0.3))
        || (!signal && nonzero != 0)
    {
        return Err(format!(
            "waveform differs: signal={signal} nonzero={nonzero} rms={rms} magnitude880={magnitude}"
        )
        .into());
    }
    if samples.len() < 120_000 || samples[samples.len() - 96_000..].iter().any(|&v| v != 0.0) {
        return Err("capture lacks the full fresh quiet tail".into());
    }
    Ok(
        serde_json::json!({"bytes":bytes.len(),"nonzero":nonzero,"max_window_rms":rms,"magnitude_880":magnitude,"expected_signal":signal}),
    )
}

struct Progress {
    previous: u64,
    last_progress: Instant,
    worst_stall: Duration,
}

struct QuietRecovery {
    reader: File,
    zero_since: Instant,
    zero_frames: u64,
}

impl QuietRecovery {
    fn new(path: &Path, offset: u64, now: Instant) -> Result<Self> {
        let mut reader = File::open(path)?;
        reader.seek(SeekFrom::Start(offset))?;
        Ok(Self {
            reader,
            zero_since: now,
            zero_frames: 0,
        })
    }

    fn observe(&mut self, bytes: u64, now: Instant) -> Result<bool> {
        let mut buffer = [0_u8; 32768];
        let count = self.reader.read(&mut buffer)?;
        if !count.is_multiple_of(2) {
            return Err("unaligned PCM during fresh recovery quiet observation".into());
        }
        if buffer[..count].iter().any(|&byte| byte != 0) {
            self.zero_since = now;
            self.zero_frames = 0;
        } else {
            self.zero_frames += (count / 2) as u64;
        }
        Ok(
            now.duration_since(self.zero_since) >= Duration::from_millis(300)
                && self.zero_frames >= 14400
                && self.reader.stream_position()? >= bytes,
        )
    }
}
impl Progress {
    fn sample(&mut self, path: &Path, now: Instant) -> Result<u64> {
        let bytes = fs::metadata(path)?.len();
        if !bytes.is_multiple_of(2) || bytes < self.previous || bytes > 4 * 1024 * 1024 {
            return Err("invalid or unbounded recorder progression".into());
        }
        let gap = now.duration_since(self.last_progress);
        if gap > Duration::from_millis(300) {
            return Err("INVALID: recorder PCM progress stalled over 300 ms".into());
        }
        if bytes != self.previous {
            self.worst_stall = self.worst_stall.max(gap);
            self.previous = bytes;
            self.last_progress = now;
        }
        Ok(bytes)
    }
}

fn run(
    route: &str,
    probe: &Path,
    scenario: &str,
    input: &Path,
    payload: &Path,
    root: &Path,
    signal: bool,
) -> Result<()> {
    if !Path::new("/.dockerenv").is_file()
        || ["/dev/snd", "/dev/dsp", "/dev/audio", "/dev/dri"]
            .iter()
            .any(|p| Path::new(p).exists())
    {
        return Err("owned device-free container required".into());
    }
    let mut env = Environment::from([
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("LC_ALL".into(), "C.UTF-8".into()),
        ("GDK_BACKEND".into(), "x11".into()),
        ("GSK_RENDERER".into(), "cairo".into()),
        ("GTK_A11Y".into(), "none".into()),
        ("PULSE_SINK".into(), "among_audit".into()),
        ("PULSE_SOURCE".into(), "among_audit.monitor".into()),
        ("GST_DEBUG".into(), "pulse*:4".into()),
        ("GST_DEBUG_NO_COLOR".into(), "1".into()),
    ]);
    for (key, directory) in [
        ("HOME", "home"),
        ("XDG_RUNTIME_DIR", "runtime"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
        ("TMPDIR", "tmp"),
    ] {
        let path = root.join(directory);
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        env.insert(key.into(), path.into_os_string());
    }
    let socket = format!("unix:{}/runtime/pulse.sock", root.display());
    env.insert("PULSE_SERVER".into(), socket.clone().into());
    fs::write(
        root.join("pulse.pa"),
        format!(
            "load-module module-native-protocol-unix socket={}/runtime/pulse.sock auth-anonymous=1\nload-module module-null-sink sink_name=among_audit rate=48000 channels=1 channel_map=mono format=s16le\nset-default-sink among_audit\nset-default-source among_audit.monitor\n",
            root.display()
        ),
    )?;
    let conf = root.join("pulse.pa");
    let mut pulse_args = vec![
        "-n",
        "--daemonize=no",
        "--use-pid-file=no",
        "--exit-idle-time=-1",
        "--log-level=debug",
        "--log-time=yes",
        "--file",
        utf8(&conf)?,
    ];
    match route {
        "default" => (),
        "copy" => pulse_args.extend(["--disable-shm=yes", "--enable-memfd=no"]),
        _ => return Err("route must be separately labeled default or copy".into()),
    }
    fs::write(
        root.join("invocation.json"),
        serde_json::to_vec_pretty(
            &serde_json::json!({"route":route,"probe":probe,"scenario":scenario,"input":input,"payload":payload,"pulse_args":pulse_args,"environment":format!("{env:#?}")}),
        )?,
    )?;
    let mut server = Process::spawn(
        Path::new("/usr/bin/pulseaudio"),
        &pulse_args,
        &env,
        Stdio::from(File::create_new(root.join("server.stdout"))?),
        &root.join("server.stderr"),
    )?;
    let start = Instant::now();
    while !root.join("runtime/pulse.sock").exists() {
        if start.elapsed() >= Duration::from_secs(10) || !server.running()? {
            return Err("private server failed readiness".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let info = text("/usr/bin/pactl", &["info"], &env, root, "server-info.txt")?;
    let sinks = text(
        "/usr/bin/pactl",
        &["list", "short", "sinks"],
        &env,
        root,
        "sinks.txt",
    )?;
    let modules = text(
        "/usr/bin/pactl",
        &["list", "short", "modules"],
        &env,
        root,
        "modules.txt",
    )?;
    if !info
        .lines()
        .any(|line| line == format!("Server String: {socket}"))
        || !info.lines().any(|line| line == "Default Sink: among_audit")
        || sinks.lines().count() != 1
        || !sinks.contains("among_audit\tmodule-null-sink.c")
        || modules.lines().count() != 2
        || !modules.contains("module-native-protocol-unix")
        || !modules.contains("module-null-sink")
    {
        return Err("sole private null-sink identity was not confirmed".into());
    }
    let mut display = Process::spawn(
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
        Stdio::piped(),
        &root.join("xvfb.stderr"),
    )?;
    let mut pipe = display
        .child
        .stdout
        .take()
        .ok_or("missing private display pipe")?;
    let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut number = Vec::new();
    let start = Instant::now();
    while !number.contains(&b'\n') {
        let mut bytes = [0; 32];
        match pipe.read(&mut bytes) {
            Ok(n) if n > 0 => number.extend_from_slice(&bytes[..n]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => (),
            Ok(_) => return Err("display pipe closed before readiness".into()),
            Err(error) => return Err(error.into()),
        }
        if number.len() > 32 || start.elapsed() >= Duration::from_secs(30) || !display.running()? {
            return Err("private display failed readiness".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let number: u16 = std::str::from_utf8(&number)?.trim().parse()?;
    env.insert("DISPLAY".into(), format!(":{number}").into());
    let pcm = root.join("capture.s16le");
    let mut recorder = Process::spawn(
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
        Stdio::from(File::create_new(&pcm)?),
        &root.join("recorder.stderr"),
    )?;
    let mut monitor = File::create_new(root.join("liveness.jsonl"))?;
    let readiness = Instant::now();
    loop {
        if !server.running()? || !recorder.running()? || !display.running()? {
            return Err("INVALID: server/recorder/display died before PCM readiness".into());
        }
        let bytes = fs::metadata(&pcm)?.len();
        writeln!(
            monitor,
            "{}",
            serde_json::json!({"phase":"readiness","ms":readiness.elapsed().as_millis(),"pcm_bytes":bytes,"server":server.child.id(),"recorder":recorder.child.id()})
        )?;
        if bytes >= 19_200 {
            break;
        }
        if readiness.elapsed() >= Duration::from_secs(10) {
            return Err("INVALID: no actual recorder PCM readiness".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut progress = Progress {
        previous: fs::metadata(&pcm)?.len(),
        last_progress: Instant::now(),
        worst_stall: Duration::ZERO,
    };
    let settle = Instant::now();
    while settle.elapsed() < Duration::from_millis(300) {
        if !server.running()? || !recorder.running()? || !display.running()? {
            return Err("INVALID: process death during recorder settling".into());
        }
        let bytes = progress.sample(&pcm, Instant::now())?;
        writeln!(
            monitor,
            "{}",
            serde_json::json!({"phase":"settle","ms":settle.elapsed().as_millis(),"pcm_bytes":bytes})
        )?;
        std::thread::sleep(Duration::from_millis(20));
    }
    snapshot(server.child.id(), root, "server")?;
    snapshot(recorder.child.id(), root, "recorder")?;
    let anchor_bytes = fs::metadata(&pcm)?.len();
    let anchor = Instant::now();
    let consumer_output = root.join("consumer");
    let workload_scenario = if matches!(
        scenario,
        "validator-kill-server" | "validator-kill-recorder"
    ) {
        "construct-origin"
    } else {
        scenario
    };
    fs::write(root.join("actual-workload-scenario.txt"), workload_scenario)?;
    let mut consumer = Process::spawn(
        Path::new("/usr/bin/dbus-run-session"),
        &[
            "--",
            utf8(probe)?,
            workload_scenario,
            utf8(&consumer_output)?,
            utf8(input)?,
            utf8(payload)?,
        ],
        &env,
        Stdio::from(File::create_new(root.join("consumer.stdout"))?),
        &root.join("consumer.stderr"),
    )?;
    let mut tail: Option<(Instant, u64)> = None;
    let mut final_check: Option<Process> = None;
    let mut observed_signal = false;
    let mut fault_injected = false;
    let mut pcm_reader = File::open(&pcm)?;
    pcm_reader.seek(SeekFrom::Start(anchor_bytes))?;
    let mut active_nonzero = 0_u64;
    let mut quiet_recovery: Option<QuietRecovery> = None;
    let mut recovery_offset = None;
    let final_bytes;
    let final_clock;
    loop {
        let now = Instant::now();
        if !fault_injected
            && scenario.starts_with("validator-kill-")
            && anchor.elapsed() >= Duration::from_millis(700)
        {
            if scenario == "validator-kill-server" {
                server.signal(libc::SIGKILL)?;
            } else if scenario == "validator-kill-recorder" {
                recorder.signal(libc::SIGKILL)?;
            } else {
                return Err("unknown fault control".into());
            }
            fault_injected = true;
        }
        let server_alive = server.running()?;
        let recorder_alive = recorder.running()?;
        let display_alive = display.running()?;
        if !server_alive || !recorder_alive || !display_alive {
            writeln!(
                monitor,
                "{}",
                serde_json::json!({"phase":"INVALID-process-death","ms":anchor.elapsed().as_millis(),"server_status":server.status.map(|status|status.to_string()),"recorder_status":recorder.status.map(|status|status.to_string()),"display_status":display.status.map(|status|status.to_string()),"fault_injected":fault_injected})
            )?;
            return Err("INVALID: actual server/recorder/display death during observation".into());
        }
        let bytes = progress.sample(&pcm, now)?;
        let expected = (anchor.elapsed().as_nanos() * 48_000 / 1_000_000_000) as u64;
        let frames = (bytes - anchor_bytes) / 2;
        if expected.saturating_sub(frames) > 9600 {
            return Err("INVALID: whole-window clock/sample shortfall exceeds 9600 frames".into());
        }
        if consumer_output.join("fresh-recovery-await-quiet").is_file() && recovery_offset.is_none()
        {
            if quiet_recovery.is_none() {
                quiet_recovery = Some(QuietRecovery::new(&pcm, bytes, now)?);
            }
            if let Some(quiet) = &mut quiet_recovery {
                if quiet.observe(bytes, now)? {
                    recovery_offset = Some(bytes);
                    fs::write(
                        consumer_output.join("fresh-recovery-quiet-observed"),
                        format!(
                            "offset_bytes={bytes}\nquiet_frames={}\nquiet_ms={}\n",
                            quiet.zero_frames,
                            now.duration_since(quiet.zero_since).as_millis()
                        ),
                    )?;
                }
            }
        }
        if consumer_output.join("active-await-observed-pcm").exists() && !observed_signal {
            let mut chunk = [0_u8; 8192];
            let count = pcm_reader.read(&mut chunk)?;
            if !count.is_multiple_of(2) {
                return Err("unaligned PCM read during active signal observation".into());
            }
            active_nonzero += chunk[..count]
                .as_chunks::<2>()
                .0
                .iter()
                .filter(|&&b| b != [0, 0])
                .count() as u64;
            if active_nonzero >= 9600 {
                fs::write(
                    consumer_output.join("active-pcm-observed"),
                    format!("anchor_frames={frames} nonzero_frames={active_nonzero}\n"),
                )?;
                observed_signal = true;
            }
        }
        let running = consumer.running()?;
        writeln!(
            monitor,
            "{}",
            serde_json::json!({"phase":if tail.is_some(){"tail"}else{"workload"},"ms":anchor.elapsed().as_millis(),"pcm_bytes":bytes,"new_frames":frames,"expected_frames":expected,"consumer_running":running,"server_alive":true,"recorder_alive":true,"active_signal_observed":observed_signal})
        )?;
        if !running && tail.is_none() {
            let status = consumer.status.ok_or("missing workload status")?;
            if !status.success() {
                return Err(format!("consumer failed; capture is INVALID: {status}").into());
            }
            tail = Some((now, bytes));
        }
        if running && anchor.elapsed() >= Duration::from_secs(25) {
            return Err("consumer exceeded unchanged 25-second observation bound".into());
        }
        if let Some((started, baseline)) = tail {
            if now.duration_since(started) >= Duration::from_millis(2500)
                && bytes - baseline >= 240_000
            {
                if final_check.is_none() {
                    final_check = Some(Process::spawn(
                        Path::new("/usr/bin/pactl"),
                        &["info"],
                        &env,
                        Stdio::from(File::create_new(root.join("final-server-info.txt"))?),
                        &root.join("final-server-info.stderr"),
                    )?);
                }
                if let Some(check) = &mut final_check {
                    if !check.running()? {
                        if !check.status.ok_or("missing final server check")?.success() {
                            return Err("INVALID: final server identity request failed".into());
                        }
                        final_bytes = bytes;
                        final_clock = now;
                        break;
                    }
                }
            }
            if now.duration_since(started) > Duration::from_secs(10) {
                return Err("INVALID: fresh tail progress never completed".into());
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    monitor.sync_all()?;
    let (tail_start, tail_baseline) = tail.ok_or("missing actual post-workload baseline")?;
    let elapsed = final_clock.duration_since(anchor);
    let tail_elapsed = final_clock.duration_since(tail_start);
    recorder.signal(libc::SIGINT)?;
    let recorder_status = recorder.wait(Duration::from_secs(5))?;
    server.signal(libc::SIGTERM)?;
    let server_status = server.wait(Duration::from_secs(5))?;
    if !recorder_status.success() || !server_status.success() {
        return Err("INVALID: server/recorder failed after recorded intentional shutdown".into());
    }
    let all_bytes = fs::read(&pcm)?;
    let anchored = &all_bytes[usize::try_from(anchor_bytes)?..usize::try_from(final_bytes)?];
    let analysis = analyze(anchored, signal)?;
    let fresh_recovery_analysis = match recovery_offset {
        Some(offset) => Some(analyze(
            &all_bytes[usize::try_from(offset)?..usize::try_from(final_bytes)?],
            true,
        )?),
        None if scenario.ends_with("-recovery") => {
            return Err("fresh recovery lacks its separate observed PCM window".into());
        }
        None => None,
    };
    fs::write(
        root.join("result.json"),
        serde_json::to_vec_pretty(
            &serde_json::json!({"valid":true,"route":route,"scenario":scenario,"recorder_ready_bytes":anchor_bytes,"settle_ms":300,"anchor_elapsed_ms":elapsed.as_millis(),"whole_window_frames":(final_bytes-anchor_bytes)/2,"tail_elapsed_ms":tail_elapsed.as_millis(),"fresh_tail_frames":(final_bytes-tail_baseline)/2,"buffer_tolerance_frames":9600,"stall_limit_ms":300,"worst_observed_stall_ms":progress.worst_stall.as_millis(),"active_nonzero_frames_before_revoke":active_nonzero,"recorder_status":recorder_status.to_string(),"server_status":server_status.to_string(),"analysis":analysis,"fresh_recovery_offset_bytes":recovery_offset,"fresh_recovery_analysis":fresh_recovery_analysis}),
        )?,
    )?;
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 8 {
        return Err("route probe scenario input payload NEW-output signal|silent required".into());
    }
    let root = PathBuf::from(&args[6]);
    fs::DirBuilder::new().mode(0o700).create(&root)?;
    let root = root.canonicalize()?;
    let route = args[1].to_str().ok_or("non-UTF8 route")?;
    let scenario = args[3].to_str().ok_or("non-UTF8 scenario")?;
    let signal = match args[7].to_str() {
        Some("signal") => true,
        Some("silent") => false,
        _ => return Err("signal or silent expectation required".into()),
    };
    let result = run(
        route,
        Path::new(&args[2]),
        scenario,
        Path::new(&args[4]),
        Path::new(&args[5]),
        &root,
        signal,
    );
    if let Err(error) = &result {
        fs::write(
            root.join("INVALID.txt"),
            format!("{error}\nNo PCM padding or retry is performed.\n"),
        )?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advancing_pcm_cannot_hide_an_excessive_unobserved_gap() -> Result<()> {
        let path =
            std::env::temp_dir().join(format!("among-pcm-gap-control-{}", std::process::id()));
        let mut file = File::create_new(&path)?;
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_file(&self.0);
            }
        }
        let _cleanup = Cleanup(path.clone());
        file.write_all(&[0, 0])?;
        let start = Instant::now();
        let mut progress = Progress {
            previous: 0,
            last_progress: start,
            worst_stall: Duration::ZERO,
        };
        assert!(progress
            .sample(&path, start + Duration::from_millis(301))
            .is_err());
        assert_eq!(progress.previous, 0);
        assert_eq!(
            progress.sample(&path, start + Duration::from_millis(300))?,
            2
        );
        assert_eq!(progress.worst_stall, Duration::from_millis(300));
        Ok(())
    }

    #[test]
    fn silent_recovery_cannot_borrow_signal_from_an_old_active_tone() {
        let mut bytes = Vec::new();
        for frame in 0..36000 {
            let angle = f64::from(frame) * 880.0 * std::f64::consts::TAU / 48000.0;
            bytes.extend(((angle.sin() * 8192.0) as i16).to_le_bytes());
        }
        let recovery = bytes.len();
        bytes.resize(recovery + 260000, 0);
        assert!(analyze(&bytes, true).is_ok());
        assert!(analyze(&bytes[recovery..], true).is_err());
    }
}
