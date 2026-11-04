use super::*;

pub(super) fn pump_for(duration: Duration) {
    let deadline = std::time::Instant::now() + duration;
    while std::time::Instant::now() < deadline {
        while glib::MainContext::default().pending() {
            glib::MainContext::default().iteration(false);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub(super) fn wait(mut condition: impl FnMut() -> bool, description: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !condition() && std::time::Instant::now() < deadline {
        pump_for(Duration::from_millis(20));
    }
    assert!(condition(), "timed out: {description}");
}

pub(super) fn xdo(args: &[&str]) -> String {
    let output = std::process::Command::new("xdotool")
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

pub(super) fn activate(window: &impl IsA<gtk::Window>) {
    let window = window.as_ref();
    let ids = xdo(&[
        "search",
        "--onlyvisible",
        "--pid",
        &std::process::id().to_string(),
        "--name",
        &window.title().unwrap(),
    ]);
    xdo(&["windowactivate", "--sync", ids.lines().last().unwrap()]);
    wait(|| window.is_active(), "window activation");
    pump_for(Duration::from_millis(100));
}

pub(super) fn terminal(ctx: &Window, id: PaneId) -> vte::Terminal {
    ctx.state.borrow().pane(id).unwrap().terminal().clone()
}

pub(super) fn real_bell(ctx: &Window, backend: &bell::Backend, id: PaneId) {
    let before = backend.event_count();
    terminal(ctx, id).feed_child(b"printf '\\007'\n");
    wait(
        || backend.event_count() == before + 1,
        "real PTY semantic bell",
    );
    pump_for(Duration::from_millis(100));
}

#[test]
#[ignore = "requires an isolated X11 session with a window manager and xdotool"]
fn live_bells_follow_terminal_focus_lifetime_and_owned_windows() {
    let config = std::path::PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap());
    crate::test_isolation::assert_below_check_root(&config);
    let app = adw::Application::builder()
        .application_id("com.github.among-terminal.bell-backend-test")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    pump_for(Duration::from_millis(500));
    activate(&ctx.window);
    let backend = ctx.state.borrow().bells.clone().unwrap();
    let first = ctx.state.borrow().workspaces.active().focused;
    wait(
        || backend.focus().viewed.map(|s| s.pane()) == Some(first.0),
        "first terminal focus",
    );
    assert_eq!(backend.event_count(), 0);
    assert!(!terminal(&ctx, first).is_audible_bell());

    terminal(&ctx, first).feed_child(b"printf '\\033]0;osc-only\\007'\n");
    wait(
        || terminal(&ctx, first).window_title().as_deref() == Some("osc-only"),
        "OSC title",
    );
    pump_for(Duration::from_millis(150));
    assert_eq!(backend.event_count(), 0, "OSC BEL terminator became a bell");
    real_bell(&ctx, &backend, first);
    assert!(!backend.attention(first.0).unread);
    println!("PASS real PTY BEL enters the host; OSC title BEL terminator does not; viewed default bell has no unread; independent VTE audio is disabled");

    split_focused(&ctx);
    pump_for(Duration::from_millis(200));
    let second = ctx.state.borrow().workspaces.active().focused;
    split_focused(&ctx);
    pump_for(Duration::from_millis(200));
    let third = ctx.state.borrow().workspaces.active().focused;
    real_bell(&ctx, &backend, first);
    real_bell(&ctx, &backend, second);
    assert!(backend.attention(first.0).unread);
    assert!(backend.attention(second.0).unread);
    assert_eq!(
        backend.aggregate([first.0, second.0, third.0]).unread_panes,
        2
    );
    assert!(!backend.attention(third.0).unread);

    toggle_zoom(&ctx);
    pump_for(Duration::from_millis(100));
    assert!(!terminal(&ctx, first).is_mapped());
    real_bell(&ctx, &backend, first);
    assert!(backend.attention(first.0).unread);
    // actual Restore activation returns keyboard focus to the source terminal;
    // its hidden siblings must not be acknowledged by header reconstruction.
    let restore = find_with_class(ctx.pane_area.upcast_ref(), "pane-restore")
        .unwrap()
        .downcast::<gtk::Button>()
        .unwrap();
    assert!(restore.grab_focus());
    xdo(&["key", "--clearmodifiers", "Return"]);
    wait(
        || !ctx.state.borrow().workspaces.active().is_zoomed(),
        "Restore",
    );
    wait(
        || backend.focus().viewed.map(|s| s.pane()) == Some(third.0),
        "Restore keyboard focus",
    );
    assert!(backend.attention(first.0).unread && backend.attention(second.0).unread);
    println!("PASS live zoom-hidden sibling bells survive Restore reconstruction; returned actual terminal focus does not acknowledge siblings");

    let flood_started = std::time::Instant::now();
    let before_flood = backend.event_count();
    terminal(&ctx, first).feed_child(b"printf '\\007%.0s' {1..20000}\n");
    xdo(&[
        "type",
        "--clearmodifiers",
        "--delay",
        "1",
        "printf '\\033]0;input-still-live\\007'",
    ]);
    xdo(&["key", "--clearmodifiers", "Return"]);
    wait(
        || terminal(&ctx, third).window_title().as_deref() == Some("input-still-live"),
        "real keyboard input during sibling bell flood",
    );
    wait(
        || backend.event_count() > before_flood,
        "flood semantic ingress",
    );
    assert_eq!(backend.focus().viewed.map(|s| s.pane()), Some(third.0));
    assert_eq!(
        backend.aggregate([first.0, second.0, third.0]).unread_panes,
        2
    );
    println!("PASS real 20000-BEL PTY burst retains per-source unread and real keyboard input in a sibling stays usable ({:?}); VTE may coalesce semantic signals", flood_started.elapsed());

    new_workspace(&ctx);
    pump_for(Duration::from_millis(200));
    real_bell(&ctx, &backend, third);
    assert!(backend.attention(third.0).unread);
    ctx.state.borrow_mut().workspaces.activate(0); // selection without focus
    backend.observe_focus();
    assert!(backend.attention(third.0).unread);
    switch_workspace(&ctx);
    wait(
        || !backend.attention(third.0).unread,
        "workspace actually focusing third terminal",
    );
    assert!(backend.attention(first.0).unread && backend.attention(second.0).unread);
    println!("PASS inactive-workspace bell retains its real origin; model selection alone does not acknowledge; actual returned focus acknowledges only its source");

    run_action(&ctx, Action::OpenPreferences);
    pump_for(Duration::from_millis(300));
    let preferences = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Window>().ok())
        .find(|window| window.title().as_deref() == Some("Preferences"))
        .unwrap();
    activate(&preferences);
    assert!(
        backend.focus().application_active,
        "Preferences is still Among"
    );
    assert!(backend.focus().viewed.is_none());
    real_bell(&ctx, &backend, third);
    rebuild(&ctx);
    pump_for(Duration::from_millis(150));
    assert!(
        backend.attention(third.0).unread,
        "rebuilding behind Preferences acknowledged a source"
    );
    preferences.destroy();
    activate(&ctx.window);
    apply_focus(&ctx);
    wait(
        || !backend.attention(third.0).unread,
        "focus returned from Preferences",
    );
    println!("PASS real plain-transient Preferences counts as foreground Among, not terminal viewing; reconstruction behind Preferences preserves unread");

    xdo(&["key", "--clearmodifiers", "shift+F10"]);
    wait(
        || ctx.state.borrow().pane(third).unwrap().menu_open(),
        "native context menu",
    );
    assert!(backend.focus().application_active);
    assert!(backend.focus().viewed.is_none());
    real_bell(&ctx, &backend, third);
    assert!(backend.attention(third.0).unread);
    xdo(&["key", "--clearmodifiers", "Escape"]);
    wait(
        || !backend.attention(third.0).unread,
        "focus returned from menu",
    );
    println!("PASS native keyboard context menu does not acknowledge bells while it owns focus; Escape's real terminal focus acknowledges its source");

    let unrelated = gtk::Window::builder()
        .title("bell-unrelated-window")
        .default_width(300)
        .default_height(200)
        .build();
    unrelated.present();
    pump_for(Duration::from_millis(150));
    activate(&unrelated);
    assert!(!backend.focus().application_active);
    real_bell(&ctx, &backend, third);
    assert!(
        backend.attention(third.0).unread,
        "remembered background terminal focus acknowledged"
    );
    unrelated.destroy();
    activate(&ctx.window);
    wait(
        || !backend.attention(third.0).unread,
        "main window reactivation",
    );
    assert!(backend.attention(first.0).unread && backend.attention(second.0).unread);
    println!("PASS unrelated active window makes Among background; remembered terminal selection cannot acknowledge; real main activation acknowledges only its source");

    let stale = terminal(&ctx, first);
    close_pane(&ctx, first);
    pump_for(Duration::from_millis(100));
    assert_eq!(
        backend.attention(first.0),
        among::notifications::Attention::default()
    );
    let events = backend.event_count();
    stale.emit_by_name::<()>("bell", &[]); // a late callback after lifetime revocation
    assert_eq!(backend.event_count(), events);
    assert!(backend.attention(second.0).unread);
    assert_eq!(
        backend.aggregate([first.0, second.0, third.0]).unread_panes,
        1
    );
    assert!(backend
        .diagnostics()
        .iter()
        .any(|diagnostic| diagnostic.kind == among::notifications::DiagnosticKind::StaleSource));
    println!("PASS closed source loses attention; late semantic callback is rejected without redirecting to surviving panes or changing workspace aggregation");

    ctx.window.destroy();
    pump_for(Duration::from_millis(100));
    stale.emit_by_name::<()>("bell", &[]);
    assert_eq!(
        backend.aggregate([second.0, third.0]),
        among::notifications::Aggregate::default()
    );
    println!("PASS destroying the owning main window drops session-local state and rejects callbacks after shutdown");
}

#[test]
#[ignore = "requires isolated X11/xdotool; run notification_commands first to build its Rust fixture"]
fn hostile_command_backend_preserves_real_terminal_input_and_nonblocking_shutdown() {
    use among::notifications::commands::{self, Configuration, OwnedDirectory, Runtime};
    use among::notifications::{Eligibility, Policy};
    use std::os::unix::fs::DirBuilderExt;

    fn pids(root: &std::path::Path) -> Vec<u32> {
        std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("started-")
                    || path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("child-")
            })
            .map(|path| std::fs::read_to_string(path).unwrap().parse().unwrap())
            .collect()
    }
    let config = std::path::PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap());
    crate::test_isolation::assert_below_check_root(&config);
    let root =
        crate::test_isolation::check_root().join(format!("native-command-{}", std::process::id()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    let cwd = root.join("owned-cwd");
    let output = root.join("hostile-tree");
    std::fs::DirBuilder::new().mode(0o700).create(&cwd).unwrap();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&output)
        .unwrap();
    let target = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    let fixture = target.join("notification-command-fixture");
    assert!(
        fixture.is_file(),
        "run cargo test --test notification_commands before this native check"
    );
    let app = adw::Application::builder()
        .application_id("com.github.among-terminal.command-backend-test")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    pump_for(Duration::from_millis(300));
    activate(&ctx.window);
    let origin = ctx.state.borrow().workspaces.active().focused;
    let backend = ctx.state.borrow().bells.clone().unwrap();
    split_focused(&ctx);
    pump_for(Duration::from_millis(200));
    let focused = ctx.state.borrow().workspaces.active().focused;
    assert_ne!(origin, focused);
    wait(
        || terminal(&ctx, focused).has_focus(),
        "sibling terminal focus",
    );
    let mut runtime =
        Runtime::new(&target.join("among"), OwnedDirectory::open(&cwd).unwrap()).unwrap();
    let channel = backend
        .with_host(|host| {
            runtime.register(
                host,
                Configuration::new(
                    &fixture,
                    vec!["tree".into(), output.to_str().unwrap().into()],
                )
                .unwrap(),
            )
        })
        .unwrap()
        .unwrap();
    backend
        .with_host(|host| {
            runtime.set_policy(
                host,
                channel,
                Policy {
                    enabled: true,
                    eligibility: Eligibility::Always,
                },
            )
        })
        .unwrap()
        .unwrap();
    let state = Rc::new(RefCell::new(runtime));
    let stopped = Rc::new(Cell::new(false));
    glib::timeout_add_local(Duration::from_millis(10), {
        let state = state.clone();
        let backend = backend.clone();
        let stopped = stopped.clone();
        move || {
            if stopped.get() {
                return glib::ControlFlow::Break;
            }
            let _ =
                backend.with_host(|host| state.borrow_mut().poll(host, std::time::Instant::now()));
            glib::ControlFlow::Continue
        }
    });
    real_bell(&ctx, &backend, origin);
    wait(|| pids(&output).len() >= 3, "real stubborn command tree");
    assert!(backend.attention(origin.0).unread);
    let events = backend.event_count();
    let began = std::time::Instant::now();
    xdo(&[
        "type",
        "--clearmodifiers",
        "--delay",
        "1",
        "printf '\\033]0;command-input-alive\\007'",
    ]);
    xdo(&["key", "--clearmodifiers", "Return"]);
    wait(
        || terminal(&ctx, focused).window_title().as_deref() == Some("command-input-alive"),
        "real terminal keyboard input during hostile command",
    );
    assert!(began.elapsed() < Duration::from_secs(1));
    assert_eq!(commands::active_count(), 1);
    assert_eq!(
        backend.event_count(),
        events,
        "sibling OSC title terminator became another command event"
    );
    assert!(backend
        .with_host(|host| host.command_completions().next().is_none())
        .unwrap());
    let began = std::time::Instant::now();
    {
        backend
            .with_host(|host| state.borrow_mut().shutdown(host))
            .unwrap();
    }
    assert!(
        began.elapsed() < Duration::from_millis(50),
        "shutdown blocked the GTK caller"
    );
    ctx.window.destroy();
    wait(
        || commands::active_count() == 0,
        "off-loop shutdown/reaping",
    );
    assert!(pids(&output)
        .into_iter()
        .all(|pid| !std::path::Path::new(&format!("/proc/{pid}")).exists()));
    let supervisors = std::fs::read_dir(&output)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("supervisor-")
        });
    for path in supervisors {
        let pid: u32 = std::fs::read_to_string(path).unwrap().parse().unwrap();
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    }
    stopped.set(true);
    println!("PASS real semantic VTE bell from one pane dispatches the configured command through the actual host; hostile leader/child/grandchild run with production limits while real sibling VTE keyboard input responds within one second; shutdown returns within 50ms and supervisor/leader/descendants reap off-loop");
    println!("NOTE channel registration/polling uses test-only access to the real host and the reusable command API, not a not-yet-implemented General control or default live channel registration");
}
