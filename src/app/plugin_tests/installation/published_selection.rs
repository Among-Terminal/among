use super::*;
use crate::plugin::install::metadata::{Architecture, Platform};

const CATALOGUE: &str = "86f784e6a0eb794cf070edb676207b7964b8317c";
const CATALOGUE_SHA256: &str = "510bdef3b532ed266d10fd925bf15a0d7d536c1889f2454e8d9a7cb446cf8a5b";
const SOURCE: &str = "aa1ec272d4ed6326338f3de0e5fbf30bbe181e39";
const TAG_OBJECT: &str = "31b3ad165bb6a685dfe49fcde082949e12e5887b";
const VERSION: &str = "0.1.0-rc.1";
const KEY: &str = "github:1431055733/clock";

fn description(id: &str, architecture: &str, asset: u64, size: u64) -> String {
    format!("github:1431055733/{id} · version {VERSION} · linux/{architecture} · asset {asset} · {size} bytes")
}

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

#[test]
#[ignore = "requires an isolated native display, keyboard input, ImageMagick and the pinned public GitHub prerelease"]
fn published_native_selection_is_shown_installed_disabled_and_reviewed_without_implicit_access() {
    let (architecture, assets) = match Platform::current().unwrap().architecture {
        Architecture::Amd64 => (
            "amd64",
            [
                ("clock", 678182662, 728319),
                ("system-status", 678182661, 686717),
                ("pane-pwd", 678182660, 683844),
            ],
        ),
        Architecture::Arm64 => (
            "arm64",
            [
                ("clock", 678182659, 712926),
                ("system-status", 678182654, 671723),
                ("pane-pwd", 678182754, 666864),
            ],
        ),
    };
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
    let inspect = || {
        focus(&preferences);
        press(&button(&preferences, "Inspect repository…"));
        let view = inspector();
        focus(&view);
        let address = widgets(view.upcast_ref())
            .into_iter()
            .find_map(|widget| widget.downcast::<adw::EntryRow>().ok())
            .unwrap();
        assert!(address.grab_focus());
        xdo(&[
            "type",
            "--clearmodifiers",
            "--delay",
            "0",
            "Among-Terminal/among-plugins",
        ]);
        press(&button(&view, "Inspect"));
        wait_for(
            || {
                has_label(view.upcast_ref(), "Metadata preview ready")
                    || has_label(view.upcast_ref(), "No metadata preview")
            },
            Duration::from_secs(65),
        );
        assert!(
            has_label(view.upcast_ref(), "Metadata preview ready"),
            "public inspection failed; no synthetic fallback is permitted"
        );
        let output = widgets(view.upcast_ref())
            .into_iter()
            .find_map(|widget| widget.downcast::<gtk::TextView>().ok())
            .unwrap()
            .buffer();
        let preview = output.text(&output.start_iter(), &output.end_iter(), false);
        assert!(
            preview.contains(&format!("at {CATALOGUE}")),
            "this acceptance case is pinned, not an unresolved latest consumer"
        );
        assert!(preview.contains(SOURCE));
        view
    };
    let view = inspect();
    for (id, asset, size) in assets {
        let expected = description(id, architecture, asset, size);
        let row = selected_row(&view, &expected);
        let install = button(&row, "Install disabled");
        assert!(install.grab_focus());
        visible_description(&row, &expected);
        assert!(install.is_sensitive());
        println!("published_selection_row={expected}");
    }
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
    assert!(!root.join("installed.json").exists());
    let clock_description = description("clock", architecture, assets[0].1, assets[0].2);
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
    assert_eq!(receipt.repository_id, 1431055733);
    assert_eq!(receipt.release_id, 424255611);
    assert_eq!(receipt.asset_id, assets[0].1);
    assert_eq!(receipt.revision, SOURCE);
    let expected_package = match architecture {
        "amd64" => "82ea5a94715d8c98f0977eaccbd21b33020e4b46c7b890c9facd79071c50e011",
        "arm64" => "e4722f7d2a3ab593d2d03c9867b6faf28062dbdc9be099686f2089f4446c7860",
        _ => unreachable!(),
    };
    assert_eq!(receipt.package_sha256, expected_package);
    let source = receipt.source.as_ref().unwrap();
    assert_eq!(source.publisher_id, 339567100);
    assert_eq!(source.catalogue_revision, CATALOGUE);
    assert_eq!(source.catalogue_sha256, CATALOGUE_SHA256);
    let binding = source.release_binding.as_ref().unwrap();
    assert_eq!(binding.tag, "v0.1.0-rc.1");
    assert_eq!(binding.object, TAG_OBJECT);
    assert_eq!(binding.commit, SOURCE);
    assert_eq!(entry.inspected.as_ref().unwrap().manifest.version, VERSION);
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

    let view = inspect();
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
    println!("PASS native {architecture} public selection rows and review dialog show chosen version/target; explicit keyboard Install verified Release 424255611 asset {}, source/tag/catalogue binding and disabled inventory with no grants/process. Cancel preserved bytes and the same PTY accepted real input. This is not enabled output, update execution, uninstall, source/legal acceptance or whole RC acceptance.", assets[0].1);
}
