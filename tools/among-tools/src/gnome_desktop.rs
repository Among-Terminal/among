//! Observe AppImageLauncher matching in an owned GNOME Wayland namespace.
use crate::{deb_installed::Runner, process::OwnedProcess, storage, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::Path,
    time::{Duration, Instant},
};

// the native shell automation hook enables inspection only inside the owned
// compositor; the Rust driver owns its lifetime and all assertions.
const AUTOMATION: &str = r#"import GLib from 'gi://GLib';
export function init() { global.context.unsafe_mode = true; }
export async function run() {
    await new Promise(resolve => GLib.timeout_add(GLib.PRIORITY_DEFAULT, 180000,
        () => { resolve(); return GLib.SOURCE_REMOVE; }));
}
"#;

fn eval(runner: &mut Runner, code: &str) -> Result<String> {
    let text = runner.command(
        "/usr/bin/gdbus",
        &[
            "call",
            "--session",
            "--dest",
            "org.gnome.Shell",
            "--object-path",
            "/org/gnome/Shell",
            "--method",
            "org.gnome.Shell.Eval",
            code,
        ],
        15,
    )?;
    if !text.trim_start().starts_with("(true,") {
        return Err(format!("owned GNOME inspection failed: {text}").into());
    }
    Ok(text)
}

fn observe(runner: &mut Runner, base: &Path, phase: &str) -> Result<Value> {
    let observation = base.join(format!("{phase}.json"));
    let output_js = serde_json::to_string(&observation)?;
    eval(runner, &format!("(async () => {{ const {{default: Shell}} = await import('gi://Shell'); const {{default: Gio}} = await import('gi://Gio'); const tracker = Shell.WindowTracker.get_default(); const windows = global.get_window_actors().map(actor => {{ const w = actor.meta_window; const app = tracker.get_window_app(w); const exe = Gio.File.new_for_path('/proc/'+w.get_pid()+'/exe').query_info('standard::symlink-target',Gio.FileQueryInfoFlags.NOFOLLOW_SYMLINKS,null).get_symlink_target(); return {{title:w.get_title(),wm_class:w.get_wm_class(),gtk_id:w.get_gtk_application_id(),pid:w.get_pid(),sequence:w.get_stable_sequence(),exe,app_id:app?.get_id(),icon:app?.get_app_info()?.get_icon()?.to_string()}} }}); Gio.File.new_for_path({output_js}).replace_contents(JSON.stringify({{windows,favorites:global.settings.get_strv('favorite-apps')}}),null,false,Gio.FileCreateFlags.REPLACE_DESTINATION,null); return true; }})()"))?;
    storage::parse_json(&fs::read(observation)?)
}

fn wait_input(shell: &mut OwnedProcess, input: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !input.exists() {
        if !shell.is_running()? || Instant::now() >= deadline {
            return Err("integrated AppImage did not accept input; inspect gnome-shell.log".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn screenshot(runner: &mut Runner, path: &Path) -> Result<()> {
    runner.command(
        "/usr/bin/gdbus",
        &[
            "call",
            "--session",
            "--dest",
            "org.gnome.Shell.Screenshot",
            "--object-path",
            "/org/gnome/Shell/Screenshot",
            "--method",
            "org.gnome.Shell.Screenshot.Screenshot",
            "false",
            "false",
            path.to_str().ok_or("screenshot path is not UTF-8")?,
        ],
        15,
    )?;
    Ok(())
}

fn click_button(runner: &mut Runner, text: &str, name: &str) -> Result<()> {
    let [x, y, width, height] = crate::sessions::rectangles(text, name)
        .last()
        .copied()
        .ok_or("dialog did not allocate its button")?;
    eval(runner, &format!("(async () => {{ const {{default: GLib}} = await import('gi://GLib'); const frame = global.get_window_actors()[0].meta_window.get_frame_rect(); global.ownedPointer.notify_absolute_motion(GLib.get_monotonic_time(),frame.x+{},frame.y+{}); }})()", x+width/2.0,y+height/2.0))?;
    std::thread::sleep(Duration::from_millis(250));
    for state in ["PRESSED", "RELEASED"] {
        eval(runner, &format!("(async () => {{ const {{default: Clutter}} = await import('gi://Clutter'); const {{default: GLib}} = await import('gi://GLib'); global.ownedPointer.notify_button(GLib.get_monotonic_time(),1,Clutter.ButtonState.{state}); }})()"))?;
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

pub fn run(image: &Path, output: &Path, scratch: &Path, native_present: bool) -> Result<()> {
    for name in [
        "bwrap",
        "gnome-shell",
        "gdbus",
        "gjs",
        "ail-cli",
        "dbus-daemon",
    ] {
        if !Path::new("/usr/bin").join(name).is_file() {
            return Err(format!("isolated GNOME check needs {name}").into());
        }
    }
    if !image.is_file() || output.exists() {
        return Err("an existing AppImage and a new evidence directory are required".into());
    }
    fs::DirBuilder::new().mode(0o700).create(output)?;
    let base = output.canonicalize()?;
    if base
        .to_str()
        .ok_or("evidence path is not UTF-8")?
        .contains(['\'', '\n', '\r', '~', ':'])
    {
        return Err("evidence path contains characters unsupported by the action script".into());
    }
    for name in ["home", "config", "data", "cache", "state"] {
        fs::create_dir(base.join(name))?;
    }
    // Unix sockets cannot use the longer evidence-root pathname.
    let runtime = storage::private_directory(scratch, "among-wl")?;
    let session_bus = format!("unix:path={}/session-bus", runtime.display());
    let system_bus = format!("unix:path={}/system-bus", runtime.display());
    let mut env = BTreeMap::from([
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("LANG".into(), "C.UTF-8".into()),
        ("HOME".into(), base.join("home").into_os_string()),
        (
            "XDG_CONFIG_HOME".into(),
            base.join("config").into_os_string(),
        ),
        ("XDG_DATA_HOME".into(), base.join("data").into_os_string()),
        ("XDG_CACHE_HOME".into(), base.join("cache").into_os_string()),
        ("XDG_STATE_HOME".into(), base.join("state").into_os_string()),
        ("XDG_RUNTIME_DIR".into(), runtime.clone().into_os_string()),
        ("DBUS_SESSION_BUS_ADDRESS".into(), session_bus.into()),
        ("DBUS_SYSTEM_BUS_ADDRESS".into(), system_bus.into()),
        ("XDG_SESSION_TYPE".into(), "wayland".into()),
        ("XDG_CURRENT_DESKTOP".into(), "GNOME".into()),
        ("WAYLAND_DISPLAY".into(), "among-wl-0".into()),
        ("LIBGL_ALWAYS_SOFTWARE".into(), "1".into()),
        ("NO_AT_BRIDGE".into(), "1".into()),
        ("SHELL".into(), "/bin/sh".into()),
        // setuid FUSE mounting is unavailable in an unprivileged namespace;
        // the AppImage runtime still launches the integrated file's payload.
        ("APPIMAGE_EXTRACT_AND_RUN".into(), "1".into()),
    ]);
    let copy = base.join("candidate.AppImage");
    fs::copy(image, &copy)?;
    fs::set_permissions(&copy, fs::Permissions::from_mode(0o755))?;
    let digest = storage::sha256(File::open(&copy)?)?;
    let mut runner = Runner::new(env.clone(), base.clone());
    let versions = runner.command(
        "/usr/bin/dpkg-query",
        &[
            "-W",
            "-f=${binary:Package}\t${Version}\n",
            "gnome-shell",
            "appimagelauncher",
            "libgtk-4-1",
        ],
        10,
    )?;
    fs::write(base.join("consumer-versions.tsv"), versions)?;
    fs::create_dir_all(base.join("data/applications"))?;
    let automation = base.join("automation.js");
    fs::write(&automation, AUTOMATION)?;
    fs::write(base.join("empty.desktop"), b"")?;
    let input = base.join("terminal-input");
    let close = base.join("close-terminal");
    let script = base.join("input.script");
    let child_environment = base.join("terminal-environment");
    fs::write(
        &script,
        format!(
            "type:env~>~'{}';~printf~gnome-input~>~'{}';~while~[~!~-e~'{}'~];~do~sleep~0.1;~done;~exit\n",
            child_environment.display(),
            input.display(),
            close.display()
        ),
    )?;
    env.insert("AMONG_SCRIPT".into(), script.into_os_string());
    env.insert("AMONG_SELFCHECK".into(), "5".into());
    let base_text = base.to_str().ok_or("evidence path is not UTF-8")?;
    let runtime_text = runtime.to_str().ok_or("runtime path is not UTF-8")?;
    let mut args = vec![
        "--ro-bind",
        "/",
        "/",
        "--dev-bind",
        "/dev",
        "/dev",
        "--proc",
        "/proc",
        "--tmpfs",
        "/run",
        "--tmpfs",
        "/tmp",
        "--bind",
        base_text,
        base_text,
        "--bind",
        runtime_text,
        runtime_text,
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-net",
        "--new-session",
        "--die-with-parent",
    ];
    let empty = base.join("empty.desktop");
    let native = "/usr/share/applications/com.github.among-terminal.desktop";
    if native_present && !Path::new(native).is_file() {
        return Err("coexistence check requires the installed native desktop entry".into());
    }
    if native_present {
        let status = runner.command(
            "/usr/bin/dpkg-query",
            &["-W", "-f=${Status}\n", "among-terminal"],
            10,
        )?;
        if status.trim() != "install ok installed" || !Path::new("/usr/bin/among").is_file() {
            return Err("coexistence check requires a configured native DEB".into());
        }
    }
    if !native_present && Path::new(native).exists() {
        args.extend([
            "--ro-bind",
            empty.to_str().ok_or("mask path is not UTF-8")?,
            native,
        ]);
    }
    args.extend(["--", "/bin/sh", "-ec", "dbus-daemon --session --address=\"$DBUS_SYSTEM_BUS_ADDRESS\" --fork; dbus-daemon --session --address=\"$DBUS_SESSION_BUS_ADDRESS\" --fork; exec gnome-shell --wayland --headless --virtual-monitor=1280x900 --no-x11 --wayland-display=among-wl-0 --sm-disable --automation-script \"$1\"", "gnome-driver", automation.to_str().ok_or("automation path is not UTF-8")?]);
    let log = File::create(base.join("gnome-shell.log"))?;
    let mut shell = OwnedProcess::spawn(Path::new("/usr/bin/bwrap"), &args, &env, &base, &log)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if !shell.is_running()? || Instant::now() >= deadline {
            return Err(format!(
                "owned GNOME did not become inspectable; evidence: {}",
                base.display()
            )
            .into());
        }
        let (success, text) = runner.execute(
            "/usr/bin/gdbus",
            &[
                "call",
                "--session",
                "--dest",
                "org.gnome.Shell",
                "--object-path",
                "/org/gnome/Shell",
                "--method",
                "org.gnome.Shell.Eval",
                "(async () => { const Main = await import('resource:///org/gnome/shell/ui/main.js'); return !Main.layoutManager._startingUp; })()",
            ],
            2,
        )?;
        if success && text.trim() == "(true, 'true')" {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // A pre-integrated profile misses icon/app-info cache races in a running
    // desktop. Integrate while the owned compositor is already observing it.
    runner.command(
        "/usr/bin/ail-cli",
        &["integrate", copy.to_str().ok_or("image path is not UTF-8")?],
        60,
    )?;
    let entries = fs::read_dir(base.join("data/applications"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    let entries = entries
        .iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "desktop"))
        .collect::<Vec<_>>();
    if entries.len() != 1 {
        return Err("AppImageLauncher must generate exactly one owned desktop entry".into());
    }
    let entry = entries[0];
    let id = entry
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("desktop ID missing")?;
    fs::copy(entry, base.join("integrated.desktop"))?;
    // ail-cli writes thumbnails under HOME even with XDG_CACHE_HOME set.
    // Correct themed SVG bytes do not prove these PNGs contain visible pixels.
    let hash = id
        .strip_prefix("appimagekit_")
        .and_then(|value| value.strip_suffix("-Among_Terminal.desktop"))
        .ok_or("unexpected AppImageLauncher desktop ID")?;
    let mut thumbnail_paths = Vec::new();
    for (kind, size) in [("normal", 128), ("large", 256)] {
        let copy = base.join(format!("thumbnail-{size}.png"));
        fs::copy(
            base.join(format!("home/.cache/thumbnails/{kind}/{hash}.png")),
            &copy,
        )?;
        thumbnail_paths.push(copy);
    }
    let decoded = runner.command("/usr/bin/gjs", &[
        "-c",
        "const P=imports.gi.GdkPixbuf; const result=ARGV.map((path,index)=>{const p=P.Pixbuf.new_from_file(path);const size=[128,256][index];if(p.width!==size||p.height!==size)throw Error('wrong thumbnail dimensions');const bytes=p.get_pixels(),stride=p.rowstride,c=p.n_channels;let visible=0,gold=0,dark=0;for(let y=0;y<size;y++)for(let x=0;x<size;x++){const i=y*stride+x*c,a=c===4?bytes[i+3]:255;if(a>128){visible++;const r=bytes[i],g=bytes[i+1],b=bytes[i+2];if(r>200&&g>80&&g<230&&b<80)gold++;if(r<80&&g<100&&b<120)dark++;}}if(gold<size*size/100||dark<size*size/100)throw Error('thumbnail lacks visible approved mark: '+path);return {size,visible,gold,dark};});print(JSON.stringify(result));",
        thumbnail_paths[0].to_str().ok_or("thumbnail path is not UTF-8")?,
        thumbnail_paths[1].to_str().ok_or("thumbnail path is not UTF-8")?,
    ], 15)?;
    let thumbnails = storage::parse_json(decoded.as_bytes())?;
    fs::write(
        base.join("thumbnails.json"),
        serde_json::to_vec_pretty(&thumbnails)?,
    )?;
    eval(&mut runner, "(async () => { const {default: Clutter} = await import('gi://Clutter'); const seat = global.stage.context.get_backend().get_default_seat(); global.ownedPointer = seat.create_virtual_device(Clutter.InputDeviceType.POINTER_DEVICE); global.ownedKeyboard = seat.create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE); })()")?;
    let id_js = serde_json::to_string(id)?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let visible = eval(&mut runner, &format!("(async () => {{ const {{default: Shell}} = await import('gi://Shell'); return Boolean(Shell.AppSystem.get_default().lookup_app({id_js})); }})()"))?;
        if visible.trim() == "(true, 'true')" {
            break;
        }
        if !shell.is_running()? || Instant::now() >= deadline {
            return Err("running GNOME did not discover the new integrated entry".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let activate = format!("(async () => {{ const {{default: Shell}} = await import('gi://Shell'); const app = Shell.AppSystem.get_default().lookup_app({id_js}); if (!app) throw new Error('integrated entry not found'); app.activate(); return true; }})()");
    eval(&mut runner, "global.settings.set_strv('favorite-apps', []); (async () => { const {default: Gio} = await import('gi://Gio'); new Gio.Settings({schema_id:'org.gnome.desktop.notifications'}).set_boolean('show-banners',false); })()")?;
    // Opening the integrated file itself does not supply GIO desktop-launch
    // hints. A later menu activation must not leave that first window unbranded.
    let entry_text = fs::read_to_string(entry)?;
    let integrated_image = entry_text
        .lines()
        .find_map(|line| line.strip_prefix("Exec="))
        .ok_or("integrated executable missing")?;
    if !integrated_image.starts_with('/') || integrated_image.chars().any(char::is_whitespace) {
        return Err("test integration executable must be one absolute path".into());
    }
    let image_js = serde_json::to_string(integrated_image)?;
    eval(&mut runner, &format!("(async () => {{ const {{default: Gio}} = await import('gi://Gio'); Gio.Subprocess.new([{image_js}],Gio.SubprocessFlags.NONE); }})()"))?;
    wait_input(&mut shell, &input)?;
    let unhinted = observe(&mut runner, &base, "unhinted-file-launch")?;
    screenshot(&mut runner, &base.join("unhinted-file-launch.png"))?;
    eval(&mut runner, &activate)?;
    wait_input(&mut shell, &input)?;
    eval(&mut runner, "(async () => { const Main = await import('resource:///org/gnome/shell/ui/main.js'); Main.overview.hide(); global.get_window_actors()[0].meta_window.activate(global.get_current_time()); })()")?;
    let menu = observe(&mut runner, &base, "menu-launch")?;
    eval(
        &mut runner,
        &format!("global.settings.set_strv('favorite-apps', [{id_js}])"),
    )?;
    eval(&mut runner, &activate)?;
    std::thread::sleep(Duration::from_millis(500));
    let pin = observe(&mut runner, &base, "pin-activation")?;
    fs::write(&close, b"close owned shell")?;
    // exiting the last shell opens Among's normal confirmation dialog.
    std::thread::sleep(Duration::from_secs(2));
    let log_text = fs::read_to_string(base.join("gnome-shell.log"))?;
    click_button(&mut runner, &log_text, "Exit")?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let closed = observe(&mut runner, &base, "after-close")?;
        if closed["windows"].as_array().is_some_and(Vec::is_empty) {
            break;
        }
        if Instant::now() >= deadline {
            let state = eval(&mut runner, "JSON.stringify({pointer:global.get_pointer(),focus:global.display.focus_window?.get_title(),frame:global.get_window_actors()[0].meta_window.get_frame_rect(),stage:[global.stage.width,global.stage.height]})")?;
            fs::write(base.join("input-state.txt"), state)?;
            screenshot(&mut runner, &base.join("close-failure.png"))?;
            return Err("terminal did not close normally".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    fs::remove_file(&input)?;
    fs::remove_file(&close)?;
    let script = base.join("input.script");
    let content = fs::read_to_string(&script)?;
    fs::write(
        &script,
        format!("{}{content}", "expand-sidebar\n".repeat(10)),
    )?;
    let previous_log_length = fs::metadata(base.join("gnome-shell.log"))?.len() as usize;
    eval(&mut runner, &activate)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let text = fs::read_to_string(base.join("gnome-shell.log"))?;
        let text = text
            .get(previous_log_length..)
            .ok_or("owned log was truncated")?;
        if !crate::sessions::rectangles(text, "Restore").is_empty() {
            click_button(&mut runner, text, "Restore")?;
            break;
        }
        if Instant::now() >= deadline {
            return Err("saved-session prompt did not appear".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    wait_input(&mut shell, &input)?;
    let relaunch = observe(&mut runner, &base, "pin-relaunch")?;
    let child_environment = fs::read_to_string(child_environment)?;
    let clean_environment = [
        "GIO_LAUNCHED_DESKTOP_FILE",
        "GIO_LAUNCHED_DESKTOP_FILE_PID",
        "AMONG_APPDIR",
        "LD_LIBRARY_PATH",
        "GSETTINGS_SCHEMA_DIR",
    ]
    .iter()
    .all(|key| {
        !child_environment
            .lines()
            .any(|line| line.starts_with(&format!("{key}=")))
    });
    let desktop = fs::read_to_string(entry)?;
    let icon = desktop
        .lines()
        .find_map(|line| line.strip_prefix("Icon="))
        .ok_or("integrated icon is missing")?;
    let icon_file = storage::safe_path(
        &base.join("data"),
        Path::new(&format!("icons/hicolor/scalable/apps/{icon}.svg")),
    )?;
    let approved_artwork =
        fs::read(icon_file)? == include_bytes!("../../../docs/media/logo-small.svg");
    let matched = [&unhinted, &menu, &pin, &relaunch].iter().all(|value| {
        value["windows"].as_array().is_some_and(|windows| {
            windows.len() == 1
                && windows[0]["gtk_id"] == "com.github.among-terminal"
                && windows[0]["app_id"] == id
                && windows[0]["exe"]
                    .as_str()
                    .is_some_and(|exe| exe.starts_with("/tmp/") && exe.ends_with("/usr/bin/among"))
        })
    }) && menu["windows"][0]["sequence"] == pin["windows"][0]["sequence"]
        && menu["windows"][0]["pid"] != relaunch["windows"][0]["pid"];
    fs::write(
        base.join("result.json"),
        serde_json::to_vec_pretty(
            &json!({"image_sha256":digest,"desktop_id":id,"native_present":native_present,"matched":matched,"approved_artwork":approved_artwork,"thumbnails":thumbnails,"clean_child_environment":clean_environment,"unhinted_file_launch":unhinted,"menu_launch":menu,"pin_activation":pin,"pin_relaunch":relaunch,"fuse_mode":"runtime extract-and-run inside owned namespace"}),
        )?,
    )?;
    eval(&mut runner, "(async () => { const Main = await import('resource:///org/gnome/shell/ui/main.js'); Main.overview.show(); })()")?;
    std::thread::sleep(Duration::from_secs(1));
    screenshot(&mut runner, &base.join("desktop.png"))?;
    fs::write(close, b"close owned shell")?;
    shell.cleanup()?;
    if !matched || !approved_artwork || !clean_environment {
        return Err(format!(
            "AppImageLauncher association/artwork/environment check failed; evidence: {}",
            base.display()
        )
        .into());
    }
    println!("PASS owned GNOME Wayland window matches its integrated AppImageLauncher entry; evidence: {}", base.display());
    Ok(())
}
