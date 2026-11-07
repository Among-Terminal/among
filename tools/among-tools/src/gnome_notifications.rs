//! Real GNOME notification banners and pointer activation in an owned session.
//! Bus-contract checks remain separate; this observes the actual desktop shell.

use crate::{
    owned_gnome::{self, Mode, Session, ROOT},
    Result,
};
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const SCRIPT: &str = r#"type:printf~'\033]0;make\007\033]7;file://localhost/work\007'
await:initial
await:minimized
feed:1:printf~'\007'
await:minimized-clicked
await:desktop
feed:1:printf~'\007'
await:desktop-clicked
split-pane
toggle-zoom
await:hidden
feed:1:printf~'\007'
await:hidden-clicked
new-workspace
await:project
feed:1:printf~'\007'
await:project-clicked
await:closing-bell
feed:1:printf~'\007'
await:closing
close-pane
await:closed
"#;

// These fields are read from the installed GNOME Shell notification objects,
// not reconstructed from Among's outgoing call or from a test notification.
const SNAPSHOT: &str = r#"(async () => {
    const Main = await import('resource:///org/gnome/shell/ui/main.js');
    const {default: Meta} = await import('gi://Meta');
    const tray = Main.messageTray;
    const seat = global.stage.context.get_backend().get_default_seat();
    global.ownedNotificationTokens ??= new WeakMap();
    global.ownedNotificationSequence ??= 0;
    const token = item => {
        if (!item) return null;
        if (!global.ownedNotificationTokens.has(item))
            global.ownedNotificationTokens.set(item,++global.ownedNotificationSequence);
        return global.ownedNotificationTokens.get(item);
    };
    const allocation = actor => {
        if (!actor) return null;
        const position = actor.get_transformed_position();
        const size = actor.get_transformed_size();
        const [x,y] = position;
        const [width,height] = size;
        return {rect:[x,y,width,height], mapped:actor.mapped, visible:actor.visible,
            opacity:actor.get_paint_opacity(), reactive:actor.reactive,
            has_allocation:actor.has_allocation(),
            position_result:position.map(String), size_result:size.map(String)};
    };
    const windows = global.get_window_actors().map(actor => {
        const window = actor.meta_window;
        const clientType = window.get_client_type();
        return {pid: window.get_pid(), sequence: window.get_stable_sequence(),
            client_type: clientType === Meta.WindowClientType.WAYLAND ? 'wayland' :
                clientType === Meta.WindowClientType.X11 ? 'x11' : 'unknown',
            client_type_value: clientType,
            gtk_id: window.get_gtk_application_id(), title: window.get_title(),
            minimized: window.minimized, workspace: window.get_workspace()?.index(),
            focused: global.display.focus_window === window};
    });
    const notifications = Main.messageTray.getSources()
        .filter(source => source.policy.id === 'com.github.among-terminal')
        .flatMap(source => source.notifications.map(notification => ({
            id: notification.id, title: notification.title, body: notification.body,
            target: notification._defaultActionTarget?.deepUnpack() ?? null,
            acknowledged: notification.acknowledged, urgency: notification.urgency,
            policy: {enabled: source.policy.enable, banners: source.policy.showBanners},
        })));
    const banner = Main.messageTray._banner;
    const notification = banner?.notification;
    let shown = null;
    if (notification?.source.policy.id === 'com.github.among-terminal') {
        const [x, y] = banner.get_transformed_position();
        const [width, height] = banner.get_transformed_size();
        shown = {id: notification.id, rect: [x,y,width,height],
            mapped: banner.mapped, opacity: banner.get_paint_opacity(),
            offset_y: Main.messageTray._bannerBin.y};
    }
    return {windows, notifications, banner: shown,
        all_notifications: tray.getSources().flatMap(source => source.notifications.map(item => ({
            token: token(item), title:item.title, source:source.policy.id}))),
        shell: {notification_state: tray._notificationState,
            seat_devices: seat.list_devices().map(device => ({name:device.get_device_name(),
                type:device.get_device_type(), capabilities:device.get_capabilities()})),
            current_notification: tray._notification?.id ?? null,
            current_title: tray._notification?.title ?? null,
            raw_banner_present: banner !== null,
            raw_banner_notification: notification?.id ?? null,
            raw_banner_title: notification?.title ?? null,
            raw_banner_body: notification?.body ?? null,
            raw_banner_source: notification?.source.policy.id ?? null,
            raw_banner_class: banner?.constructor.name ?? null,
            raw_banner_fields: banner ? Object.keys(banner).sort() : [],
            raw_banner_children: banner?.get_children().map(child => ({
                name:child.name, class:child.constructor.name})) ?? [],
            raw_banner_token: token(notification),
            raw_banner_allocation: allocation(banner),
            raw_close_allocation: allocation(banner?._header.closeButton),
            banner_bin: {mapped: tray._bannerBin.mapped, opacity: tray._bannerBin.opacity,
                y: tray._bannerBin.y, visible: tray._bannerBin.visible},
            queue: tray._notificationQueue.map(item => ({id: item.id,
                title: item.title, acknowledged: item.acknowledged, urgency: item.urgency})),
            blocked: tray._bannerBlocked, busy: tray._busy,
            presence: tray._presence.status, idle_ms: tray.idleMonitor.get_idletime(),
            notifications_allowed: Main.sessionMode.hasNotifications,
            session_mode: Main.sessionMode.currentMode,
            overview_visible: Main.overview.visible,
            starting_up: Main.layoutManager._startingUp,
            monitor: Main.layoutManager.primaryMonitor ? {
                index: Main.layoutManager.primaryMonitor.index,
                fullscreen: Main.layoutManager.primaryMonitor.inFullscreen} : null},
        active_desktop: global.workspace_manager.get_active_workspace_index(),
        late_activation: global.ownedLateActivation ?? null,
        late_action_control: allocation(global.ownedLateButton),
        among_pid: global.ownedAmong?.get_identifier() ?? null};
})()"#;

pub fn run(
    source: &Path,
    binary: &Path,
    mode: &str,
    output: &Path,
    xvfb: Option<&Path>,
) -> Result<PathBuf> {
    owned_gnome::launch(source, binary, Mode::parse(mode)?, output, xvfb)
}

fn release(mark: &str) -> Result<()> {
    fs::write(
        Path::new(ROOT).join("input.script").with_extension(mark),
        b"continue",
    )?;
    Ok(())
}

fn app_log() -> Result<String> {
    match fs::read_to_string(Path::new(ROOT).join("among.log")) {
        Ok(log) => Ok(log),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

fn stage_diagnostics(session: &mut Session, mark: &str) {
    let observation = match snapshot(session, &format!("{mark}-failed")) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("failed to capture owned GNOME state: {error}");
            return;
        }
    };
    let Some(pid) = observation["among_pid"]
        .as_str()
        .and_then(|pid| pid.parse::<u32>().ok())
        .filter(|pid| *pid > 0)
    else {
        return;
    };
    let process = Path::new("/proc").join(pid.to_string());
    let read = |path: &Path| {
        fs::read_to_string(path).unwrap_or_else(|error| format!("unavailable: {error}"))
    };
    let mut threads = Vec::new();
    if let Ok(entries) = fs::read_dir(process.join("task")) {
        for entry in entries.flatten() {
            let path = entry.path();
            threads.push(json!({"tid":entry.file_name().to_string_lossy(),
                "status":read(&path.join("status")),"wait_channel":read(&path.join("wchan"))}));
        }
    }
    let environment = fs::read(process.join("environ"))
        .map(|bytes| {
            bytes
                .split(|byte| *byte == 0)
                .filter_map(|entry| std::str::from_utf8(entry).ok())
                .filter(|entry| {
                    [
                        "AMONG_",
                        "GDK_",
                        "GSK_",
                        "GTK_A11Y=",
                        "WAYLAND_DISPLAY=",
                        "DISPLAY=",
                        "DBUS_SESSION_BUS_ADDRESS=",
                    ]
                    .iter()
                    .any(|prefix| entry.starts_with(prefix))
                })
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let diagnostic = json!({"pid":pid,"status":read(&process.join("status")),
        "environment":environment,"thread_count":threads.len(),"threads":threads,
        "cgroup":read(&process.join("cgroup")),
        "cgroup_pids_events":read(Path::new("/sys/fs/cgroup/pids.events"))});
    let path = Path::new(ROOT).join(format!("{mark}-process.json"));
    let save = || -> Result<()> {
        fs::write(path, serde_json::to_vec_pretty(&diagnostic)?)?;
        Ok(())
    };
    if let Err(error) = save() {
        eprintln!("failed to save owned process diagnostics: {error}");
    }
}

fn wait_mark(session: &mut Session, mark: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if app_log()?
            .lines()
            .any(|line| line == format!("among: script awaits {mark}"))
        {
            return Ok(());
        }
        if !session.alive()? || Instant::now() >= deadline {
            stage_diagnostics(session, mark);
            return Err(
                format!("Among did not reach script stage {mark}; inspect among.log").into(),
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn snapshot(session: &mut Session, name: &str) -> Result<Value> {
    let observation = session.json(name, SNAPSHOT)?;
    let mut history = OpenOptions::new()
        .create(true)
        .append(true)
        .open(Path::new(ROOT).join("observations.jsonl"))?;
    serde_json::to_writer(&mut history, &json!({"phase":name,"state":observation}))?;
    history.write_all(b"\n")?;
    for window in observation["windows"]
        .as_array()
        .ok_or("GNOME window observation is not an array")?
        .iter()
        .filter(|window| window["gtk_id"] == "com.github.among-terminal")
    {
        if window["client_type"] != session.mode.name() {
            return Err(format!(
                "Among client type {} does not match {} proof at {name}",
                window["client_type"],
                session.mode.name()
            )
            .into());
        }
    }
    Ok(observation)
}

fn activity(session: &mut Session, mark: &str) -> Result<()> {
    // Motion enters through the compositor's virtual seat, so its ordinary
    // idle/presence tracking observes activity before the terminal emits BEL.
    session.json(
        &format!("{mark}-activity"),
        r#"(async () => {
        const {default: GLib} = await import('gi://GLib');
        const [x] = global.get_pointer();
        global.ownedPointer.notify_absolute_motion(GLib.get_monotonic_time(),x < 30 ? 40 : 20,850);
        return true;
    })()"#,
    )?;
    Ok(())
}

fn ring(session: &mut Session, mark: &str) -> Result<()> {
    activity(session, mark)?;
    release(mark)
}

fn wait_startup_notifications(session: &mut Session) -> Result<Vec<Value>> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut quiet_since = None;
    let mut dismissals = Vec::new();
    loop {
        let value = snapshot(session, "startup-notifications")?;
        if value["notifications"]
            .as_array()
            .is_none_or(|notifications| !notifications.is_empty())
        {
            return Err("Among notified before the first scripted bell".into());
        }
        let quiet = value["shell"]["raw_banner_present"] == false
            && value["shell"]["queue"]
                .as_array()
                .is_some_and(Vec::is_empty);
        if quiet {
            let start = quiet_since.get_or_insert_with(Instant::now);
            if start.elapsed() >= Duration::from_millis(500) {
                return Ok(dismissals);
            }
        } else {
            quiet_since = None;
            let close = &value["shell"]["raw_close_allocation"];
            if close["mapped"] == true
                && close["visible"] == true
                && close["opacity"] == 255
                && value["shell"]["banner_bin"]["y"] == 0
                && rectangle(&close["rect"]).is_ok()
            {
                let token = value["shell"]["raw_banner_token"]
                    .as_u64()
                    .ok_or("startup banner has no observed notification binding")?;
                let name = format!("startup-dismissal-{}", dismissals.len() + 1);
                session.screen(&format!("{name}-before"))?;
                let [x, y, width, height] = rectangle(&close["rect"])?;
                pointer_click(session, &name, x + width / 2.0, y + height / 2.0)?;
                let after = wait_snapshot(session, &format!("{name}-after"), |state| {
                    state["all_notifications"]
                        .as_array()
                        .is_some_and(|items| items.iter().all(|item| item["token"] != token))
                })?;
                dismissals.push(json!({"before":value,"after":after,"closed_token":token}));
                fs::write(
                    Path::new(ROOT).join("startup-dismissals.json"),
                    serde_json::to_vec_pretty(&dismissals)?,
                )?;
            }
        }
        if Instant::now() >= deadline {
            return Err("GNOME startup banners did not close; inspect startup-notifications.json and observations.jsonl".into());
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn wait_snapshot(
    session: &mut Session,
    name: &str,
    predicate: impl Fn(&Value) -> bool,
) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let observation = snapshot(session, name)?;
        if predicate(&observation) {
            return Ok(observation);
        }
        if !session.alive()? || Instant::now() >= deadline {
            return Err(format!("GNOME observation failed at {name}; inspect {name}.json").into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn among_window(value: &Value) -> Option<&Value> {
    value["windows"]
        .as_array()?
        .iter()
        .find(|window| window["gtk_id"] == "com.github.among-terminal")
}

fn assert_app_focus(value: &Value) -> bool {
    among_window(value)
        .is_some_and(|window| window["focused"] == true && window["minimized"] == false)
}

fn wait_pane_focus(after: usize, pane: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let log = app_log()?;
        let fresh = log.get(after..).ok_or("Among log was truncated")?;
        let state = fresh
            .lines()
            .rev()
            .find(|line| line.contains("workspaces=") && line.contains("focused=PaneId("));
        if state.is_some_and(|line| {
            line.contains("active=0 ")
                && line.contains(&format!("focused=PaneId({pane})"))
                && line.contains("has_focus=true")
        }) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "no fresh Among observation of focused terminal {pane}: {}",
                state.unwrap_or("no new self-check state")
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn other_project_active(log: &str) -> bool {
    let complete = log.rfind('\n').map_or("", |end| &log[..=end]);
    complete
        .lines()
        .rev()
        .find(|line| line.contains("workspaces=") && line.contains("focused=PaneId("))
        .is_some_and(|line| line.contains("workspaces=2 active=1 "))
}

fn wait_other_project(session: &mut Session, after: usize) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let log = app_log()?;
        fs::write(Path::new(ROOT).join("project-before-app.log"), &log)?;
        let fresh = log.get(after..).ok_or("Among log was truncated")?;
        if other_project_active(fresh) {
            return Ok(());
        }
        if !session.alive()? || Instant::now() >= deadline {
            return Err("other-project scenario did not activate Among workspace 1; inspect project-before-app.log".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn minimise(session: &mut Session, name: &str) -> Result<Value> {
    session.json(
        &format!("{name}-minimise-request"),
        r#"(() => {
        const window = global.get_window_actors().map(actor => actor.meta_window)
            .find(candidate => candidate.get_gtk_application_id() === 'com.github.among-terminal');
        if (!window) throw new Error('Among window is absent');
        window.minimize(); return true;
    })()"#,
    )?;
    wait_snapshot(session, &format!("{name}-before"), |value| {
        among_window(value)
            .is_some_and(|window| window["minimized"] == true && window["focused"] == false)
    })
}

fn notification(session: &mut Session, name: &str) -> Result<Value> {
    let value = wait_snapshot(session, &format!("{name}-notification"), |value| {
        value["notifications"]
            .as_array()
            .is_some_and(|notifications| notifications.len() == 1)
            && value["banner"]["mapped"] == true
            && value["banner"]["opacity"] == 255
            && value["banner"]["offset_y"]
                .as_f64()
                .is_some_and(|offset| offset.abs() < 0.5)
            && rectangle(&value["banner"]["rect"]).is_ok()
    })?;
    let notifications = value["notifications"]
        .as_array()
        .ok_or("GNOME notifications are not an array")?;
    let notification = &notifications[0];
    if notification["title"] != "project"
        || notification["body"] != "Bell in 'make' /work"
        || notification["target"]
            .as_array()
            .is_none_or(|target| target.len() != 4 || target[3] != 1)
        || notification["id"] != value["banner"]["id"]
    {
        return Err(
            format!("unexpected GNOME notification content at {name}: {notification}").into(),
        );
    }
    session.screen(&format!("{name}-notification"))?;
    Ok(value)
}

fn rectangle(value: &Value) -> Result<[f64; 4]> {
    let rectangle = value
        .as_array()
        .filter(|rect| rect.len() == 4)
        .ok_or("GNOME banner rectangle missing")?;
    let number = |index: usize| {
        rectangle[index]
            .as_f64()
            .filter(|value| value.is_finite())
            .ok_or("GNOME banner rectangle is not finite")
    };
    let (x, y, width, height) = (number(0)?, number(1)?, number(2)?, number(3)?);
    if width <= 0.0 || height <= 0.0 {
        return Err("GNOME banner has no clickable allocation".into());
    }
    Ok([x, y, width, height])
}

fn pointer_click(session: &mut Session, name: &str, x: f64, y: f64) -> Result<()> {
    session.json(&format!("{name}-pointer"), &format!("(async () => {{ const {{default: GLib}} = await import('gi://GLib'); global.ownedPointer.notify_absolute_motion(GLib.get_monotonic_time(),{x},{y}); return true; }})()"))?;
    for state in ["PRESSED", "RELEASED"] {
        session.json(&format!("{name}-button-{}", state.to_lowercase()), &format!("(async () => {{ const {{default: Clutter}} = await import('gi://Clutter'); const {{default: GLib}} = await import('gi://GLib'); global.ownedPointer.notify_button(GLib.get_monotonic_time(),1,Clutter.ButtonState.{state}); return true; }})()"))?;
    }
    Ok(())
}

fn click(session: &mut Session, name: &str, value: &Value) -> Result<Value> {
    let log_offset = app_log()?.len();
    let [x, y, width, height] = rectangle(&value["banner"]["rect"])?;
    // The left body area avoids the close and expand controls at the far right.
    pointer_click(session, name, x + width / 3.0, y + height / 2.0)?;
    let observation = wait_snapshot(session, &format!("{name}-after"), assert_app_focus)?;
    wait_pane_focus(log_offset, 1)?;
    session.screen(&format!("{name}-after"))?;
    Ok(observation)
}

/// Entry point of the copied checker inside its private filesystem/PID namespace.
pub fn inside(mode: &str) -> Result<()> {
    let mode = Mode::parse(mode)?;
    let mut session = Session::start(mode)?;
    fs::write(Path::new(ROOT).join("input.script"), SCRIPT)?;
    let versions = session
        .runner
        .command("/usr/bin/gnome-shell", &["--version"], 5)?;
    fs::write(Path::new(ROOT).join("gnome-version.txt"), versions)?;
    session.json("launch", r#"(async () => {
        const {default: Gio} = await import('gi://Gio');
        const {default: Shell} = await import('gi://Shell');
        const Main = await import('resource:///org/gnome/shell/ui/main.js');
        if (!Shell.AppSystem.get_default().lookup_app('com.github.among-terminal.desktop'))
            throw new Error('matching private desktop entry was not discovered');
        new Gio.Settings({schema_id:'org.gnome.desktop.notifications'}).set_boolean('show-banners',true);
        const launcher = new Gio.SubprocessLauncher({flags: Gio.SubprocessFlags.NONE});
        // The owned display exposes no GPU; use the existing software-rendered
        // test path and keep GTK4 from starting an unrelated accessibility bus.
        launcher.setenv('GSK_RENDERER','cairo',true);
        launcher.setenv('GTK_A11Y','none',true);
        launcher.set_stdout_file_path('/owned/among.stdout');
        launcher.set_stderr_file_path('/owned/among.log');
        global.ownedAmong = launcher.spawnv(['/owned/bin/among']);
        Main.overview.hide();
        return {pid: global.ownedAmong.get_identifier(),
            fixture_renderer: 'cairo', fixture_accessibility: 'none'};
    })()"#)?;
    wait_mark(&mut session, "initial")?;
    let ready = wait_snapshot(&mut session, "initial", |value| {
        among_window(value).is_some()
    })?;
    let startup_dismissals = wait_startup_notifications(&mut session)?;
    let mut scenarios = Vec::new();

    minimise(&mut session, "minimized")?;
    release("initial")?;
    wait_mark(&mut session, "minimized")?;
    ring(&mut session, "minimized")?;
    let shown = notification(&mut session, "minimized")?;
    scenarios.push(json!({"name":"minimized","notification":shown,"after":click(&mut session,"minimized",&shown)?}));
    release("minimized-clicked")?;

    wait_mark(&mut session, "desktop")?;
    session.json("switch-desktop", r#"(() => {
        const manager = global.workspace_manager;
        const workspace = manager.get_workspace_by_index(1) ?? manager.append_new_workspace(false,global.get_current_time());
        workspace.activate(global.get_current_time()); return workspace.index();
    })()"#)?;
    wait_snapshot(&mut session, "desktop-before", |value| {
        value["active_desktop"] == 1
            && among_window(value).is_some_and(|window| window["focused"] == false)
    })?;
    ring(&mut session, "desktop")?;
    let shown = notification(&mut session, "desktop")?;
    scenarios.push(json!({"name":"other-desktop","notification":shown,"after":click(&mut session,"desktop",&shown)?}));
    release("desktop-clicked")?;

    wait_mark(&mut session, "hidden")?;
    let before_hidden = app_log()?;
    fs::write(
        Path::new(ROOT).join("hidden-before-app.log"),
        &before_hidden,
    )?;
    if !before_hidden
        .lines()
        .rev()
        .find(|line| line.contains("workspaces="))
        .is_some_and(|line| line.contains("visible=1 ") && line.contains("focused=PaneId(2)"))
    {
        return Err("hidden-sibling scenario did not prepare an expanded sibling".into());
    }
    minimise(&mut session, "hidden")?;
    ring(&mut session, "hidden")?;
    let shown = notification(&mut session, "hidden")?;
    let after = click(&mut session, "hidden", &shown)?;
    let hidden_log = app_log()?;
    fs::write(Path::new(ROOT).join("hidden-after-app.log"), &hidden_log)?;
    if !hidden_log
        .lines()
        .rev()
        .find(|line| line.contains("workspaces="))
        .is_some_and(|line| line.contains("visible=2 "))
    {
        return Err("notification focus did not restore the expanded layout".into());
    }
    scenarios.push(json!({"name":"hidden-sibling","notification":shown,"after":after}));
    let project_offset = app_log()?.len();
    release("hidden-clicked")?;

    wait_mark(&mut session, "project")?;
    wait_other_project(&mut session, project_offset)?;
    minimise(&mut session, "project")?;
    ring(&mut session, "project")?;
    let shown = notification(&mut session, "project")?;
    let after = click(&mut session, "project", &shown)?;
    scenarios.push(json!({"name":"other-project","notification":shown,"after":after}));
    release("project-clicked")?;

    wait_mark(&mut session, "closing-bell")?;
    minimise(&mut session, "closing")?;
    ring(&mut session, "closing-bell")?;
    let closing = notification(&mut session, "closing")?;
    let target = closing["notifications"][0]["target"].clone();
    wait_mark(&mut session, "closing")?;
    release("closing")?;
    wait_mark(&mut session, "closed")?;
    let closed = wait_snapshot(&mut session, "source-closed", |value| {
        value["notifications"].as_array().is_some_and(Vec::is_empty)
    })?;
    let state_before = app_log()?;
    // A removed banner cannot be clicked. Reproduce an action already captured
    // by the Shell, delivered during a new actual input event, so its launch
    // context carries that event's timestamp instead of requesting focus with 0.
    session.json("closed-target-event-armed", &format!(r#"(async () => {{
        const {{default: Shell}} = await import('gi://Shell');
        const {{default: GLib}} = await import('gi://GLib');
        const {{default: Clutter}} = await import('gi://Clutter');
        const {{default: St}} = await import('gi://St');
        const Main = await import('resource:///org/gnome/shell/ui/main.js');
        const target = {};
        global.ownedLateActivation = {{target, received:false, completed:false, error:null}};
        const app = Shell.AppSystem.get_default().lookup_app('com.github.among-terminal.desktop');
        const button = new St.Button({{name:'owned-late-action',label:'Dispatch captured notification action',
            style_class:'button',reactive:true,can_focus:false}});
        global.ownedLateButton = button;
        Main.layoutManager.addChrome(button,{{affectsInputRegion:true}});
        button.set_position(32,720);
        button.set_size(320,50);
        const handler = button.connect('button-release-event', (_button,event) => {{
            if (event.get_button() !== 1)
                return Clutter.EVENT_PROPAGATE;
            if (global.ownedLateActivation.received) return Clutter.EVENT_STOP;
            button.disconnect(handler);
            const time = event.get_time();
            Object.assign(global.ownedLateActivation,{{received:true,time,
                device:event.get_source_device()?.get_device_name() ?? null,
                event_type:'button-release',button:event.get_button()}});
            app.activate_action('focus-pane',new GLib.Variant('av',[
                new GLib.Variant('(sttt)',target)]),time,-1,null)
                .then(() => {{
                    global.ownedLateActivation.completed = true;
                    global.ownedLateButton = null;
                    button.destroy();
                }})
                .catch(error => {{global.ownedLateActivation.error = String(error);}});
            return Clutter.EVENT_STOP;
        }});
        return true;
    }})()"#, serde_json::to_string(&target)?))?;
    let gesture = wait_snapshot(&mut session, "closed-target-gesture", |value| {
        let control = &value["late_action_control"];
        control["mapped"] == true
            && control["opacity"] == 255
            && rectangle(&control["rect"]).is_ok()
    })?;
    session.screen("closed-target-gesture")?;
    let [x, y, width, height] = rectangle(&gesture["late_action_control"]["rect"])?;
    pointer_click(
        &mut session,
        "closed-target-gesture",
        x + width / 2.0,
        y + height / 2.0,
    )?;
    let raised = wait_snapshot(&mut session, "closed-target-raised", |value| {
        assert_app_focus(value) && value["late_activation"]["completed"] == true
    })?;
    if raised["late_activation"]["received"] != true
        || raised["late_activation"]["time"]
            .as_u64()
            .is_none_or(|time| time == 0)
        || !raised["late_activation"]["error"].is_null()
        || raised["late_activation"]["target"] != target
    {
        return Err("late action did not originate from the owned pointer event".into());
    }
    wait_pane_focus(state_before.len(), 2)?;
    let state_after = app_log()?;
    let focus_line = |log: &str| {
        log.lines()
            .rev()
            .find(|line| line.contains("workspaces="))
            .map(|line| line.contains("active=0 ") && line.contains("focused=PaneId(2)"))
    };
    if focus_line(&state_before) != Some(true) || focus_line(&state_after) != Some(true) {
        return Err("closed target changed the selected terminal".into());
    }
    scenarios.push(json!({"name":"closed-source-withdrawal-and-late-action","notification":closing,"closed":closed,"gesture":gesture,"raised":raised,"action_kind":"captured-target race dispatched from an actual click on the owned Shell test control after withdrawal, not a click on a removed banner"}));
    session.screen("closed-target-raised")?;
    let result = json!({"schema":1,"backend":mode.name(),"passed":true,"initial":ready,"startup_dismissals":startup_dismissals,"scenarios":scenarios,"notification_surface":"actual GNOME Shell banner; virtual pointer input","manual_physical_desktop":"not exercised"});
    fs::write(
        Path::new(ROOT).join("result.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    Ok(())
}

#[cfg(test)]
mod project_tests {
    use super::*;

    #[test]
    fn a_noop_workspace_action_or_stale_report_cannot_prove_another_project() {
        let original = "among[tick1]: workspaces=2 active=0 name=one focused=PaneId(1)\n";
        let project = "among[tick2]: workspaces=2 active=1 name=two focused=PaneId(3)\n";
        assert!(!other_project_active(original));
        assert!(other_project_active(&format!("{original}{project}")));
        assert!(!other_project_active(&format!("{project}{original}")));
        assert!(!other_project_active(project.trim_end()));
        let before = format!("{project}{original}");
        let after = format!("{before}{original}");
        assert!(!other_project_active(&after[before.len()..]));
    }
}
