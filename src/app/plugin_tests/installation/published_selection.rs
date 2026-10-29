use super::published_lifecycle::{inspect, observed, Observed};
use super::*;

const KEY: &str = "github:1431055733/clock";

fn selected_row(view: &adw::PreferencesWindow, expected: &str) -> adw::ActionRow {
    widgets(view.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
        .find(|row| row.subtitle().as_deref() == Some(expected))
        .unwrap()
}

fn visible_description(row: &adw::ActionRow, expected: &str) {
    let label = widgets(row.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
        .find(|label| label.text() == expected)
        .unwrap();
    wait_for(
        || label.is_mapped() && label.width() > 0 && label.height() > 0,
        Duration::from_secs(2),
    );
    assert_eq!(label.accessible_role(), gtk::AccessibleRole::Label);
}

// The release, asset names, sizes and digests come from the prepared
// observation of the current latest official release, so the case follows
// each new release instead of asserting an older one.
#[test]
#[ignore = "requires an isolated native display, keyboard input and prepared inputs of the latest official release"]
fn published_native_selection_is_shown_installed_disabled_and_reviewed_without_implicit_access() {
    let Observed {
        platform,
        architecture,
        snapshot,
        binding,
        revision,
        catalogue,
        ..
    } = observed();
    let rows: Vec<_> = catalogue
        .plugins
        .iter()
        .map(|plugin| {
            let version = &plugin.versions[0];
            let artifact = version.select(&platform).unwrap();
            (
                plugin.id.clone(),
                version.manifest.version.clone(),
                artifact.clone(),
                format!(
                    "github:1431055733/{} · version {} · linux/{architecture} · asset {} · {} bytes",
                    plugin.id, version.manifest.version, artifact.asset, artifact.size
                ),
            )
        })
        .collect();
    let root = crate::plugin::inventory::root().unwrap();
    crate::test_isolation::assert_below_check_root(&root);
    assert!(crate::plugin::inventory::read(&root).unwrap().is_empty());
    let app = application("com.github.among-terminal.published-selection-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    let terminal = ctx
        .state
        .borrow()
        .pane(PaneId(1))
        .unwrap()
        .terminal()
        .clone();
    let pty = terminal.pty().unwrap();
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

    // Use the production manager action, network client and installer. No test
    // TLS roots, archive substitution, receipt injection or executable build.
    let view = inspect(&preferences, &revision);
    for (_, _, _, expected) in &rows {
        let row = selected_row(&view, expected);
        let install = button(&row, "Install disabled");
        assert!(install.grab_focus());
        visible_description(&row, expected);
        assert!(install.is_sensitive());
        println!("published_selection_row={expected}");
    }
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
    assert!(!root.join("installed.json").exists());
    let (_, clock_version, clock_artifact, clock_description) =
        rows.iter().find(|(id, ..)| id == "clock").unwrap().clone();
    let clock_asset = snapshot.asset(&clock_artifact.asset).unwrap();
    let clock = selected_row(&view, &clock_description);
    assert!(button(&clock, "Install disabled").grab_focus());
    screenshot(&view, "published-native-selection");
    press(&button(&clock, "Install disabled"));
    wait_for(
        || {
            has_label(
                clock.upcast_ref(),
                "Installed github:1431055733/clock disabled. Enable separately in Your plugins.",
            ) || has_label(clock.upcast_ref(), "Package installation failed")
        },
        Duration::from_secs(65),
    );
    let installed = crate::plugin::inventory::read(&root).unwrap();
    assert_eq!(
        installed.len(),
        1,
        "the one explicit Install action must select only clock"
    );
    let entry = &installed[0];
    let receipt = &entry.receipt;
    let catalogue_asset = snapshot.asset("among-plugins.json").unwrap();
    assert_eq!(receipt.repository_id, 1431055733);
    assert_eq!(receipt.release_id, snapshot.id);
    assert_eq!(receipt.asset_id, clock_asset.id);
    assert_eq!(receipt.revision, revision);
    assert_eq!(receipt.package_sha256, clock_artifact.sha256);
    let source = receipt.source.as_ref().unwrap();
    assert_eq!(source.publisher_id, 339567100);
    assert_eq!(source.catalogue_revision, revision);
    assert_eq!(source.catalogue_sha256, catalogue_asset.sha256);
    assert_eq!(*source.release_snapshot, snapshot);
    assert_eq!(source.release_binding, binding);
    assert_eq!(
        entry.inspected.as_ref().unwrap().manifest.version,
        clock_version
    );
    assert_eq!(ctx.state.borrow().plugins.listing().len(), 1);
    assert!(!ctx.state.borrow().plugins.desired_enabled(KEY));
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
    let config = crate::settings::directory()
        .unwrap()
        .join("managed-plugins.json");
    let saved = std::fs::read(&config).ok();
    if let Some(bytes) = &saved {
        let document: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        assert_ne!(document["plugins"][KEY]["enabled"], true);
        assert!(document["plugins"][KEY].get("review").is_none());
    }
    view.destroy();

    let view = inspect(&preferences, &revision);
    let row = selected_row(&view, &clock_description);
    press(&button(&row, "Apply update…"));
    wait_for(|| view.visible_dialog().is_some(), Duration::from_secs(2));
    let dialog = view
        .visible_dialog()
        .unwrap()
        .downcast::<adw::AlertDialog>()
        .unwrap();
    assert!(dialog.body().starts_with(&format!("{clock_description}\n")));
    assert!(!dialog.is_body_use_markup());
    let body = widgets(dialog.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
        .find(|label| label.text().starts_with(&clock_description))
        .unwrap();
    wait_for(
        || body.is_mapped() && body.width() > 0,
        Duration::from_secs(2),
    );
    screenshot_dialog(dialog.upcast_ref(), "published-native-selection-review");
    press(&button(&dialog, "Cancel"));
    wait_for(|| view.visible_dialog().is_none(), Duration::from_secs(2));
    assert_eq!(
        crate::plugin::inventory::read(&root).unwrap()[0].receipt,
        *receipt
    );
    assert_eq!(std::fs::read(&config).ok(), saved);
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
    view.destroy();
    preferences.destroy();
    focus(&ctx.window);
    apply_focus(&ctx);
    wait_for(|| terminal.has_focus(), Duration::from_secs(2));
    xdo(&[
        "type",
        "--clearmodifiers",
        "--delay",
        "0",
        "printf published-selection-alive > \"$HOME/published-selection-proof\"",
    ]);
    xdo(&["key", "--clearmodifiers", "Return"]);
    let proof = std::path::PathBuf::from(std::env::var_os("HOME").unwrap())
        .join("published-selection-proof");
    wait_for(|| proof.exists(), Duration::from_secs(2));
    assert_eq!(std::fs::read(proof).unwrap(), b"published-selection-alive");
    assert_eq!(terminal.pty().unwrap(), pty);
    ctx.window.destroy();
    println!("PASS native {architecture} public selection rows and review dialog show chosen version/target; explicit keyboard Install verified Release {} asset {}, source/tag/catalogue binding and disabled inventory with no grants/process. Cancel preserved bytes and the same PTY accepted real input. This is not enabled output, update execution, uninstall, source/legal or whole RC acceptance.", snapshot.id, clock_asset.id);
}
