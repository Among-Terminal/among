use crate::{
    action_review::{digest, new_output, write_report},
    process::OwnedProcess,
    sessions::{fresh, Harness},
    storage, Result,
};
use nix::sys::signal::Signal;
use serde_json::{json, Value};
use std::{
    fs::{self, File},
    path::Path,
    time::{Duration, Instant},
};

pub fn extents(text: &str) -> Result<[u64; 4]> {
    let values = text
        .split_once('=')
        .ok_or("Preferences frame extents absent")?
        .1
        .split(',')
        .map(|part| part.trim().parse::<u64>())
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let values: [u64; 4] = values
        .try_into()
        .map_err(|_| "Preferences frame extents incomplete")?;
    if values.iter().any(|v| *v > 1000) {
        return Err("Preferences frame extents exceed capture bounds".into());
    }
    Ok(values)
}

fn preferences(h: &Harness, pid: u32) -> Result<(String, [u64; 4])> {
    let windows = h.probe(
        "/usr/bin/xdotool",
        &[
            "search",
            "--onlyvisible",
            "--pid",
            &pid.to_string(),
            "--name",
            "^Preferences$",
        ],
    )?;
    let window = windows
        .split_whitespace()
        .last()
        .ok_or("live Preferences window absent")?
        .to_string();
    let frame = extents(&h.probe("/usr/bin/xprop", &["-id", &window, "_GTK_FRAME_EXTENTS"])?)?;
    Ok((window, frame))
}

fn key(h: &Harness, binding: &str) -> Result<()> {
    h.key(binding)?;
    std::thread::sleep(Duration::from_millis(400));
    Ok(())
}
fn command(h: &Harness, text: &str) -> Result<()> {
    h.xdo(&["type", "--clearmodifiers", "--delay", "35", text])?;
    key(h, "Return")?;
    std::thread::sleep(Duration::from_millis(500));
    Ok(())
}

pub fn capture(
    root: &Path,
    binary: &Path,
    plugins: &Path,
    mode: &str,
    output: &Path,
) -> Result<Value> {
    crate::preferences::owned_display()?;
    let enabled = match mode {
        "on" => true,
        "off" => false,
        _ => return Err("plugin capture mode must be on or off".into()),
    };
    let root = root.canonicalize()?;
    let docs = fs::read_to_string(storage::safe_path(&root, Path::new("docs/plugins.md"))?)?;
    let hook = docs
        .split_once("among_osc7() {")
        .and_then(|(_, tail)| tail.split_once("```"))
        .ok_or("documented OSC7 hook absent")?
        .0;
    let names = ["clock", "system-status", "pane-pwd"];
    let mut originals = Vec::new();
    for name in names {
        let path = storage::safe_path(plugins, Path::new(name))?;
        let bytes = fs::read(&path)?;
        if !bytes.starts_with(b"\x7fELF") {
            return Err("capture plugin must be a native ELF".into());
        }
        originals.push((path, digest_bytes(&bytes)?, bytes));
    }
    let output = new_output(&root, output)?;
    let mut h = fresh(binary, &output, "plugins-capture")?;
    h.env.insert("AMONG_SELFCHECK_TICKS".into(), "2000".into());
    // A new bus binds dconf to this capture's private XDG directories instead
    // of the parent GNOME session. All post-appearance demo clips are dark.
    h.probe(
        "/usr/bin/dbus-run-session",
        &[
            "--",
            "/usr/bin/gsettings",
            "set",
            "org.gnome.desktop.interface",
            "color-scheme",
            "prefer-dark",
        ],
    )?;
    let theme = h.probe(
        "/usr/bin/dbus-run-session",
        &[
            "--",
            "/usr/bin/gsettings",
            "get",
            "org.gnome.desktop.interface",
            "color-scheme",
        ],
    )?;
    if theme.trim() != "'prefer-dark'" {
        return Err("private capture dark preference was not observed".into());
    }
    let plugin_dir = h.base.join("data/among/plugins");
    fs::create_dir(&plugin_dir)?;
    for (name, (_, _, bytes)) in names.iter().zip(&originals) {
        storage::atomic_write(&plugin_dir, Path::new(name), bytes, 0o755)?;
    }
    fs::write(
        h.base.join("config/among/plugins.json"),
        serde_json::to_vec(
            &json!({"plugins":names.map(|name|json!({"name":name,"enabled":!enabled}))}),
        )?,
    )?;
    for dir in ["projects/atlas/src", "projects/atlas/tests"] {
        fs::create_dir_all(h.base.join(dir))?;
    }
    fs::write(
        h.base.join("projects/atlas/src/main.rs"),
        b"// synthetic demonstration fixture\n",
    )?;
    fs::write(
        h.base.join("projects/atlas/tests/smoke.rs"),
        b"// synthetic demonstration fixture\n",
    )?;
    storage::atomic_write(&h.base,Path::new("shell"),format!("#!/bin/bash\namong_osc7() {{{hook}\nll() {{ ls -alF --color=auto; }}\nexport -f ll among_osc7\nexport PROMPT_COMMAND\nexport PS1='demo@among:\\w\\$ '\nexec /bin/bash --noprofile --norc\n").as_bytes(),0o755)?;
    let clip = if enabled { "plugins-on" } else { "plugins-off" };
    let pid = h.start(&format!("{clip}-app"))?;
    h.wait(20, |this| {
        let text = this.text()?;
        Ok(text.contains("has_focus=true")
            && names.iter().all(|name| {
                text.contains(&format!(
                    "{name}={}",
                    if enabled { "Disabled" } else { "Running" }
                ))
            }))
    })?;
    let dimensions = h.probe("/usr/bin/xdotool", &["getdisplaygeometry"])?;
    if dimensions.trim() != "1600 1200" {
        return Err("plugin recording requires the owned 1600x1200 video display profile".into());
    }
    command(&h, "cd \"$HOME\"; clear; ll")?;
    key(&h, "ctrl+shift+n")?;
    h.wait(20, |this| Ok(this.text()?.contains("visible=2 panes=2")))?;
    command(&h, "cd \"$HOME/../projects/atlas/src\"; ll")?;
    key(&h, "ctrl+shift+n")?;
    h.wait(20, |this| Ok(this.text()?.contains("visible=3 panes=3")))?;
    command(&h, "cd \"$HOME/../projects/atlas/tests\"; ll")?;
    key(&h, "ctrl+comma")?;
    let (window, frame) = preferences(&h, pid)?;
    h.xdo(&[
        "mousemove",
        "--window",
        &window,
        &(380 + frame[0]).to_string(),
        &(23 + frame[2]).to_string(),
        "click",
        "1",
    ])?;
    std::thread::sleep(Duration::from_secs(2));
    let log = File::create(h.base.join(format!("{clip}-capture.log")))?;
    let display = h
        .env
        .get(std::ffi::OsStr::new("DISPLAY"))
        .and_then(|v| v.to_str())
        .ok_or("owned capture DISPLAY absent")?;
    let movie = h.base.join(format!("{clip}.mp4"));
    let args = [
        "-v",
        "warning",
        "-nostdin",
        "-f",
        "x11grab",
        "-draw_mouse",
        "1",
        "-framerate",
        "25",
        "-video_size",
        "1600x1200",
        "-i",
        display,
        "-an",
        "-map_metadata",
        "-1",
        "-c:v",
        "libx264",
        "-threads",
        "1",
        "-preset",
        "veryfast",
        "-pix_fmt",
        "yuv420p",
        "-movflags",
        "+faststart",
        movie.to_str().ok_or("capture path is not UTF-8")?,
    ];
    let mut recorder =
        OwnedProcess::spawn(Path::new("/usr/bin/ffmpeg"), &args, &h.env, &h.base, &log)?;
    let started = Instant::now();
    std::thread::sleep(Duration::from_millis(1100));
    if !recorder.is_running()? {
        return Err("capture recorder exited before observed plugin actions".into());
    }
    let mut changed = Vec::new();
    let mut events = Vec::new();
    for (y, name) in [(163, "clock"), (218, "system-status"), (273, "pane-pwd")] {
        let (window, frame) = preferences(&h, pid)?;
        h.xdo(&["windowactivate", "--sync", &window])?;
        let offset = h.text()?.len();
        h.xdo(&[
            "mousemove",
            "--window",
            &window,
            &(550 + frame[0]).to_string(),
            &(y + frame[2]).to_string(),
        ])?;
        std::thread::sleep(Duration::from_millis(120));
        h.xdo(&["click", "1"])?;
        changed.push(name);
        h.wait(20, |this| {
            Ok(this
                .text()?
                .get(offset..)
                .ok_or("capture app log shrank")?
                .lines()
                .any(|line| {
                    changed.iter().all(|name| {
                        line.contains(&format!(
                            "{name}={}",
                            if enabled { "Running" } else { "Disabled" }
                        ))
                    })
                }))
        })?;
        let mut event = json!({"plugin":name,"observed_seconds":started.elapsed().as_secs_f64()});
        event[if enabled { "enabled" } else { "disabled" }] = json!(changed);
        events.push(event);
        std::thread::sleep(Duration::from_millis(650));
    }
    if enabled {
        key(&h, "Escape")?;
    }
    std::thread::sleep(Duration::from_millis(if enabled { 5200 } else { 1200 }));
    recorder.signal(Signal::SIGINT)?;
    if !matches!(
        recorder.wait(Duration::from_secs(20))?.code(),
        Some(0 | 255)
    ) {
        return Err("recorder failed while finalizing plugin capture".into());
    }
    h.clean_log(&h.log)?;
    h.stop(false)?;
    for (path, hash, _) in originals {
        if digest(&path)? != hash {
            return Err("capture plugin bytes changed".into());
        }
    }
    if fs::metadata(&movie)?.len() == 0 {
        return Err("plugin capture video absent".into());
    }
    write_report(&h.base, &format!("{clip}-events.json"), &json!(events))?;
    let report = json!({"clip":clip,"sha256":digest(&movie)?,"bytes":fs::metadata(movie)?.len(),"events":events,"evidence_directory":h.base,"privacy_review":"pending; owned display only, no approved source replaced"});
    write_report(&output, "plugin-capture.json", &report)?;
    Ok(report)
}

fn digest_bytes(bytes: &[u8]) -> Result<String> {
    storage::sha256(bytes)
}
