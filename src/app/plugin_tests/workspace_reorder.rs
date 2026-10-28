use super::*;
use std::path::Path;

const KEY: &str = "github:12/sample";

fn references(ctx: &Window) -> Vec<u64> {
    ctx.state
        .borrow()
        .workspaces
        .workspaces()
        .iter()
        .map(|workspace| workspace.reference)
        .collect()
}

fn point(ctx: &Window, index: i32, fraction: f64) -> (i32, i32) {
    let row = ctx.workspace_list.row_at_index(index).unwrap();
    let bounds = row.compute_bounds(&ctx.window).unwrap();
    let (x, y) = ctx.window.surface_transform();
    (
        (x + f64::from(bounds.x()) + f64::from(bounds.width()) * 0.4) as i32,
        (y + f64::from(bounds.y()) + f64::from(bounds.height()) * fraction) as i32,
    )
}

fn move_pointer(window: &str, point: (i32, i32)) {
    xdo(&[
        "mousemove",
        "--window",
        window,
        &point.0.to_string(),
        &point.1.to_string(),
    ]);
}

fn pump_for(duration: Duration) {
    let until = std::time::Instant::now() + duration;
    while std::time::Instant::now() < until {
        while glib::MainContext::default().pending() {
            glib::MainContext::default().iteration(false);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn begin(ctx: &Window, window: &str, index: i32) {
    wait_for(
        || {
            ctx.workspace_list
                .row_at_index(index)
                .is_some_and(|row| row.width() > 32 && row.height() > 16)
        },
        Duration::from_secs(3),
    );
    let start = point(ctx, index, 0.5);
    move_pointer(window, start);
    pump_for(Duration::from_millis(50));
    xdo(&["mousedown", "1"]);
    pump_for(Duration::from_millis(50));
    for offset in [5, 12, 24] {
        move_pointer(window, (start.0 + offset, start.1));
        pump_for(Duration::from_millis(50));
    }
    wait_for(
        || ctx.state.borrow().workspace_drag.is_some(),
        Duration::from_secs(3),
    );
}

fn hover(ctx: &Window, window: &str, index: i32, after: bool) {
    move_pointer(window, point(ctx, index, if after { 0.75 } else { 0.25 }));
    let gap = index as usize + usize::from(after);
    wait_for(
        || {
            ctx.state
                .borrow()
                .workspace_drag
                .as_ref()
                .is_some_and(|drag| drag.target == Some(gap))
        },
        Duration::from_secs(3),
    );
}

fn release(ctx: &Window) {
    xdo(&["mouseup", "1"]);
    wait_for(
        || ctx.state.borrow().workspace_drag.is_none(),
        Duration::from_secs(3),
    );
}

fn summary(ctx: &Window, index: usize, expected: &str) -> bool {
    ctx.state
        .borrow()
        .store
        .summaries(index)
        .iter()
        .any(|summary| summary.source == KEY && summary.text.as_deref() == Some(expected))
}

fn fresh_summaries(ctx: &Window) {
    ctx.state.borrow_mut().store.remove_source(KEY);
    ctx.summary_requested.borrow_mut().clear();
    wait_for(
        || {
            references(ctx)
                .iter()
                .enumerate()
                .all(|(index, reference)| summary(ctx, index, &format!("workspace-{reference}")))
        },
        Duration::from_secs(4),
    );
}

fn configure_fixture(root: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let mut receipt = crate::plugin::inventory::tests::fixture(root);
    let package = receipt.package_path(root);
    std::fs::copy(&fixture("managed-echo")[0], package.join("plugin")).unwrap();
    std::fs::set_permissions(
        package.join("plugin"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    std::fs::write(package.join("among-fake-mode"), "managed-echo").unwrap();
    std::fs::write(package.join("target-labels"), b"yes").unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(package.join("among-plugin.json")).unwrap()).unwrap();
    manifest["required_features"] = serde_json::json!(["status", "settings", "context.terminals"]);
    manifest["optional_features"] = serde_json::json!([]);
    let bytes = manifest.to_string();
    std::fs::write(package.join("among-plugin.json"), &bytes).unwrap();
    receipt.manifest_sha256 = crate::plugin::inventory::checksum(bytes.as_bytes()).unwrap();
    std::fs::write(
        root.join("installed.json"),
        serde_json::json!({"version":1,"installations":[receipt]}).to_string(),
    )
    .unwrap();
    package
}

fn restore_button(widget: &gtk::Widget) -> Option<gtk::Button> {
    if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        if button.label().as_deref() == Some("Restore") {
            return Some(button.clone());
        }
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        if let Some(button) = restore_button(&widget) {
            return Some(button);
        }
        child = widget.next_sibling();
    }
    None
}

fn restored_plugin_drag_session(app: &adw::Application) {
    let ctx = build_with_writer(app, None, crate::session::Writer::new).unwrap();
    wait_for(
        || restore_button(ctx.window.upcast_ref()).is_some_and(|button| button.width() > 0),
        Duration::from_secs(3),
    );
    let window = xdo(&[
        "search",
        "--onlyvisible",
        "--pid",
        &std::process::id().to_string(),
        "--name",
        &ctx.window.title().unwrap(),
    ])
    .lines()
    .last()
    .unwrap()
    .to_string();
    xdo(&["windowactivate", "--sync", &window]);
    xdo(&["windowfocus", "--sync", &window]);
    wait_for(|| ctx.window.is_active(), Duration::from_secs(3));
    let button = restore_button(ctx.window.upcast_ref()).unwrap();
    assert!(button.grab_focus());
    pump_for(Duration::from_millis(50));
    xdo(&["key", "Return"]);
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    assert_eq!(references(&ctx), [3, 2]);
    assert_eq!(ctx.state.borrow().workspaces.active().reference, 3);
    ctx.state.borrow_mut().plugins.review_access(KEY, serde_json::json!({"owner":"example","repository":"sample","features":["status","settings","context.terminals"]})).unwrap();
    ctx.state.borrow_mut().plugins.enable(KEY);
    fresh_summaries(&ctx);
    xdo(&["key", "alt+2"]);
    wait_for(
        || ctx.state.borrow().workspaces.active().reference == 2,
        Duration::from_secs(2),
    );
    let proof = crate::settings::directory()
        .unwrap()
        .join("composed-restored-input");
    std::fs::create_dir_all(proof.parent().unwrap()).unwrap();
    xdo(&[
        "type",
        "--clearmodifiers",
        &format!("printf restored > '{}'", proof.display()),
    ]);
    xdo(&["key", "Return"]);
    wait_for(|| proof.is_file(), Duration::from_secs(3));
    assert_eq!(std::fs::read_to_string(proof).unwrap(), "restored");
    fresh_summaries(&ctx);
    ctx.state.borrow_mut().plugins.shutdown();
    wait_for(
        || ctx.state.borrow().plugins.is_stopped(),
        Duration::from_secs(5),
    );
    ctx.state.borrow_mut().exiting = true;
    ctx.window.destroy();
    println!("PASS the late-plugin/drop checkpoint restores in a fresh process through real Restore and Alt+2, with correctly owned fresh summaries and actual terminal input");
}

#[test]
#[ignore = "requires an owned display, built managed fake-plugin and real pointer events"]
fn real_plugin_updates_and_structural_changes_do_not_retarget_a_workspace_drag() {
    let app = application("com.github.among-terminal.workspace-reorder-races");
    if std::env::var_os("AMONG_REORDER_RESTORE_PHASE").is_some() {
        restored_plugin_drag_session(&app);
        return;
    }
    let root = crate::plugin::inventory::root().unwrap();
    crate::test_isolation::assert_below_check_root(&root);
    let package = configure_fixture(&root);
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    new_workspace(&ctx);
    new_workspace(&ctx);
    let window = xdo(&[
        "search",
        "--onlyvisible",
        "--pid",
        &std::process::id().to_string(),
        "--name",
        &ctx.window.title().unwrap(),
    ])
    .lines()
    .last()
    .unwrap()
    .to_string();
    xdo(&["windowactivate", &window]);
    xdo(&["windowfocus", &window]);
    wait_for(|| ctx.window.is_active(), Duration::from_secs(3));

    ctx.state.borrow_mut().plugins.review_access(KEY, serde_json::json!({"owner":"example","repository":"sample","features":["status","settings","context.terminals"]})).unwrap();
    ctx.state.borrow_mut().plugins.enable(KEY);
    fresh_summaries(&ctx);
    let pid = std::fs::read_to_string(package.join("executed")).unwrap();
    let generation = ctx.state.borrow().plugins.running_generation(KEY).unwrap();

    std::fs::write(package.join("hold-summary"), b"1").unwrap();
    ctx.summary_requested.borrow_mut().clear();
    wait_for(
        || package.join("status-held").exists(),
        Duration::from_secs(3),
    );
    begin(&ctx, &window, 0);
    hover(&ctx, &window, 2, true);
    let source_row = ctx.workspace_list.row_at_index(0).unwrap();
    std::fs::write(package.join("allow-status"), b"yes").unwrap();
    wait_for(
        || summary(&ctx, 0, "retired-target-marker"),
        Duration::from_secs(3),
    );
    assert_eq!(ctx.workspace_list.row_at_index(0).unwrap(), source_row);
    assert!(ctx.state.borrow().workspace_drag.is_some());
    release(&ctx);
    wait_for(|| references(&ctx) == [2, 3, 1], Duration::from_secs(3));
    assert!(summary(&ctx, 2, "retired-target-marker"));
    assert!(!summary(&ctx, 0, "retired-target-marker"));
    println!("PASS a real managed reply updates its cache during dragging without replacing the source row");

    std::fs::remove_file(package.join("hold-summary")).unwrap();
    std::fs::remove_file(package.join("allow-status")).unwrap();
    std::fs::remove_file(package.join("status-held")).unwrap();
    std::fs::remove_file(package.join("summary-released")).unwrap();
    fresh_summaries(&ctx);
    std::fs::write(package.join("hold-summary"), b"1").unwrap();
    ctx.summary_requested.borrow_mut().clear();
    wait_for(
        || package.join("status-held").exists(),
        Duration::from_secs(3),
    );
    begin(&ctx, &window, 2);
    hover(&ctx, &window, 0, false);
    release(&ctx);
    wait_for(|| references(&ctx) == [1, 2, 3], Duration::from_secs(3));
    let held_request = std::fs::read_to_string(package.join("transcript.jsonl"))
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .rfind(|frame| {
            frame["type"] == "status"
                && frame["scope"]["slot"] == "workspace_summary"
                && frame["scope"]["target"]["reference"] == "1"
        })
        .unwrap()["request_id"]
        .as_u64()
        .unwrap();
    assert!(ctx.state.borrow().plugins.status_pending(KEY, held_request));
    let stale_seen = Rc::new(Cell::new(false));
    let observation = glib::timeout_add_local(Duration::from_millis(1), {
        let ctx = ctx.handle();
        let stale_seen = stale_seen.clone();
        move || {
            if (0..3).any(|index| summary(&ctx, index, "retired-target-marker"))
                || has_label(ctx.workspace_list.upcast_ref(), "retired-target-marker")
            {
                stale_seen.set(true);
            }
            glib::ControlFlow::Continue
        }
    });
    std::fs::write(package.join("hold-following-status"), b"yes").unwrap();
    std::fs::remove_file(package.join("hold-summary")).unwrap();
    std::fs::write(package.join("allow-status"), b"yes").unwrap();
    wait_for(
        || {
            package.join("following-status-held").exists()
                && !ctx.state.borrow().plugins.status_pending(KEY, held_request)
        },
        Duration::from_secs(3),
    );
    assert!((0..3).all(|index| !summary(&ctx, index, "retired-target-marker")));
    assert!(!has_label(
        ctx.workspace_list.upcast_ref(),
        "retired-target-marker"
    ));
    assert!(
        !stale_seen.get(),
        "a stale response was briefly published before fresh collection"
    );
    observation.remove();
    std::fs::write(package.join("allow-following-status"), b"yes").unwrap();
    fresh_summaries(&ctx);
    assert_eq!(
        std::fs::read_to_string(package.join("executed")).unwrap(),
        pid
    );
    assert_eq!(
        ctx.state
            .borrow()
            .plugins
            .running_generation(KEY)
            .as_deref(),
        Some(generation.as_str())
    );
    println!("PASS a managed answer released after the drop cannot relabel a row and the same plugin process remains running");

    begin(&ctx, &window, 0);
    set_sidebar_collapsed(&ctx, true);
    release(&ctx);
    assert_eq!(references(&ctx), [1, 2, 3]);
    begin(&ctx, &window, 0);
    set_sidebar_collapsed(&ctx, false);
    release(&ctx);
    assert_eq!(references(&ctx), [1, 2, 3]);
    begin(&ctx, &window, 0);
    close_pane(&ctx, PaneId(1));
    release(&ctx);
    assert_eq!(references(&ctx), [2, 3]);
    assert_eq!(ctx.state.borrow().workspaces.active().reference, 3);
    println!(
        "PASS presentation changes and closing the dragged workspace invalidate the old gesture"
    );
    begin(&ctx, &window, 0);
    hover(&ctx, &window, 1, true);
    release(&ctx);
    assert_eq!(references(&ctx), [3, 2]);
    fresh_summaries(&ctx);
    let session = crate::session::path().unwrap();
    wait_for(
        || {
            crate::session::load(&session)
                .unwrap()
                .is_some_and(|snapshot| {
                    let value = serde_json::to_value(snapshot).unwrap();
                    value["workspaces"][0]["name"] == "project 3"
                        && value["workspaces"][1]["name"] == "project 2"
                        && value["active"] == 0
                })
        },
        Duration::from_secs(5),
    );
    ctx.state.borrow_mut().plugins.shutdown();
    wait_for(
        || ctx.state.borrow().plugins.is_stopped(),
        Duration::from_secs(5),
    );
    ctx.state.borrow_mut().exiting = true;
    ctx.window.destroy();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "app::plugin_tests::workspace_reorder::real_plugin_updates_and_structural_changes_do_not_retarget_a_workspace_drag", "--nocapture"])
        .env("AMONG_REORDER_RESTORE_PHASE", "yes").output().unwrap();
    println!("{}", String::from_utf8_lossy(&output.stdout));
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        output.status.success(),
        "composed restoration child failed: {}",
        output.status
    );
}

fn workspace_fingerprints(ctx: &Window) -> std::collections::BTreeMap<u64, String> {
    ctx.state
        .borrow()
        .workspaces
        .workspaces()
        .iter()
        .map(|workspace| (workspace.reference, format!("{workspace:?}")))
        .collect()
}

#[test]
#[ignore = "requires an owned display and real foreign-window drag sources"]
fn real_foreign_payloads_and_single_workspace_self_drops_do_not_reorder() {
    let app = application("com.github.among-terminal.workspace-reorder-rejection");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    let ids = xdo(&[
        "search",
        "--onlyvisible",
        "--pid",
        &std::process::id().to_string(),
        "--name",
        &ctx.window.title().unwrap(),
    ]);
    let window = ids.lines().last().unwrap();
    xdo(&["windowactivate", "--sync", window]);
    begin(&ctx, window, 0);
    hover(&ctx, window, 0, true);
    release(&ctx);
    assert_eq!(references(&ctx), [1]);
    new_workspace(&ctx);
    new_workspace(&ctx);
    pump_for(Duration::from_millis(100));
    move_pointer(window, point(&ctx, 2, 0.5));
    xdo(&["click", "--repeat", "2", "--delay", "80", "1"]);
    wait_for(
        || {
            ctx.workspace_list
                .row_at_index(2)
                .and_then(|row| row.child())
                .and_then(|child| find_name_widget(&child))
                .is_some_and(|widget| widget.is::<gtk::Entry>() && widget.width() > 0)
        },
        Duration::from_secs(3),
    );
    let entry = find_name_widget(&ctx.workspace_list.row_at_index(2).unwrap().child().unwrap())
        .unwrap()
        .downcast::<gtk::Entry>()
        .unwrap();
    let bounds = entry.compute_bounds(&ctx.window).unwrap();
    let (sx, sy) = ctx.window.surface_transform();
    let start = (
        (sx + f64::from(bounds.x()) + 5.0) as i32,
        (sy + f64::from(bounds.y() + bounds.height() / 2.0)) as i32,
    );
    move_pointer(window, start);
    pump_for(Duration::from_millis(50));
    xdo(&["mousedown", "1"]);
    pump_for(Duration::from_millis(50));
    move_pointer(window, (start.0 + 100, start.1));
    pump_for(Duration::from_millis(100));
    xdo(&["mouseup", "1"]);
    wait_for(
        || entry.selection_bounds().is_some(),
        Duration::from_secs(2),
    );
    assert!(ctx.state.borrow().workspace_drag.is_none());
    xdo(&["key", "Escape"]);
    wait_for(
        || ctx.state.borrow().renaming.is_none(),
        Duration::from_secs(2),
    );
    assert_eq!(references(&ctx), [1, 2, 3]);
    println!(
        "PASS dragging across an editable workspace name selects text without starting a reorder"
    );
    let foreign_workspace = {
        let state = ctx.state.borrow();
        WorkspaceDrag::new(&state.workspaces, state.workspace_order_revision, 0, false).unwrap()
    };
    let file = crate::settings::directory().unwrap().join("foreign-file");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, b"foreign").unwrap();
    let values = [
        ("text", "foreign terminal text".to_value()),
        (
            "file",
            gtk::gdk::FileList::from_array(&[gio::File::for_path(file)]).to_value(),
        ),
        ("pane", 1u64.to_value()),
        ("workspace", foreign_workspace.to_value()),
    ];
    let handle = gtk::Button::with_label("Foreign drag source");
    let foreign = gtk::Window::builder()
        .title("Foreign drag source")
        .default_width(250)
        .default_height(100)
        .child(&handle)
        .build();
    foreign.present();
    pump_for(Duration::from_millis(100));
    let ids = xdo(&[
        "search",
        "--onlyvisible",
        "--pid",
        &std::process::id().to_string(),
        "--name",
        "^Foreign drag source$",
    ]);
    let foreign_id = ids.lines().last().unwrap();
    xdo(&["windowmove", foreign_id, "1000", "400"]);
    for (kind, value) in values {
        let source = gtk::DragSource::new();
        source.set_actions(gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE);
        source.set_propagation_phase(gtk::PropagationPhase::Capture);
        source.set_content(Some(&gtk::gdk::ContentProvider::for_value(&value)));
        let started = Rc::new(Cell::new(false));
        source.connect_drag_begin({
            let started = started.clone();
            move |_, _| started.set(true)
        });
        let ended = Rc::new(Cell::new(false));
        source.connect_drag_end({
            let ended = ended.clone();
            move |_, _, _| ended.set(true)
        });
        handle.add_controller(source.clone());
        xdo(&["windowactivate", "--sync", foreign_id]);
        move_pointer(foreign_id, (80, 50));
        pump_for(Duration::from_millis(50));
        xdo(&["mousedown", "1"]);
        pump_for(Duration::from_millis(50));
        for offset in [5, 12, 30] {
            move_pointer(foreign_id, (80 + offset, 50));
            pump_for(Duration::from_millis(50));
        }
        wait_for(|| started.get(), Duration::from_secs(2));
        move_pointer(window, point(&ctx, 0, 0.25));
        pump_for(Duration::from_millis(100));
        assert!(ctx.state.borrow().workspace_drag.is_none());
        xdo(&["mouseup", "1"]);
        wait_for(|| ended.get(), Duration::from_secs(3));
        assert_eq!(
            references(&ctx),
            [1, 2, 3],
            "foreign {kind} payload mutated order"
        );
        assert!((0..3).all(|index| {
            let row = ctx.workspace_list.row_at_index(index).unwrap();
            !row.has_css_class("workspace-drop-before")
                && !row.has_css_class("workspace-drop-after")
        }));
        handle.remove_controller(&source);
        println!("PASS real foreign-window {kind} drag is not a workspace reorder");
    }
    foreign.destroy();
    ctx.state.borrow_mut().exiting = true;
    ctx.window.destroy();
}

#[test]
#[ignore = "requires an owned display, real pointer/keyboard events and live shell processes"]
fn real_workspace_moves_preserve_nested_zoomed_terminals_and_do_not_leak_shortcuts() {
    use std::os::fd::AsRawFd;
    use vte::prelude::*;

    let app = application("com.github.among-terminal.workspace-reorder-continuity");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    split_focused(&ctx);
    pump_for(Duration::from_millis(150));
    split_focused(&ctx);
    pump_for(Duration::from_millis(150));
    {
        let mut state = ctx.state.borrow_mut();
        let workspace = state.workspaces.active_mut();
        workspace.nodes.set_ratio_at(&[], 0.35);
        workspace.nodes.set_ratio_at(&[1], 0.65);
    }
    rebuild_panes(&ctx);
    new_workspace(&ctx);
    new_workspace(&ctx);
    ctx.state.borrow_mut().workspaces.activate(0);
    switch_workspace(&ctx);
    toggle_zoom(&ctx);
    let window = xdo(&[
        "search",
        "--onlyvisible",
        "--pid",
        &std::process::id().to_string(),
        "--name",
        &ctx.window.title().unwrap(),
    ])
    .lines()
    .last()
    .unwrap()
    .to_string();
    xdo(&["windowactivate", "--sync", &window]);
    xdo(&["windowfocus", "--sync", &window]);
    let root = crate::settings::directory()
        .unwrap()
        .join("workspace-continuity");
    std::fs::create_dir_all(&root).unwrap();
    let terminals: Vec<_> = ctx
        .state
        .borrow()
        .panes
        .iter()
        .map(|(id, pane)| (*id, pane.terminal().clone()))
        .collect();
    for (id, terminal) in &terminals {
        let directory = root.join(format!("pane-{}", id.0));
        std::fs::create_dir(&directory).unwrap();
        terminal.feed_child(format!("cd '{}'; printf '\\033]7;file://localhost%s\\007' \"$PWD\"; sleep 120 & printf '%s %s' \"$$\" \"$!\" > '{}/processes'\n", directory.display(), directory.display()).as_bytes());
    }
    wait_for(
        || {
            terminals.iter().all(|(id, _)| {
                let directory = root.join(format!("pane-{}", id.0));
                directory.join("processes").is_file()
                    && ctx.state.borrow().pane(*id).unwrap().cwd().as_deref() == directory.to_str()
            })
        },
        Duration::from_secs(5),
    );
    pump_for(Duration::from_millis(200));
    let before = workspace_fingerprints(&ctx);
    let pane_container = ctx.pane_area.first_child().unwrap();
    let focused = ctx.state.borrow().workspaces.active().focused;
    let original: Vec<_> = terminals
        .iter()
        .map(|(id, terminal)| {
            let pgrp = unsafe { libc::tcgetpgrp(terminal.pty().unwrap().fd().as_raw_fd()) };
            assert!(pgrp > 0);
            let directory = root.join(format!("pane-{}", id.0));
            let pids = std::fs::read_to_string(directory.join("processes")).unwrap();
            let processes: Vec<_> = pids
                .split_whitespace()
                .map(|pid| {
                    let cwd = std::fs::read_link(format!("/proc/{pid}/cwd")).unwrap();
                    assert_eq!(cwd, directory);
                    (pid.to_string(), cwd)
                })
                .collect();
            (*id, terminal.parent(), pgrp, processes)
        })
        .collect();
    for (source, target, after, expected) in [
        (0, 2, true, vec![4, 5, 1]),
        (0, 2, true, vec![5, 1, 4]),
        (1, 1, false, vec![5, 1, 4]),
    ] {
        begin(&ctx, &window, source);
        hover(&ctx, &window, target, after);
        release(&ctx);
        wait_for(|| references(&ctx) == expected, Duration::from_secs(3));
        pump_for(Duration::from_millis(50));
        assert_eq!(workspace_fingerprints(&ctx), before);
        assert_eq!(ctx.pane_area.first_child().unwrap(), pane_container);
    }
    begin(&ctx, &window, 0);
    hover(&ctx, &window, 2, true);
    xdo(&["key", "Escape"]);
    release(&ctx);
    pump_for(Duration::from_millis(50));
    assert_eq!(workspace_fingerprints(&ctx), before);
    assert_eq!(ctx.pane_area.first_child().unwrap(), pane_container);
    for ((id, terminal), (original_id, parent, pgrp, processes)) in terminals.iter().zip(&original)
    {
        assert_eq!(id, original_id);
        assert_eq!(terminal.parent(), *parent);
        assert_eq!(
            unsafe { libc::tcgetpgrp(terminal.pty().unwrap().fd().as_raw_fd()) },
            *pgrp
        );
        for (pid, cwd) in processes {
            assert_eq!(
                std::fs::read_link(format!("/proc/{pid}/cwd")).unwrap(),
                *cwd
            );
        }
    }
    assert!(ctx
        .state
        .borrow()
        .pane(focused)
        .unwrap()
        .terminal()
        .has_focus());
    let proof = root.join("input-proof");
    xdo(&[
        "type",
        "--clearmodifiers",
        &format!("printf '%s' \"$$\" > '{}'", proof.display()),
    ]);
    xdo(&["key", "Return"]);
    wait_for(|| proof.is_file(), Duration::from_secs(3));
    let shell = &original.iter().find(|entry| entry.0 == focused).unwrap().3[0].0;
    assert_eq!(&std::fs::read_to_string(proof).unwrap(), shell);
    println!("PASS active/inactive moves, no-op and Escape retain nested ratios, zoom, names, colours, directories, live shell/job PIDs, terminal parents and the active container; real typing reaches the original focused shell");

    toggle_zoom(&ctx);
    pump_for(Duration::from_millis(150));
    let unzoomed_container = ctx.pane_area.first_child().unwrap();
    begin(&ctx, &window, 1);
    hover(&ctx, &window, 0, false);
    release(&ctx);
    assert_eq!(references(&ctx), [1, 5, 4]);
    assert_eq!(ctx.pane_area.first_child().unwrap(), unzoomed_container);
    let paned = ctx.paneds.borrow()[0].widget.clone();
    let previous_ratio = ctx
        .state
        .borrow()
        .workspaces
        .active()
        .nodes
        .ratio_at(&[])
        .unwrap();
    let bounds = paned.compute_bounds(&ctx.window).unwrap();
    let (sx, sy) = ctx.window.surface_transform();
    let horizontal = paned.orientation() == gtk::Orientation::Horizontal;
    let start = if horizontal {
        (
            (sx + f64::from(bounds.x()) + f64::from(paned.position())) as i32,
            (sy + f64::from(bounds.y() + bounds.height() / 2.0)) as i32,
        )
    } else {
        (
            (sx + f64::from(bounds.x() + bounds.width() / 2.0)) as i32,
            (sy + f64::from(bounds.y()) + f64::from(paned.position())) as i32,
        )
    };
    move_pointer(&window, start);
    pump_for(Duration::from_millis(50));
    xdo(&["mousedown", "1"]);
    pump_for(Duration::from_millis(50));
    for offset in [20, 40, 70] {
        move_pointer(
            &window,
            (
                start.0 + if horizontal { offset } else { 0 },
                start.1 + if horizontal { 0 } else { offset },
            ),
        );
        pump_for(Duration::from_millis(50));
    }
    xdo(&["mouseup", "1"]);
    wait_for(
        || {
            (ctx.state
                .borrow()
                .workspaces
                .active()
                .nodes
                .ratio_at(&[])
                .unwrap()
                - previous_ratio)
                .abs()
                > 0.02
        },
        Duration::from_secs(2),
    );
    let changed_ratio = ctx
        .state
        .borrow()
        .workspaces
        .active()
        .nodes
        .ratio_at(&[])
        .unwrap();
    let changed_position = paned.position();
    pump_for(Duration::from_millis(1200));
    assert!(
        (ctx.state
            .borrow()
            .workspaces
            .active()
            .nodes
            .ratio_at(&[])
            .unwrap()
            - changed_ratio)
            .abs()
            < 0.005
    );
    assert!((paned.position() - changed_position).abs() <= 2);
    begin(&ctx, &window, 0);
    hover(&ctx, &window, 1, true);
    release(&ctx);
    assert_eq!(references(&ctx), [5, 1, 4]);
    println!("PASS a real divider drag after moving the unzoomed active workspace updates its nested model and survives subsequent ratio-sync ticks");

    for _ in 3..10 {
        new_workspace(&ctx);
    }
    pump_for(Duration::from_millis(200));
    begin(&ctx, &window, 9);
    hover(&ctx, &window, 8, false);
    release(&ctx);
    assert_eq!(references(&ctx), [5, 1, 4, 6, 7, 8, 9, 10, 12, 11]);
    ctx.state
        .borrow_mut()
        .bindings
        .rebind(Action::ActivateWorkspace(1), "<Control><Alt>g");
    reinstall_shortcuts(&ctx);
    let terminals: Vec<_> = ctx
        .state
        .borrow()
        .panes
        .iter()
        .map(|(id, pane)| (*id, pane.terminal().clone()))
        .collect();
    // Every terminal records raw input, including hidden ones. A shortcut that
    // switches focus before leaking its bytes is caught at either destination.
    for (id, terminal) in &terminals {
        terminal.feed_child(format!("stty -echo -icanon min 1 time 0; printf ready > '{0}/{1}-ready'; timeout 4 dd bs=1 count=128 of='{0}/{1}-input' 2>/dev/null; stty sane; printf done > '{0}/{1}-done'\n", root.display(), id.0).as_bytes());
    }
    wait_for(
        || {
            terminals
                .iter()
                .all(|(id, _)| root.join(format!("{}-ready", id.0)).is_file())
        },
        Duration::from_secs(3),
    );
    for (key, reference) in [
        ("alt+1", 5),
        ("ctrl+alt+g", 1),
        ("alt+9", 12),
        ("ctrl+Next", 11),
        ("ctrl+Next", 5),
        ("ctrl+Prior", 11),
        ("ctrl+alt+g", 1),
    ] {
        xdo(&["key", "--clearmodifiers", key]);
        wait_for(
            || ctx.state.borrow().workspaces.active().reference == reference,
            Duration::from_secs(2),
        );
    }
    wait_for(
        || {
            terminals
                .iter()
                .all(|(id, _)| root.join(format!("{}-done", id.0)).is_file())
        },
        Duration::from_secs(6),
    );
    for (id, _) in &terminals {
        assert!(
            std::fs::read(root.join(format!("{}-input", id.0)))
                .unwrap()
                .is_empty(),
            "shortcut leaked into pane {id:?}"
        );
    }
    println!(
        "PASS recognized positional, custom and wrapping cycle keys follow the ninth/tenth boundary and emit no bytes into any real terminal"
    );
    ctx.state.borrow_mut().exiting = true;
    ctx.window.destroy();
}
