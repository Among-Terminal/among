use super::bell_tests::{pump_for, real_bell, wait};
use super::desktop_notification_tests::{activate, named, Server, APP_ID};
use super::*;
use among::notifications::{preferences::Edit, Policy, COOLDOWN};

struct FileLimit {
    original: libc::rlimit,
    restored: bool,
}

impl FileLimit {
    fn refuse_new_descriptors() -> Self {
        let mut original = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut original) },
            0
        );
        let denied = libc::rlimit {
            rlim_cur: 0,
            rlim_max: original.rlim_max,
        };
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &denied) }, 0);
        Self {
            original,
            restored: false,
        }
    }

    fn restore(&mut self) {
        assert_eq!(
            unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &self.original) },
            0
        );
        self.restored = true;
    }
}

impl Drop for FileLimit {
    fn drop(&mut self) {
        if !self.restored {
            // an assertion must not leave the test process with its soft limit
            // lowered while GTK and GIO finish disposing their existing objects.
            unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &self.original) };
        }
    }
}

#[test]
#[ignore = "requires an exact fresh test process, owned X11 display, private session bus and xdotool"]
fn failed_desktop_edit_withdraws_and_requires_explicit_retry_without_saving() {
    let config = std::path::PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap());
    crate::test_isolation::assert_below_check_root(&config);
    assert!(!gtk::is_initialized(), "run this test in its own process");
    assert!(std::env::args().any(|argument| argument == "--exact"));
    assert!(std::env::args().any(|argument| argument == "--test-threads=1"));
    let server = Server::new();
    let application = adw::Application::builder().application_id(APP_ID).build();
    application.register(gio::Cancellable::NONE).unwrap();
    let ctx = build_with_writer(&application, None, crate::session::Writer::new).unwrap();
    wait(
        || ctx.state.borrow().session_ready,
        "initial terminal ready",
    );
    let backend = ctx.state.borrow().bells.clone().unwrap();
    wait(
        || backend.desktop_report().is_none(),
        "notification service observed",
    );
    let context = general_context(&ctx).unwrap();
    assert_eq!(
        (context.edit)(Edit::Desktop(Policy::DESKTOP_DEFAULT)),
        Ok(crate::settings::Durability::Durable)
    );
    let accepted = backend.preferences();
    let file = crate::settings::directory().unwrap().join("general.json");
    let original = std::fs::read(&file).unwrap();
    let pane = ctx.state.borrow().workspaces.active().focused;
    let other = gtk::Window::builder()
        .title("Outside Among during desktop failure")
        .default_width(300)
        .default_height(160)
        .build();
    other.present();
    open_preferences(&ctx);
    let preferences = gtk::Window::list_toplevels()
        .into_iter()
        .find_map(|widget| widget.downcast::<adw::PreferencesWindow>().ok())
        .unwrap();
    preferences.set_visible_page_name("notifications");
    pump_for(Duration::from_millis(150));
    let enabled: adw::SwitchRow = named(&preferences, "notification-desktop-switch");
    let banner: adw::Banner = named(&preferences, "notification-save-status");
    assert!(enabled.is_active());
    activate(&other, "background-before-failed-policy-edit");
    wait(
        || !backend.focus().application_active,
        "Among in background before the first notification",
    );
    real_bell(&ctx, &backend, pane);
    wait(
        || server.additions().len() == 1,
        "initial desktop notification",
    );
    let id = server.additions()[0].0.clone();
    assert!(!server.removed(&id));

    // only this fresh exact test process lowers its soft limit. restore it
    // before dispatching the main loop or opening any evidence/settings file.
    let mut limit = FileLimit::refuse_new_descriptors();
    let probe = unsafe {
        libc::memfd_create(
            c"desktop-authority-failure-probe".as_ptr(),
            libc::MFD_CLOEXEC,
        )
    };
    let refusal = std::io::Error::last_os_error().raw_os_error();
    enabled.set_active(false);
    limit.restore();
    assert_eq!(probe, -1);
    assert_eq!(refusal, Some(libc::EMFILE));
    assert_eq!(backend.preferences(), accepted);
    assert_eq!(std::fs::read(&file).unwrap(), original);
    assert!(banner.is_revealed());
    assert!(banner.title().contains("Not saved:"));
    let failure = backend.desktop_report().unwrap();
    assert!(failure.contains("Change a Desktop setting to retry"));
    wait(
        || server.removed(&id),
        "failed policy edit withdraws its old notification",
    );
    pump_for(Duration::from_millis(150));
    assert_eq!(enabled.subtitle().as_deref(), Some(failure));

    let visual = Policy {
        enabled: !accepted.visual.enabled,
        ..accepted.visual
    };
    assert_eq!(
        (context.edit)(Edit::Visual(visual)),
        Ok(crate::settings::Durability::Durable),
        "another channel's edit must remain independent"
    );
    assert_eq!(backend.preferences().visual, visual);
    assert_eq!(backend.preferences().desktop, accepted.desktop);
    assert_eq!(backend.desktop_report(), Some(failure));
    let updated: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(
        updated["notifications"]["visual"]["enabled"],
        visual.enabled
    );
    assert_eq!(
        updated["notifications"]["desktop"]["enabled"],
        accepted.desktop.enabled
    );
    activate(&other, "background-before-bell-with-failed-policy");
    wait(
        || !backend.focus().application_active,
        "Among in background with failed Desktop authority",
    );
    real_bell(&ctx, &backend, pane);
    assert_eq!(
        server.additions().len(),
        1,
        "failed authority delivered a bell"
    );

    enabled.set_active(true);
    assert_eq!(backend.preferences().desktop, accepted.desktop);
    assert!(backend.desktop_report().is_none());
    assert!(!banner.is_revealed());
    pump_for(COOLDOWN + Duration::from_millis(100));
    assert_eq!(
        server.additions().len(),
        1,
        "a bell from the failed revision replayed"
    );
    real_bell(&ctx, &backend, pane);
    wait(
        || server.additions().len() == 2,
        "a fresh bell after explicit policy retry",
    );
    println!(
        "PASS actual EMFILE refuses Desktop edit without saving, withdraws old notification, preserves independent Visual edits, and requires an explicit same-policy retry before a fresh bell sends"
    );
    preferences.close();
    other.destroy();
    ctx.window.destroy();
}
