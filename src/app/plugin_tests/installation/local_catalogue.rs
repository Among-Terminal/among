//! Native development-mode installation from a local schema2 catalogue: the
//! real Preferences window, real keyboard input, the real installer worker
//! and the live plugin inventory.

use super::*;

#[test]
#[ignore = "requires an isolated native display, xdotool and its own process for development mode"]
fn local_catalogue_installs_and_replaces_only_with_explicit_review_in_development_mode() {
    use crate::plugin::install::test_package as fixture_for;
    use serde_json::json;
    // Ignored native tests run one per process, so fixing the mode here
    // affects no other test.
    assert!(crate::plugin::development::fix(true));
    let root = crate::plugin::inventory::root().unwrap();
    let author = crate::test_isolation::check_root().join("local-author");
    std::fs::create_dir_all(&author).unwrap();
    std::fs::set_permissions(&author, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (mut version, package) = fixture_for("sample");
    version.as_object_mut().unwrap().remove("schema_version");
    let asset = version["artifacts"][0]["asset"]
        .as_str()
        .unwrap()
        .to_string();
    std::fs::write(author.join(&asset), &package).unwrap();
    std::fs::write(
        author.join("among-plugins.json"),
        json!({"schema_version":2,"plugins":[{"id":"sample","versions":[version]}]}).to_string(),
    )
    .unwrap();
    for file in [asset.as_str(), "among-plugins.json"] {
        std::fs::set_permissions(author.join(file), std::fs::Permissions::from_mode(0o644))
            .unwrap();
    }

    let app = application("com.github.among-terminal.local-catalogue-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    ctx.window
        .set_title(Some("Local catalogue fixture terminal"));
    focus(&ctx.window);
    open_preferences(&ctx);
    let preferences = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
        .find(|window| window.title().as_deref() == Some("Preferences"))
        .unwrap();
    let page = widgets(preferences.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::PreferencesPage>().ok())
        .find(|page| page.title() == "Plugins")
        .unwrap();
    preferences.set_visible_page(&page);
    assert!(has_label(page.upcast_ref(), "Plugin development mode"));
    // The real entry point exists in development mode; the file chooser it
    // opens is replaced by the chosen path below.
    button(&page, "Install from local catalogue…");

    let changed: Rc<dyn Fn()> = Rc::new({
        let state = ctx.state.clone();
        move || reconcile_managed_installations(&state)
    });
    let details: Rc<crate::prefs::PluginDetailsQuery> = Rc::new({
        let state = ctx.state.clone();
        move |key: &str| state.borrow().plugins.managed_details(key)
    });
    crate::prefs::local_catalogue::present_path_for_test(
        preferences.upcast_ref(),
        &author.join("among-plugins.json"),
        changed.clone(),
        details.clone(),
    );
    let view = local_window();
    focus(&view);
    assert!(has_label(
        view.upcast_ref(),
        crate::plugin::inventory::LOCAL_MISSING_PROVENANCE
    ));
    assert!(
        !root.join("installed.json").exists(),
        "opening reads metadata only"
    );
    press(&button(&view, "Review…"));
    wait_for(|| view.visible_dialog().is_some(), Duration::from_secs(2));
    let dialog = view.visible_dialog().unwrap();
    assert!(has_label(
        dialog.upcast_ref(),
        "Install this local package?"
    ));
    screenshot_dialog(&dialog, "local-catalogue-review");
    press(&button(&dialog, "Install"));
    let installed = "Installed development/sample disabled. Enable separately in Your plugins.";
    wait_for(
        || has_label(view.upcast_ref(), installed),
        Duration::from_secs(5),
    );
    wait_for(
        || {
            ctx.state
                .borrow()
                .plugins
                .managed_details("development/sample")
                .is_some()
        },
        Duration::from_secs(5),
    );
    let first = ctx
        .state
        .borrow()
        .plugins
        .managed_details("development/sample")
        .unwrap()
        .0
        .receipt;
    assert!(first.requires_development());
    assert!(!ctx
        .state
        .borrow()
        .plugins
        .desired_enabled("development/sample"));
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
    view.destroy();

    // A second install of the same ID is a replacement that needs its own
    // confirmation and starts with a new consent token.
    crate::prefs::local_catalogue::present_path_for_test(
        preferences.upcast_ref(),
        &author.join("among-plugins.json"),
        changed,
        details,
    );
    let view = local_window();
    focus(&view);
    press(&button(&view, "Review…"));
    wait_for(|| view.visible_dialog().is_some(), Duration::from_secs(2));
    let dialog = view.visible_dialog().unwrap();
    assert!(has_label(
        dialog.upcast_ref(),
        "Replace the development install?"
    ));
    press(&button(&dialog, "Cancel"));
    wait_for(|| view.visible_dialog().is_none(), Duration::from_secs(2));
    assert_eq!(
        crate::plugin::inventory::read(&root).unwrap()[0].receipt,
        first,
        "cancelled replacement changes nothing"
    );
    press(&button(&view, "Review…"));
    wait_for(|| view.visible_dialog().is_some(), Duration::from_secs(2));
    press(&button(&view.visible_dialog().unwrap(), "Replace"));
    wait_for(
        || has_label(view.upcast_ref(), installed),
        Duration::from_secs(5),
    );
    let entries = crate::plugin::inventory::read(&root).unwrap();
    assert_eq!(entries.len(), 1);
    assert_ne!(entries[0].receipt.installation, first.installation);
    assert_eq!(entries[0].previous.as_ref(), Some(&first));
    wait_for(
        || {
            ctx.state
                .borrow()
                .plugins
                .managed_details("development/sample")
                .is_some_and(|(entry, _)| entry.receipt == entries[0].receipt)
        },
        Duration::from_secs(5),
    );
    assert!(!ctx
        .state
        .borrow()
        .plugins
        .desired_enabled("development/sample"));
    view.destroy();
    preferences.destroy();
    ctx.window.destroy();
    println!("PASS development-mode Preferences showed the mode and the local catalogue action, installed a local schema2 package disabled into development/sample through review, refused nothing silently, and replaced it only after a separate confirmation with a new consent token.");
}

fn local_window() -> adw::PreferencesWindow {
    let title = "Install from local catalogue";
    wait_for(
        || {
            gtk::Window::list_toplevels().iter().any(|window| {
                window
                    .downcast_ref::<adw::PreferencesWindow>()
                    .is_some_and(|window| window.title().as_deref() == Some(title))
            })
        },
        Duration::from_secs(2),
    );
    gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
        .find(|window| window.title().as_deref() == Some(title))
        .unwrap()
}
