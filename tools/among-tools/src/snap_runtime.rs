use crate::{
    platform, preferences,
    sessions::{fresh, pair, pane_rectangles, Harness},
    storage, Result,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::Duration,
};

/// Read `meta/snap.yaml` out of a real Snap and parse it.
///
/// Packaged identity is the authority for what a promotion may claim; a
/// filename or a record field is not evidence of what is inside the artifact.
pub fn snap_metadata(snap: &Path) -> Result<Value> {
    if !snap.symlink_metadata()?.file_type().is_file() {
        return Err("Snap artifact must be a regular file".into());
    }
    let base = crate::storage::private_directory(&std::env::temp_dir(), "among-snap-metadata")?;
    let metadata = base.join("snap.yaml");
    let errors = fs::File::create(base.join("snap.yaml.stderr"))?;
    let env = BTreeMap::from([
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("LC_ALL".into(), "C.UTF-8".into()),
    ]);
    let mut process = crate::process::OwnedProcess::spawn_split(
        Path::new("/usr/bin/unsquashfs"),
        &[
            "-cat",
            snap.to_str().ok_or("Snap path is not UTF-8")?,
            "meta/snap.yaml",
        ],
        &env,
        &base,
        &fs::File::create(&metadata)?,
        &errors,
    )?;
    if !process.wait(Duration::from_secs(60))?.success() {
        return Err("cannot read packaged Snap metadata".into());
    }
    let text = fs::read_to_string(&metadata)?;
    let document: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&text).map_err(|_| "packaged Snap metadata is not valid YAML")?;
    serde_json::to_value(document)
        .map_err(|_| "packaged Snap metadata cannot be represented as JSON".into())
}

fn require(value: bool, message: &str) -> Result<()> {
    if value {
        Ok(())
    } else {
        Err(message.into())
    }
}
pub fn approval(github_hosted: bool) -> Result<()> {
    preferences::owned_display()?;
    let virtualization = std::process::Command::new("/usr/bin/systemd-detect-virt")
        .env_clear()
        .output()?;
    let guest = matches!(
        std::str::from_utf8(&virtualization.stdout)?.trim(),
        "kvm" | "qemu"
    );
    let approved = platform::hosted_rehearsal(
        github_hosted,
        std::env::var("GITHUB_ACTIONS").ok().as_deref(),
        std::env::var("RUNNER_ENVIRONMENT").ok().as_deref(),
    );
    require(
        (guest || approved) && Path::new("/snap/among/current/bin/among").is_file(),
        "installed Snap runtime requires a disposable guest or explicitly approved hosted runner",
    )
}
pub fn host_probe(output: &Path) -> Result<()> {
    require(
        std::env::var("AMONG_HOST_MARKER").as_deref() == Ok("vm-fixture"),
        "host probe requires the synthetic runtime marker",
    )?;
    let root = output.parent().ok_or("host probe parent absent")?;
    let root = root.canonicalize()?;
    require(
        root.starts_with("/tmp/opencode")
            && root.metadata()?.uid() == nix::unistd::geteuid().as_raw(),
        "host probe requires an owned private evidence directory",
    )?;
    let name = output.file_name().ok_or("host probe filename absent")?;
    require(!root.join(name).exists(), "host probe output must be new")?;
    let observed = json!({"cwd":std::env::current_dir()?,"home":std::env::var("HOME")?,"marker":std::env::var("AMONG_HOST_MARKER")?,"maps":fs::read_to_string("/proc/self/maps")?,"path":std::env::var("PATH")?,"ld_library_path":std::env::var("LD_LIBRARY_PATH").unwrap_or_default(),"schema_dir":std::env::var("GSETTINGS_SCHEMA_DIR")?,"data_dirs":std::env::var("XDG_DATA_DIRS")?});
    storage::atomic_write(
        &root,
        Path::new(name),
        &serde_json::to_vec(&observed)?,
        0o600,
    )
}

// This is an external runtime-test fixture, never one of the shipped plugins.
// Real pipe messages drive its replies; it cannot substitute for plugin lifecycle
// observation in the application under test.
pub fn external_plugin() -> Result<()> {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    loop {
        let mut bytes = Vec::new();
        let size = input.by_ref().take(65537).read_until(b'\n', &mut bytes)?;
        if size == 0 {
            return Ok(());
        }
        if size > 65536 || !bytes.ends_with(b"\n") {
            return Err("external plugin fixture message exceeds limit or lacks newline".into());
        }
        let message = storage::parse_json(&bytes)?;
        let response = match message["type"].as_str() {
            Some("shutdown") => return Ok(()),
            Some("hello") => json!({"type":"hello","major":1,"minor":0}),
            Some("status") => {
                require(
                    message["request_id"].as_u64().is_some() && message["scope"].as_str().is_some(),
                    "external plugin status requires a numeric request ID and scope",
                )?;
                json!({"type":"status","request_id":message["request_id"],"refresh_ms":1000,"fragments":if message["scope"]=="app_header" {vec![json!({"text":"VM-EXTERNAL"})]} else {vec![]}})
            }
            _ => continue,
        };
        serde_json::to_writer(&mut output, &response)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
}
fn width(text: &str, id: u64) -> f64 {
    text.lines()
        .rev()
        .find_map(|line| {
            pane_rectangles(line, &id.to_string())
                .last()
                .map(|bounds| bounds[2])
        })
        .unwrap_or(0.0)
}
fn fresh_state(h: &mut Harness, key: &str, test: impl Fn(&str) -> bool) -> Result<()> {
    let offset = h.text()?.len();
    h.key(key)?;
    h.wait(25, |this| Ok(this.text()?.get(offset..).is_some_and(&test)))
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
pub fn run(github_hosted: bool, scratch: &Path) -> Result<PathBuf> {
    approval(github_hosted)?;
    let mut h = fresh(
        Path::new("/snap/bin/among"),
        scratch,
        "among-snap-runtime-rust",
    )?;
    let tool = std::env::current_exe()?;
    let tools = h.base.join("tools");
    fs::create_dir(&tools)?;
    let plugin_dir = h.base.join("data/among/plugins");
    fs::create_dir(&plugin_dir)?;
    for name in ["project", "host-schemas", "host-data"] {
        fs::create_dir(h.base.join(name))?;
    }
    let startup = h.base.join("startup");
    fs::write(
        h.base.join("home/.bashrc"),
        format!(
            "printf 'startup\\n' >> {}\n",
            quote(startup.to_str().ok_or("startup path not UTF-8")?)
        ),
    )?;
    storage::atomic_write(
        &tools,
        Path::new("among-host-tool"),
        format!(
            "#!/bin/sh\nexec {} snap-host-probe \"$@\"\n",
            quote(tool.to_str().ok_or("tool path not UTF-8")?)
        )
        .as_bytes(),
        0o755,
    )?;
    storage::atomic_write(
        &plugin_dir,
        Path::new("vm-external"),
        format!(
            "#!/bin/sh\nexec {} snap-external-plugin\n",
            quote(tool.to_str().ok_or("tool path not UTF-8")?)
        )
        .as_bytes(),
        0o755,
    )?;
    let original = serde_json::to_vec(&json!({"plugins":[{"name":"vm-external","enabled":true}]}))?;
    let config = h.base.join("config/among/plugins.json");
    fs::write(&config, &original)?;
    let baseline_ld = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
    h.env
        .insert("LD_LIBRARY_PATH".into(), baseline_ld.clone().into());
    let path = h
        .env
        .get(&std::ffi::OsString::from("PATH"))
        .ok_or("host PATH absent")?
        .to_str()
        .ok_or("host PATH not UTF-8")?;
    h.env
        .insert("PATH".into(), format!("{}:{path}", tools.display()).into());
    h.env.insert("SHELL".into(), "/bin/bash".into());
    h.env
        .insert("AMONG_HOST_MARKER".into(), "vm-fixture".into());
    h.env.insert(
        "GSETTINGS_SCHEMA_DIR".into(),
        h.base.join("host-schemas").into_os_string(),
    );
    h.env.insert(
        "XDG_DATA_DIRS".into(),
        h.base.join("host-data").into_os_string(),
    );
    let pid = h.start("app")?;
    let result = (|| -> Result<()> {
        h.wait(25, |this| {
            let text = this.text()?;
            Ok(text.contains("session_ready=true")
                && text.contains("has_focus=true")
                && text.contains("vm-external=Running")
                && text.contains("VM-EXTERNAL"))
        })?;
        let proc = PathBuf::from(format!("/proc/{pid}"));
        let executable = fs::read_link(proc.join("exe"))?;
        require(
            executable.starts_with("/snap/among/"),
            "runtime executable is not the installed Snap",
        )?;
        let maps = fs::read_to_string(proc.join("maps"))?;
        require(
            maps.contains("/snap/among/")
                && maps.contains("libgtk-4")
                && maps.contains("libvte-2.91-gtk4"),
            "installed Snap maps lack native GTK/VTE",
        )?;
        fs::write(h.base.join("app-maps.txt"), maps)?;
        for name in [
            "preferences-system-symbolic",
            "sidebar-show-symbolic",
            "tab-new-symbolic",
        ] {
            let text = h.text()?;
            require(
                text.contains(&format!("{name}:true"))
                    && text.contains(&format!("header_icon_decode={name}:true")),
                "installed symbolic icon lookup/decode failed",
            )?;
        }
        let output = h.base.join("host-result.json");
        h.command(&format!(
            "cd {} && among-host-tool {} && printf '\\033]0;vm-host-command-ok\\007'",
            quote(
                h.base
                    .join("project")
                    .to_str()
                    .ok_or("project path not UTF-8")?
            ),
            quote(output.to_str().ok_or("host result path not UTF-8")?)
        ))?;
        h.wait(25, |this| {
            Ok(output.is_file() && this.text()?.contains("vm-host-command-ok"))
        })?;
        let observed = storage::parse_json(&fs::read(output)?)?;
        require(
            fs::read_to_string(startup)? == "startup\n",
            "real shell startup did not run exactly once",
        )?;
        for (name, directory) in [
            ("cwd", "project"),
            ("home", "home"),
            ("schema_dir", "host-schemas"),
            ("data_dirs", "host-data"),
        ] {
            require(
                observed[name]
                    == h.base
                        .join(directory)
                        .to_str()
                        .ok_or("fixture directory not UTF-8")?,
                "host subprocess environment or cwd changed",
            )?;
        }
        require(
            observed["marker"] == "vm-fixture" && observed["ld_library_path"] == baseline_ld,
            "host marker or LD_LIBRARY_PATH changed",
        )?;
        require(
            observed["path"]
                .as_str()
                .is_some_and(|path| std::env::split_paths(path).any(|path| path == tools)),
            "host tool path was not preserved",
        )?;
        let host_maps = observed["maps"].as_str().ok_or("host maps absent")?;
        require(
            !host_maps.contains("/snap/among/") && !host_maps.contains("/snap/core24/"),
            "Snap libraries contaminated the host tool",
        )?;
        require(
            fs::read(&config)? == original,
            "external plugin configuration was mutated",
        )?;
        fresh_state(&mut h, "F11", |text| text.contains("fullscreen=true"))?;
        fresh_state(&mut h, "F11", |text| text.contains("fullscreen=false"))?;
        fresh_state(&mut h, "ctrl+shift+n", |text| {
            text.contains("visible=2 panes=2")
        })?;
        let before = width(&h.text()?, 2);
        require(before > 0.0, "split pane width absent")?;
        fresh_state(&mut h, "ctrl+shift+Right", |text| width(text, 2) > before)?;
        fresh_state(&mut h, "ctrl+shift+x", |text| {
            text.contains("visible=1 panes=2")
        })?;
        fresh_state(&mut h, "ctrl+shift+x", |text| {
            text.contains("visible=2 panes=2")
        })?;
        let geometry = h.geometry()?;
        let origin = pair(&geometry, "origin", ',')?;
        let left = pane_rectangles(&geometry, "1")
            .last()
            .copied()
            .ok_or("left pane rectangle missing")?;
        require(left[0] == 0.0 && left[1] == 0.0, "left pane origin changed")?;
        let before = width(&h.text()?, 2);
        let offset = h.text()?.len();
        let x = origin[0] + left[2] as i64;
        let y = origin[1] + (left[3] / 2.0) as i64;
        h.move_mouse(x, y)?;
        h.xdo(&["mousedown", "1"])?;
        std::thread::sleep(Duration::from_millis(200));
        h.move_mouse(x + 100, y)?;
        std::thread::sleep(Duration::from_millis(200));
        h.xdo(&["mouseup", "1"])?;
        h.wait(25, |this| {
            let text = this.text()?;
            let width = text.get(offset..).map(|text| width(text, 2)).unwrap_or(0.0);
            Ok(width > 0.0 && width < before)
        })?;
        h.screenshot("installed-window.png")?;
        h.clean_log(&h.log)?;
        println!("PASS installed Snap maps/icons, real shell startup, host environment/tool/plugin and fullscreen/split/resize/zoom/pointer controls; modified-VTE/debugger acceptance is separate");
        Ok(())
    })();
    h.stop(false)?;
    result?;
    println!("Evidence: {}", h.base.display());
    Ok(h.base.clone())
}
