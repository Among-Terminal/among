use super::published_lifecycle::{catalogue_row, inspect, observed, Observed};
use super::*;

const KEY: &str = "github:1431055733/clock";

fn selected_row(view: &adw::PreferencesWindow, id: &str) -> adw::ActionRow {
    catalogue_row(view, &format!("github:1431055733/{id}"))
}

fn visible_version(row: &adw::ActionRow, expected: &str) {
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

    // Published acceptance uses the production client and installer. Candidate
    // runs serve CI-built bytes through the owned HTTPS fixture. Both drive
    // the same compact list and retain the independent source/asset oracle.
    let (view, saved_catalogue) = inspect(&ctx, &preferences, &revision);
    assert_eq!(saved_catalogue.catalogue, catalogue);
    for (id, version, artifact, expected) in &rows {
        let row = selected_row(&view, id);
        let install = button(&row, "Install");
        assert!(install.grab_focus());
        assert_eq!(
            saved_catalogue.select(id, version).unwrap().description(),
            *expected
        );
        visible_version(
            &row,
            &format!("{version} · {}", glib::format_size(artifact.size)),
        );
        assert!(install.is_sensitive());
        println!("published_selection_row={expected}");
    }
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
    assert!(!root.join("installed.json").exists());
    let (_, clock_version, clock_artifact, _) =
        rows.iter().find(|(id, ..)| id == "clock").unwrap().clone();
    let clock_asset = snapshot.asset(&clock_artifact.asset).unwrap();
    let clock = selected_row(&view, "clock");
    assert!(button(&clock, "Install").grab_focus());
    screenshot(&view, "published-native-selection");
    press(&button(&clock, "Install"));
    wait_for(
        || ctx.state.borrow().plugins.managed_details(KEY).is_some(),
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
    assert_eq!(receipt.requires_development(), snapshot.is_prerelease());
    let catalogue_asset = snapshot.asset("among-plugins.json").unwrap();
    let release = receipt.release().unwrap();
    assert_eq!(release.repository_id, 1431055733);
    assert_eq!(release.release_id, snapshot.id);
    assert_eq!(release.asset_id, clock_asset.id);
    assert_eq!(receipt.revision, revision);
    assert_eq!(receipt.package_sha256, clock_artifact.sha256);
    let source = release.source.as_ref().unwrap();
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

    let (view, _) = inspect(&ctx, &preferences, &revision);
    // The identical version has no offered update. Its row offers Uninstall;
    // the plugin page explains that the saved catalogue is already current.
    let installed_row = selected_row(&view, "clock");
    assert!(button(&installed_row, "Uninstall").is_mapped());
    assert!(widgets(installed_row.upcast_ref())
        .iter()
        .all(|widget| widget
            .downcast_ref::<gtk::Button>()
            .is_none_or(
                |button| !button.is_mapped() || button.label().as_deref() != Some("Update")
            )));
    press(&named_button(&installed_row, "plugin-info"));
    wait_for(
        || {
            has_label(
                view.upcast_ref(),
                "This is the newest version the saved list offers.",
            )
        },
        Duration::from_secs(3),
    );
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
    println!("PASS native {architecture} compact selection rows show version/size; the checked snapshot retains the exact target/asset. Explicit keyboard Install verified Release {} asset {}, source/tag/catalogue binding and disabled inventory with no grants/process; the same version offers no Update and leaves saved bytes unchanged. The same PTY accepted real input. Enabled output, update execution, uninstall, source/legal and whole RC acceptance are separate checks.", snapshot.id, clock_asset.id);
}
