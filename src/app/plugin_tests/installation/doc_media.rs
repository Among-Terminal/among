//! One scene for every documentation still and recording.
//!
//! The scene is built here, not in a shell script, so the workspaces, panes,
//! terminal content and the restore prompt are real application state. The
//! owned display runner supplies the private HOME, X server and window manager;
//! `among-tools media` prepares and checks the files after this process exits.
//!
//! Set `AMONG_MEDIA_OUTPUT` to an absolute directory. The test refuses to run
//! without an owned display and writes only below that directory.

use super::*;
use crate::plugin::install::listing::tests::{digest, document};
use crate::plugin::install::source::Inspection;
use crate::plugin::inventory::tests::{fixture_plugin, release_of, source_receipt, Identity};
use crate::plugin::origin::Origin;
use crate::session::{SavedDirectory, SavedNode, SavedOrientation, SavedWorkspace, Snapshot};
use crate::tree::PaneId;
use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where a still or a raw recording is written.
fn output() -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os("AMONG_MEDIA_OUTPUT").expect("AMONG_MEDIA_OUTPUT is required"),
    );
    assert!(path.is_absolute(), "AMONG_MEDIA_OUTPUT must be absolute");
    fs::create_dir_all(&path).expect("media output directory");
    path
}

fn shell() -> PathBuf {
    let path = PathBuf::from(std::env::var_os("HOME").expect("owned display HOME")).join("shell");
    fs::write(
        &path,
        b"#!/bin/sh\nexport PS1='\\W $ '\nexport HISTFILE=/dev/null\n\
          exec /bin/bash --noprofile --norc -i\n",
    )
    .expect("media shell");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("media shell mode");
    path
}

/// A deterministic project whose working tree has a reviewable diff.
fn project(_home: &Path) -> PathBuf {
    let project = PathBuf::from("/work/among");
    if project.exists() {
        fs::remove_dir_all(&project).expect("clean media project");
    }
    fs::create_dir_all(project.join("src")).expect("project source");
    fs::create_dir_all(project.join(".demo")).expect("demo scripts");
    fs::write(
        project.join("src/review.rs"),
        r#"pub fn review(changes: &[Change]) -> Report {
    let mut report = Report::default();
    for change in changes {
        report.checked += 1;
    }
    report
}
"#,
    )
    .expect("review source");
    fs::write(
        project.join("checklist.md"),
        r#"# Review checklist

- [x] Read the changed parser
- [x] Compare the public API
- [ ] Run the integration suite
- [ ] Update the changelog
- [ ] Ask for a second reviewer

The parser keeps the old shape; only its
error path changes.
"#,
    )
    .expect("checklist");
    fs::write(
        project.join("notes.md"),
        r#"# Notes

Keep the review small enough to finish
in one sitting. The next step is the
integration suite, not another refactor.
"#,
    )
    .expect("notes");
    fs::write(
        project.join("Makefile"),
        b"check:\n\t@printf 'cargo test --workspace\\n'\n\t@sleep 2\n\t@printf '712 passed\\n'\n",
    )
    .expect("Makefile");
    write_scripts(&project);
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "docs@among.invalid"],
        vec!["config", "user.name", "Among Documentation"],
        vec!["add", "."],
        vec!["commit", "-qm", "Seed the review workspace"],
    ] {
        let status = Command::new("git")
            .current_dir(&project)
            .args(args)
            .status()
            .expect("git fixture");
        assert!(status.success(), "git fixture command failed");
    }
    fs::write(
        project.join("src/review.rs"),
        r#"pub fn review(changes: &[Change]) -> Report {
    let mut report = Report::default();
    for change in changes {
        report.checked += 1;
        if change.needs_attention() {
            report.attention.push(change.id());
        }
    }
    report
}

fn summary(report: &Report) -> String {
    format!(
        "{} checked, {} need attention",
        report.checked,
        report.attention.len()
    )
}
"#,
    )
    .expect("changed source");
    fs::write(
        project.join("checklist.md"),
        r#"# Review checklist

- [x] Read the changed parser
- [x] Compare the public API
- [x] Read the patch notes
- [ ] Run the integration suite
- [ ] Update the changelog
- [ ] Ask for a second reviewer

The parser keeps the old shape; only its
error path changes.
"#,
    )
    .expect("changed checklist");
    project
}

fn script(project: &Path, name: &str, body: &str) {
    let path = project.join(".demo").join(name);
    fs::write(
        &path,
        format!("#!/bin/sh\nprintf '\\033[2J\\033[H'\n{body}\n"),
    )
    .expect("demo script");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("demo script mode");
}

fn write_scripts(project: &Path) {
    script(
        project,
        "diff.sh",
        "printf '\\033]0;git diff\\007'\n\
         cd \"/work/among\"\n\
         git --no-pager diff --color=always -- src/review.rs | head -32",
    );
    script(
        project,
        "checklist.sh",
        "printf '\\033]0;checklist\\007'\n\
         cd \"/work/among\"\n\
         awk '{ if ($0 ~ /^- \\[x\\]/) printf \"\\033[32m%s\\033[0m\\n\", $0; else if ($0 ~ /^- \\[ \\]/) printf \"\\033[33m%s\\033[0m\\n\", $0; else print }' checklist.md",
    );
    script(
        project,
        "changes.sh",
        "printf '\\033]0;changes\\007'\n\
         cd \"/work/among\"\n\
         git --no-pager -c color.ui=always diff --stat\n\
         printf '\\n'\n\
         git --no-pager log --oneline --decorate --color=always -5",
    );
    script(
        project,
        "make.sh",
        "printf '\\033]0;make\\007'\n\
         cd \"/work/among\"\n\
         printf 'cargo test --workspace\\n'\n\
         i=1\n\
         while [ $i -le 40 ]; do\n\
           printf 'test parser_case_%02d ... ok\\n' \"$i\"\n\
           i=$((i+1))\n\
         done\n\
         printf '\\n712 passed\\n'",
    );
    script(
        project,
        "status.sh",
        "printf '\\033]0;status\\007'\n\
         cd \"/work/among\"\n\
         git status --short",
    );
    script(
        project,
        "notes.sh",
        "printf '\\033]0;notes\\007'\n\
         cd \"/work/among\"\n\
         cat notes.md",
    );
    script(
        project,
        "bell.sh",
        "printf '\\033]0;make\\007'\n\
         cd \"/work/among\"\n\
         printf 'cargo test --workspace\\n'\n\
         i=1\n\
         while [ $i -le 40 ]; do\n\
           printf 'test parser_case_%02d ... ok\\n' \"$i\"\n\
           i=$((i+1))\n\
         done\n\
         printf 'media-bell-ready\\n'\n\
         sleep 2\n\
         printf '\\a'\n\
         printf 'test parser_case_41 ... ok\\n'",
    );
    script(
        project,
        "bell-status.sh",
        "printf '\\033]0;status\\007'\n\
         cd \"/work/among\"\n\
         git status --short\n\
         sleep 2\n\
         printf '\\a'",
    );
}

fn pane(cwd: &Path, id: u64) -> SavedDirectory {
    SavedDirectory {
        pane: id,
        cwd: Some(cwd.display().to_string()),
    }
}

fn split(
    orientation: SavedOrientation,
    ratio: f64,
    first: SavedNode,
    second: SavedNode,
) -> SavedNode {
    SavedNode::Split {
        orientation,
        ratio,
        first: Box::new(first),
        second: Box::new(second),
    }
}

fn snapshot(cwd: &Path) -> Snapshot {
    Snapshot {
        version: 1,
        active: 0,
        workspaces: vec![
            SavedWorkspace {
                name: "Workbench".into(),
                colour: 0,
                focused: 1,
                layout: split(
                    SavedOrientation::Vertical,
                    0.56,
                    SavedNode::Pane { id: 1 },
                    split(
                        SavedOrientation::Horizontal,
                        0.50,
                        SavedNode::Pane { id: 2 },
                        SavedNode::Pane { id: 3 },
                    ),
                ),
                directories: vec![pane(cwd, 1), pane(cwd, 2), pane(cwd, 3)],
            },
            SavedWorkspace {
                name: "Review".into(),
                colour: 1,
                focused: 4,
                layout: split(
                    SavedOrientation::Vertical,
                    0.50,
                    SavedNode::Pane { id: 4 },
                    SavedNode::Pane { id: 5 },
                ),
                directories: vec![pane(cwd, 4), pane(cwd, 5)],
            },
            SavedWorkspace {
                name: "Notes".into(),
                colour: 2,
                focused: 6,
                layout: SavedNode::Pane { id: 6 },
                directories: vec![pane(cwd, 6)],
            },
        ],
    }
}

fn write_snapshot(cwd: &Path) {
    let path = crate::session::path().expect("session path");
    fs::create_dir_all(path.parent().expect("session parent")).expect("session directory");
    fs::write(
        &path,
        serde_json::to_vec(&snapshot(cwd)).expect("session json"),
    )
    .expect("session file");
}

fn write_appearance() {
    let config = PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").expect("owned display config"))
        .join("among");
    fs::create_dir_all(&config).expect("preferences directory");
    fs::write(
        config.join("terminal.json"),
        br#"{"version":1,"text_background":"gray-on-black","ansi_palette":"tango"}"#,
    )
    .expect("terminal appearance");
    let gtk = PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").expect("owned display config"))
        .join("gtk-4.0");
    fs::create_dir_all(&gtk).expect("GTK configuration directory");
    fs::write(
        gtk.join("settings.ini"),
        b"[Settings]\ngtk-monospace-font-name=Monospace 10\n",
    )
    .expect("GTK font settings");
}

const OFFICIAL: (u64, &str, &str) = (1_431_055_733, "Among-Terminal", "among-plugins");
const OFFICIAL_PUBLISHER: u64 = 339_567_100;

fn described(source: Origin, plugins: &[(&str, &str, &str, &str)]) -> Inspection {
    let specs: Vec<_> = plugins
        .iter()
        .map(|(id, name, version, _)| (*id, *name, *version, digest('b')))
        .collect();
    let specs: Vec<_> = specs
        .iter()
        .map(|(id, name, version, sum)| (*id, *name, *version, sum.as_str()))
        .collect();
    let mut catalogue = document(&specs);
    for (index, (_, _, _, description)) in plugins.iter().enumerate() {
        let version = &mut catalogue["plugins"][index]["versions"][0];
        version["manifest"]["description"] = json!(description);
        version["artifacts"][0]["size"] = json!(262_144);
    }
    Inspection::from_bytes(source, "a".repeat(40), catalogue.to_string().into_bytes())
        .expect("fixture catalogue")
}

fn prepare_plugin_fixtures() {
    let root = crate::plugin::inventory::root().expect("plugin inventory root");
    crate::test_isolation::assert_below_check_root(&root);
    let clock = fixture_plugin(
        &root,
        Identity {
            id: "clock",
            name: "Clock",
            description: "Local time in the header",
            repository: OFFICIAL,
        },
        "0.2.0",
    );
    let status = fixture_plugin(
        &root,
        Identity {
            id: "system-status",
            name: "System status",
            description: "CPU, memory and disk bars",
            repository: OFFICIAL,
        },
        "0.2.0",
    );
    let published = |mut receipt| {
        let mut source = source_receipt(&receipt);
        source.publisher_id = OFFICIAL_PUBLISHER;
        release_of(&mut receipt).source = Some(source);
        receipt
    };
    fs::write(
        root.join("installed.json"),
        json!({"version":1,"installations":[published(clock), published(status)]}).to_string(),
    )
    .expect("installed fixtures");
    let sources = crate::plugin::install::registry::root().expect("plugin source root");
    let official = super::list::origin(OFFICIAL.0, OFFICIAL_PUBLISHER, OFFICIAL.1, OFFICIAL.2);
    super::list::save_source(
        &sources,
        described(
            official,
            &[
                ("clock", "Clock", "0.2.0", "Local time in the header"),
                (
                    "system-status",
                    "System status",
                    "0.2.0",
                    "CPU, memory and disk bars",
                ),
                (
                    "pane-pwd",
                    "Pane directory",
                    "0.2.0",
                    "Working directory per pane",
                ),
            ],
        ),
    );
    let third_party = super::list::origin(77, 34, "example", "among-plugins");
    super::list::save_source(
        &sources,
        described(
            third_party,
            &[("notes", "Notes", "0.4.0", "A small note pad")],
        ),
    );
}

fn focus_main(window: &impl IsA<gtk::Window>) -> String {
    wait_for(|| window.as_ref().is_mapped(), Duration::from_secs(3));
    let name = format!("^{}$", window.as_ref().title().expect("window title"));
    let ids = xdo(&[
        "search",
        "--all",
        "--onlyvisible",
        "--pid",
        &std::process::id().to_string(),
        "--name",
        &name,
    ]);
    assert_eq!(ids.lines().count(), 1, "exactly one visible owned window");
    let id = ids.lines().last().expect("window id").to_string();
    xdo(&["windowactivate", "--sync", &id]);
    xdo(&["windowfocus", "--sync", &id]);
    wait_for(|| window.as_ref().is_active(), Duration::from_secs(2));
    id
}

fn place(window: &impl IsA<gtk::Window>, width: u32, height: u32, x: i32, y: i32) -> String {
    window.as_ref().unmaximize();
    wait_for(|| !window.as_ref().is_maximized(), Duration::from_secs(3));
    let id = focus_main(window);
    window
        .as_ref()
        .set_size_request(width as i32, height as i32);
    window
        .as_ref()
        .set_default_size(width as i32, height as i32);
    let end = std::time::Instant::now() + Duration::from_millis(300);
    while std::time::Instant::now() < end {
        glib::MainContext::default().iteration(false);
        std::thread::sleep(Duration::from_millis(5));
    }
    xdo(&[
        "windowsize",
        "--sync",
        &id,
        &width.to_string(),
        &height.to_string(),
    ]);
    xdo(&["windowmove", "--sync", &id, &x.to_string(), &y.to_string()]);
    eprintln!(
        "media window requested {width}x{height} at {x},{y}; actual {}x{}; {}",
        window.as_ref().width(),
        window.as_ref().height(),
        xdo(&["getwindowgeometry", &id]).trim()
    );
    wait_for(
        || {
            (window.as_ref().width() - width as i32).abs() <= 10
                && (window.as_ref().height() - height as i32).abs() <= 10
        },
        Duration::from_secs(3),
    );
    id
}

/// Runs the application's main context while time passes. A plain thread
/// sleep would freeze VTE writes and the pulse animation that the recording
/// is meant to show.
fn pump_for(duration: Duration) {
    let end = std::time::Instant::now() + duration;
    while std::time::Instant::now() < end {
        while glib::MainContext::default().pending() {
            glib::MainContext::default().iteration(false);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn capture_root(name: &str) {
    settle();
    let path = output().join(format!("{name}.png"));
    let status = Command::new("import")
        .args(["-window", "root"])
        .arg(&path)
        .status()
        .expect("ImageMagick import");
    assert!(status.success(), "capture {name} failed");
    eprintln!("media still: {}", path.display());
}

fn capture_window(name: &str, window: &impl IsA<gtk::Window>) {
    settle();
    let id = focus(window);
    let path = output().join(format!("{name}.png"));
    let status = Command::new("import")
        .args(["-screen", "-window", &id])
        .arg(&path)
        .status()
        .expect("ImageMagick import");
    assert!(status.success(), "capture {name} failed");
    eprintln!("media still: {}", path.display());
}

fn row_titled(preferences: &adw::PreferencesWindow, title: &str) -> adw::ActionRow {
    wait_for(
        || {
            widgets(preferences.upcast_ref())
                .iter()
                .any(|widget| widget.is::<adw::ActionRow>() && widget.is_mapped())
        },
        Duration::from_secs(3),
    );
    widgets(preferences.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
        .find(|row| row.title() == title && row.is_mapped())
        .unwrap_or_else(|| panic!("no shown row titled {title}"))
}

fn show_tab(preferences: &adw::PreferencesWindow, title: &str) {
    let page = widgets(preferences.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::PreferencesPage>().ok())
        .find(|page| page.title() == title)
        .unwrap_or_else(|| panic!("no tab {title}"));
    preferences.set_visible_page(&page);
    wait_for(|| page.is_mapped(), Duration::from_secs(3));
}

fn choose(preferences: &adw::PreferencesWindow, name: &str, label: &str) {
    let row = widgets(preferences.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::ComboRow>().ok())
        .find(|row| row.widget_name() == name)
        .unwrap_or_else(|| panic!("no dropdown named {name}"));
    let model = row
        .model()
        .and_downcast::<gtk::StringList>()
        .expect("a string model");
    let position = (0..model.n_items())
        .find(|index| model.string(*index).as_deref() == Some(label))
        .unwrap_or_else(|| panic!("{name} has no entry {label}"));
    row.set_selected(position);
    assert_eq!(row.selected(), position);
}

fn resize_preferences(preferences: &adw::PreferencesWindow, width: u32, height: u32) -> String {
    let id = focus(preferences);
    preferences.set_default_size(width as i32 + 10, height as i32 + 10);
    xdo(&[
        "windowsize",
        "--sync",
        &id,
        &(width + 10).to_string(),
        &(height + 10).to_string(),
    ]);
    wait_for(
        || {
            preferences.width() >= width as i32 - 5
                && preferences.width() <= width as i32 + 10
                && preferences.height() >= height as i32 - 5
                && preferences.height() <= height as i32 + 10
        },
        Duration::from_secs(3),
    );
    eprintln!(
        "media preferences {}x{} requested {width}x{height}",
        preferences.width(),
        preferences.height()
    );
    id
}

fn back(preferences: &adw::PreferencesWindow) {
    focus(preferences);
    xdo(&["key", "--clearmodifiers", "Escape"]);
    settle();
}

fn capture_preferences(ctx: &Window) {
    open_preferences(ctx);
    let preferences = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
        .find(|window| window.title().as_deref() == Some("Preferences"))
        .expect("Preferences window");
    wait_for(|| preferences.is_mapped(), Duration::from_secs(3));
    settle();

    resize_preferences(&preferences, 880, 640);
    assert!(has_label(preferences.upcast_ref(), "Installed"));
    assert!(has_label(preferences.upcast_ref(), "Available"));
    assert!(has_label(preferences.upcast_ref(), "Pane directory"));
    capture_window("plugins", &preferences);

    press(&named_button(
        &row_titled(&preferences, "System status"),
        "plugin-info",
    ));
    wait_for(
        || has_label(preferences.upcast_ref(), "Can read"),
        Duration::from_secs(3),
    );
    resize_preferences(&preferences, 880, 720);
    capture_window("plugin-info", &preferences);
    back(&preferences);

    resize_preferences(&preferences, 880, 640);
    let clock_row = row_titled(&preferences, "Clock");
    let switch = widgets(clock_row.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::Switch>().ok())
        .expect("Clock switch");
    focus(&preferences);
    assert!(switch.is_sensitive() && switch.grab_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
    wait_for(
        || preferences.visible_dialog().is_some(),
        Duration::from_secs(3),
    );
    let dialog = preferences.visible_dialog().expect("access review");
    assert!(has_label(dialog.upcast_ref(), "Review plugin access"));
    resize_preferences(&preferences, 880, 720);
    capture_window("plugin-access-review", &preferences);
    press(&button(&dialog, "Cancel"));
    settle();

    resize_preferences(&preferences, 880, 900);
    show_tab(&preferences, "Notifications");
    assert!(has_label(preferences.upcast_ref(), "Command"));
    capture_window("notifications", &preferences);

    resize_preferences(&preferences, 880, 820);
    show_tab(&preferences, "Keyboard");
    capture_window("keyboard", &preferences);

    resize_preferences(&preferences, 880, 420);
    show_tab(&preferences, "Terminal");
    choose(&preferences, "terminal-text-background", "Gray on black");
    choose(&preferences, "terminal-ansi-palette", "Tango");
    capture_window("terminal", &preferences);

    preferences.destroy();
}

fn terminal(ctx: &Window, id: u64) -> vte::Terminal {
    ctx.state
        .borrow()
        .pane(PaneId(id))
        .expect("scene pane")
        .terminal()
        .clone()
}

fn feed(ctx: &Window, id: u64, command: &str) {
    let mut bytes = command.as_bytes().to_vec();
    bytes.push(b'\r');
    terminal(ctx, id).feed_child(&bytes);
}

fn wait_for_text(ctx: &Window, id: u64, needle: &str) {
    wait_for(
        || {
            terminal(ctx, id)
                .text_format(vte::Format::Text)
                .is_some_and(|text| text.contains(needle))
        },
        Duration::from_secs(10),
    );
}

/// Stops the PTY echoing the command line and clears the first prompt, so a
/// still shows the command's output instead of the script invocation.
fn quiet(ctx: &Window, id: u64) {
    wait_for(
        || {
            terminal(ctx, id)
                .text_format(vte::Format::Text)
                .is_some_and(|text| text.contains('$'))
        },
        Duration::from_secs(10),
    );
    feed(ctx, id, "stty -echo; printf '\\033[2J\\033[H'");
}

fn quiet_all(ctx: &Window) {
    for id in 1..=6 {
        quiet(ctx, id);
    }
}

fn fill_workbench(ctx: &Window) {
    feed(ctx, 1, "bash .demo/diff.sh");
    feed(ctx, 2, "bash .demo/checklist.sh");
    feed(ctx, 3, "bash .demo/changes.sh");
    wait_for_text(ctx, 1, "needs_attention");
    wait_for_text(ctx, 2, "integration suite");
    wait_for_text(ctx, 3, "src/review.rs");
}

fn fill_other_workspaces(ctx: &Window) {
    feed(ctx, 4, "bash .demo/make.sh");
    feed(ctx, 5, "bash .demo/status.sh");
    feed(ctx, 6, "bash .demo/notes.sh");
}

#[test]
#[ignore = "requires the owned media display, xdotool and ImageMagick"]
fn capture_stills() {
    let home = PathBuf::from(std::env::var_os("HOME").expect("owned display HOME"));
    let work = project(&home);
    write_snapshot(&work);
    write_appearance();
    prepare_plugin_fixtures();
    let _shell = shell();
    let _service = crate::app::desktop_notification_tests::Server::new();
    std::env::set_var("AMONG_COLOR_SCHEME", "dark");

    let app = application("com.github.among-terminal.doc-media-stills");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).expect("application");
    wait_for(
        || ctx.window.visible_dialog().is_some(),
        Duration::from_secs(5),
    );
    place(&ctx.window, 1440, 900, 80, 50);
    settle();
    capture_root("session-restore");
    let dialog = ctx.window.visible_dialog().expect("restore prompt");
    press(&button(&dialog, "Restore"));
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    place(&ctx.window, 1440, 900, 80, 50);
    quiet_all(&ctx);
    fill_workbench(&ctx);
    fill_other_workspaces(&ctx);
    settle();
    capture_root("main-window");

    xdo(&["key", "--clearmodifiers", "ctrl+shift+x"]);
    settle();
    capture_root("expanded-pane");
    xdo(&["key", "--clearmodifiers", "ctrl+shift+x"]);
    settle();
    xdo(&["key", "--clearmodifiers", "alt+2"]);
    settle();
    capture_root("workspaces");
    xdo(&["key", "--clearmodifiers", "alt+1"]);
    capture_preferences(&ctx);
    ctx.window.destroy();
}

fn set_banners(show: bool) {
    let status = Command::new("gsettings")
        .args([
            "set",
            "org.gnome.desktop.notifications",
            "show-banners",
            if show { "true" } else { "false" },
        ])
        .status()
        .expect("gsettings");
    assert!(status.success(), "gsettings show-banners failed");
}

fn enable_banners() {
    set_banners(true);
}

fn install_application_identity(id: &str) {
    let data = PathBuf::from(std::env::var_os("XDG_DATA_HOME").expect("owned display data"));
    let applications = data.join("applications");
    fs::create_dir_all(&applications).expect("application directory");
    fs::write(
        applications.join(format!("{id}.desktop")),
        "[Desktop Entry]\nType=Application\nName=Among Terminal\n\
         Exec=/bin/true\nIcon=com.github.among-terminal\nDBusActivatable=true\n",
    )
    .expect("desktop identity");
    let icons = data.join("icons/hicolor/scalable/apps");
    fs::create_dir_all(&icons).expect("icon directory");
    fs::write(
        icons.join("com.github.among-terminal.svg"),
        include_bytes!("../../../../data/com.github.among-terminal.svg"),
    )
    .expect("icon");
}

fn search_windows(name: &str) -> Vec<String> {
    let output = Command::new("xdotool")
        .args(["search", "--all", "--name", name])
        .output()
        .expect("xdotool search");
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// A 1x1 client that takes keyboard focus without covering the maximized
/// application: a window manager has no "focus the desktop" request, and the
/// desktop notification policy is about the application being in background.
fn focus_neutral_window() -> std::process::Child {
    let child = Command::new("xmessage")
        .args([
            "-name",
            "among-media-neutral",
            "-geometry",
            "10x10+1590+1190",
            "-borderwidth",
            "0",
            "-bg",
            "#20262f",
            "-fg",
            "#20262f",
            "",
        ])
        .spawn()
        .expect("neutral window");
    wait_for(
        || !search_windows("among-media-neutral").is_empty(),
        Duration::from_secs(3),
    );
    let ids = search_windows("among-media-neutral");
    let id = ids.last().expect("neutral window id");
    xdo(&["windowactivate", "--sync", id]);
    xdo(&["windowfocus", "--sync", id]);
    child
}

fn unique_application(id: &str) -> adw::Application {
    let config = std::env::var_os("XDG_CONFIG_HOME").expect("owned display config");
    crate::test_isolation::assert_below_check_root(std::path::Path::new(&config));
    std::env::set_var("SHELL", "/bin/sh");
    let app = adw::Application::builder()
        .application_id(id)
        .flags(gio::ApplicationFlags::empty())
        .build();
    app.register(None::<&gio::Cancellable>)
        .expect("application");
    app
}

fn start_recording(path: &Path) -> std::process::Child {
    let display = std::env::var("DISPLAY").expect("display");
    let input = format!("{display}.0+0,0");
    let log = std::fs::File::create(path.with_extension("ffmpeg.log")).expect("ffmpeg log");
    Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "x11grab",
            "-draw_mouse",
            "1",
            "-video_size",
            "1600x1000",
            "-framerate",
            "30",
            "-i",
            &input,
            "-c:v",
            "libx264",
            "-preset",
            "medium",
            "-crf",
            "20",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
        ])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(log)
        .spawn()
        .expect("ffmpeg recording")
}

fn stop_recording(mut child: std::process::Child) {
    unsafe {
        libc::kill(child.id() as i32, libc::SIGINT);
    }
    let status = child.wait().expect("ffmpeg wait");
    assert!(
        status.success() || status.code() == Some(255),
        "ffmpeg recording failed: {status}"
    );
}

#[test]
#[ignore = "requires the owned GNOME media display, xdotool, ffmpeg and ImageMagick"]
fn capture_notification() {
    let home = PathBuf::from(std::env::var_os("HOME").expect("owned display HOME"));
    let work = project(&home);
    write_snapshot(&work);
    write_appearance();
    install_application_identity("com.github.among-terminal");
    std::env::set_var("AMONG_COLOR_SCHEME", "dark");

    let app = unique_application("com.github.among-terminal");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).expect("application");
    wait_for(
        || ctx.window.visible_dialog().is_some(),
        Duration::from_secs(5),
    );
    let dialog = ctx.window.visible_dialog().expect("restore prompt");
    press(&button(&dialog, "Restore"));
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    wait_for(|| ctx.window.is_maximized(), Duration::from_secs(3));

    // GNOME startup notices are kept out of the scene: banners are disabled
    // while the shell's own notices are created, then enabled for the bell.
    set_banners(false);
    pump_for(Duration::from_secs(5));
    enable_banners();
    pump_for(Duration::from_millis(500));

    xdo(&["key", "--clearmodifiers", "alt+2"]);
    settle();
    quiet(&ctx, 4);
    quiet(&ctx, 5);
    feed(&ctx, 4, "bash .demo/make.sh");
    wait_for_text(&ctx, 4, "712 passed");
    feed(&ctx, 5, "bash .demo/status.sh");
    wait_for_text(&ctx, 5, "review.rs");

    let recording = output().join("notification-return.mp4");
    let recorder = start_recording(&recording);
    pump_for(Duration::from_millis(500));
    let mut neutral = focus_neutral_window();
    wait_for(|| !ctx.window.is_active(), Duration::from_secs(2));
    terminal(&ctx, 5).feed_child(b"printf '\\a'\n");
    wait_for(
        || {
            ctx.state
                .borrow()
                .bells
                .as_ref()
                .is_some_and(|bells| bells.attention(5).unread)
        },
        Duration::from_secs(3),
    );

    pump_for(Duration::from_millis(2200));
    // The pointer gesture is visible in the recording. GNOME's X11 stage does
    // not deliver synthetic clicks to the banner in this owned display, so the
    // same action the banner's default target names is invoked next. The
    // payload and activation contract are checked separately by
    // check-desktop-notifications.
    xdo(&["mousemove", "--sync", "716", "88"]);
    pump_for(Duration::from_millis(200));
    xdo(&["click", "1"]);
    let target = ctx
        .state
        .borrow()
        .bells
        .as_ref()
        .expect("bell backend")
        .desktop_target(5)
        .expect("notification target");
    ctx.window
        .application()
        .expect("application")
        .activate_action("focus-pane", Some(&target));
    pump_for(Duration::from_millis(2200));
    assert_eq!(ctx.state.borrow().workspaces.active_index(), 1);
    assert!(ctx.window.is_active());
    stop_recording(recorder);
    let _ = neutral.kill();
    let _ = neutral.wait();
    ctx.window.destroy();
}
