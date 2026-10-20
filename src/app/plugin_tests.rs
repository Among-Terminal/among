use super::*;
use crate::plugin::host::PluginState;

fn wait_for(predicate: impl Fn() -> bool, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        while glib::MainContext::default().pending() {
            glib::MainContext::default().iteration(false);
        }
        if predicate() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "live plugin check timed out"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn xdo(args: &[&str]) -> String {
    let output = std::process::Command::new("xdotool")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn has_label(widget: &gtk::Widget, text: &str) -> bool {
    if widget
        .downcast_ref::<gtk::Label>()
        .is_some_and(|label| label.text() == text)
    {
        return true;
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        if has_label(&widget, text) {
            return true;
        }
        child = widget.next_sibling();
    }
    false
}

fn fixture(mode: &str) -> Vec<String> {
    let executable = std::env::current_exe().unwrap();
    let path = executable
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/fake-plugin");
    assert!(
        path.is_file(),
        "build fake-plugin before filtered graphical tests"
    );
    vec![path.to_string_lossy().into_owned(), mode.into()]
}

fn application(id: &str) -> adw::Application {
    let config = std::env::var_os("XDG_CONFIG_HOME").unwrap();
    crate::test_isolation::assert_below_check_root(std::path::Path::new(&config));
    std::env::set_var("SHELL", "/bin/sh");
    let app = adw::Application::builder()
        .application_id(id)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    app
}

#[test]
#[ignore = "requires an isolated graphical session, built fake-plugin and real keyboard input"]
fn hostile_plugin_pipes_preserve_terminal_input_and_withdraw_sole_content() {
    let app = application("com.github.among-terminal.plugin-transport-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    ctx.state.borrow_mut().plugins.register(PluginSpec {
        name: "sole".into(),
        command: fixture("one-status"),
        enabled: true,
    });
    wait_for(
        || has_label(ctx.app_header.upcast_ref(), "vanishing"),
        Duration::from_secs(5),
    );
    ctx.state
        .borrow_mut()
        .plugins
        .broadcast_event(EventKind::WorkingDirectoryChanged, 1, None);
    wait_for(
        || !has_label(ctx.app_header.upcast_ref(), "vanishing"),
        Duration::from_secs(2),
    );
    assert!(ctx.state.borrow().store.is_empty(Scope::AppHeader));
    println!(
        "PASS sole faulted contributor is withdrawn from the real header without another result"
    );
    ctx.state.borrow_mut().plugins.disable("sole");

    ctx.state.borrow_mut().plugins.register(PluginSpec {
        name: "disabled".into(),
        command: fixture("echo"),
        enabled: true,
    });
    wait_for(
        || has_label(ctx.app_header.upcast_ref(), "ok"),
        Duration::from_secs(5),
    );
    ctx.state.borrow_mut().plugins.disable("disabled");
    wait_for(
        || !has_label(ctx.app_header.upcast_ref(), "ok"),
        Duration::from_secs(2),
    );
    assert!(ctx.state.borrow().store.is_empty(Scope::AppHeader));
    println!("PASS sole disabled contributor is withdrawn without a replacement plugin");

    let ids = xdo(&[
        "search",
        "--onlyvisible",
        "--pid",
        &std::process::id().to_string(),
        "--name",
        "Among Terminal",
    ]);
    let id = ids.lines().last().unwrap();
    xdo(&["windowactivate", "--sync", id]);
    xdo(&["windowfocus", "--sync", id]);
    xdo(&["key", "--clearmodifiers", "ctrl+shift+t"]);
    wait_for(
        || ctx.state.borrow().workspaces.active_index() == 1,
        Duration::from_secs(2),
    );
    ctx.state.borrow_mut().plugins.register(PluginSpec {
        name: "blocked".into(),
        command: fixture("never-read"),
        enabled: true,
    });
    wait_for(
        || {
            ctx.state
                .borrow()
                .plugins
                .listing()
                .iter()
                .any(|(name, state)| name == "blocked" && state.is_healthy())
        },
        Duration::from_secs(2),
    );
    let started = std::time::Instant::now();
    for _ in 0..8 {
        ctx.state.borrow_mut().plugins.broadcast_event(
            EventKind::WorkingDirectoryChanged,
            1,
            Some("x".repeat(60 * 1024)),
        );
    }
    xdo(&["key", "--clearmodifiers", "ctrl+Page_Up"]);
    wait_for(
        || ctx.state.borrow().workspaces.active_index() == 0,
        Duration::from_secs(1),
    );
    xdo(&[
        "type",
        "--clearmodifiers",
        "--delay",
        "0",
        "printf alive > \"$HOME/plugin-alive\"",
    ]);
    xdo(&["key", "--clearmodifiers", "Return"]);
    let output = std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join("plugin-alive");
    wait_for(|| output.exists(), Duration::from_secs(1));
    assert_eq!(std::fs::read_to_string(output).unwrap(), "alive");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "typing did not precede the blocked-write deadline"
    );
    wait_for(
        || {
            ctx.state.borrow().plugins.listing().iter().any(|(name,state)|name=="blocked"&&matches!(state,PluginState::Faulted{reason} if reason.contains("two seconds")))
        },
        Duration::from_secs(4),
    );
    println!("PASS real workspace shortcut and terminal input while stdin is full; writer deadline contained the fixture");
    ctx.state.borrow_mut().plugins.register(PluginSpec {
        name: "shutdown".into(),
        command: fixture("stubborn"),
        enabled: true,
    });
    wait_for(
        || {
            ctx.state
                .borrow()
                .plugins
                .listing()
                .iter()
                .any(|(name, state)| name == "shutdown" && state.is_healthy())
        },
        Duration::from_secs(2),
    );
    let start = std::time::Instant::now();
    ctx.window.destroy();
    assert!(start.elapsed() < Duration::from_millis(100));
    assert!(!ctx.state.borrow().plugins.is_stopped());
    wait_for(
        || ctx.state.borrow().plugins.is_stopped(),
        Duration::from_secs(2),
    );
    println!(
        "PASS last-window destruction stays nonblocking and retains supervision until reaping"
    );
}
