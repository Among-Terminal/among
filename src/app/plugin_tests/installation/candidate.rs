//! Acceptance over CI-built candidate packages while no release is published.
//! A controlled HTTPS server answers exactly as GitHub would, from the prepared
//! inputs: the real UI, client, installer and packages run, and only the
//! release documents are synthetic. The bytes are CI builds, not the approved
//! preparation, so this is candidate acceptance.

use super::published_lifecycle::{observed, Observed};
use super::*;
use crate::prefs::repositories::{present_prerelease_for_test, present_updates_for_test};
use serde_json::{json, Value};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

const REPOSITORY: u64 = 1431055733;
const PRERELEASE: u64 = 999_000;
const PRERELEASE_TAG: &str = "v0.2.0-rc.1";

fn document(inputs: &std::path::Path, name: &str) -> Value {
    crate::plugin::data::parse_unique(&std::fs::read(inputs.join(name)).unwrap(), 1024 * 1024)
        .unwrap()
}

/// The same assets as the stable document, published as the prerelease of the
/// same commit, the way one preparation becomes both releases.
fn prerelease_document(latest: &Value) -> Value {
    let mut prerelease = latest.clone();
    prerelease["id"] = json!(PRERELEASE);
    prerelease["tag_name"] = json!(PRERELEASE_TAG);
    prerelease["prerelease"] = json!(true);
    prerelease
}

fn candidate_server(observed: &Observed) -> Rc<Server> {
    let root = observed.inputs_root.clone();
    let latest = document(&root, "latest.json");
    let prerelease = prerelease_document(&latest);
    let repository = document(&root, "repository.json");
    let commit = document(&root, "initial-commit.json");
    let commit_sha = commit["sha"].as_str().unwrap().to_owned();
    // Only this architecture's packages and the catalogue are prepared; any
    // other asset request is a failure of the test.
    let mut assets = std::collections::BTreeMap::new();
    for plugin in observed.inputs["plugins"].as_array().unwrap() {
        assets.insert(
            plugin["asset_id"].as_u64().unwrap(),
            root.join(format!("{}.tar.gz", plugin["id"].as_str().unwrap())),
        );
    }
    let catalogue = latest["assets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|asset| asset["name"] == "among-plugins.json")
        .unwrap()["id"]
        .as_u64()
        .unwrap();
    assets.insert(catalogue, root.join("among-plugins.json"));
    let base = format!("/repositories/{REPOSITORY}");
    Rc::new(Server::new(move |request| {
        let path = request.split_whitespace().nth(1).unwrap();
        let tail = path.strip_prefix(base.as_str());
        match tail {
            Some("") => Reply::json(repository.clone()),
            Some("/releases/latest") => Reply::json(latest.clone()),
            Some("/releases?per_page=100&page=1") => Reply::json(json!([prerelease.clone()])),
            Some(tail) if tail == format!("/releases/{PRERELEASE}") => {
                Reply::json(prerelease.clone())
            }
            Some(tail) if tail.starts_with("/releases/assets/") => {
                let id = tail.rsplit('/').next().unwrap().parse::<u64>().unwrap();
                Reply::bytes(std::fs::read(&assets[&id]).unwrap())
            }
            Some(tail) if tail.starts_with("/git/ref/tags/") => {
                let tag = tail.strip_prefix("/git/ref/tags/").unwrap();
                Reply::json(json!({"ref":format!("refs/tags/{tag}"),
                    "object":{"type":"commit","sha":commit_sha}}))
            }
            Some(tail) if tail == format!("/git/commits/{commit_sha}") => {
                Reply::json(commit.clone())
            }
            None if path == "/repos/Among-Terminal/among-plugins" => {
                Reply::json(repository.clone())
            }
            _ => panic!("unexpected candidate request: {path}"),
        }
    }))
}

fn changed(ctx: &Window) -> Rc<dyn Fn()> {
    let state = ctx.state.clone();
    Rc::new(move || reconcile_managed_installations(&state))
}

fn details(ctx: &Window) -> Rc<crate::prefs::PluginDetailsQuery> {
    let state = ctx.state.clone();
    Rc::new(move |key: &str| state.borrow().plugins.managed_details(key))
}

/// Whether the prepared inputs are a candidate, and the inspector for them.
/// Published inputs return false: the caller uses the production action.
pub(super) fn present_inspector(ctx: &Window, preferences: &adw::PreferencesWindow) -> bool {
    let observed = observed();
    if observed.inputs["candidate"] != true {
        return false;
    }
    let server = candidate_server(&observed);
    let root = crate::plugin::inventory::root().unwrap();
    present_updates_for_test(
        preferences.upcast_ref(),
        Rc::new({
            let server = server.clone();
            move |repository| server.job(repository)
        }),
        Rc::new(move |selection| server.install(selection, root.clone())),
        changed(ctx),
        details(ctx),
    );
    true
}

fn preferences_page(ctx: &Window) -> adw::PreferencesWindow {
    open_preferences(ctx);
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
    preferences
}

fn clock_row(view: &adw::PreferencesWindow) -> adw::ActionRow {
    widgets(view.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
        .find(|row| {
            row.subtitle()
                .is_some_and(|subtitle| subtitle.starts_with("github:1431055733/clock · version"))
        })
        .unwrap()
}

fn wait_for_clock_row(view: &adw::PreferencesWindow) {
    wait_for(
        || {
            widgets(view.upcast_ref())
                .into_iter()
                .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
                .any(|row| {
                    row.subtitle().is_some_and(|subtitle| {
                        subtitle.starts_with("github:1431055733/clock · version")
                    })
                })
        },
        Duration::from_secs(20),
    );
}

const KEY: &str = "github:1431055733/clock";
const INSTALLED: &str =
    "Installed github:1431055733/clock disabled. Enable separately in Your plugins.";

fn selected(ctx: &Window) -> crate::plugin::inventory::Receipt {
    ctx.state
        .borrow()
        .plugins
        .managed_details(KEY)
        .unwrap()
        .0
        .receipt
}

/// How far a started development generation is expected to get.
#[derive(Clone, Copy, PartialEq)]
enum Reach {
    /// The plugin answers the handshake and is admitted.
    Admitted,
    /// The host launches the process, which is all the development gate
    /// decides. The official plugins compile their own repository identity
    /// (`github:1431055733/<id>`) into the handshake check and close the pipe
    /// when the host offers `development/<id>`, so a local install of their
    /// bytes is launched but never admitted.
    Launched,
}

/// Grants the package's own requirements as the review would and shows that
/// the host really runs it, which only development mode allows for this
/// generation.
fn starts_under_the_flag(ctx: &Window, key: &str, reach: Reach) {
    let (entry, _) = ctx.state.borrow().plugins.managed_details(key).unwrap();
    let manifest = &entry.inspected.as_ref().unwrap().manifest;
    let review = entry.receipt.review(manifest.required_features.clone());
    ctx.state
        .borrow_mut()
        .plugins
        .review_access(key, review)
        .unwrap();
    ctx.state.borrow_mut().plugins.enable(key);
    // A supervised child only shows that a process was spawned; Running is
    // reached after the plugin has answered the handshake and been admitted.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        while glib::MainContext::default().pending() {
            glib::MainContext::default().iteration(false);
        }
        let listing = ctx.state.borrow().plugins.listing();
        let state = listing
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, state)| state);
        match (reach, state) {
            (Reach::Admitted, Some(PluginState::Running)) => break,
            (Reach::Launched, Some(PluginState::Running | PluginState::Faulted { .. })) => break,
            // Refused by the development gate: the process never started.
            (_, Some(PluginState::Unavailable { reason })) => {
                panic!("{key} was not launched: {reason}")
            }
            _ => {}
        }
        // The state names the reason a started plugin was not admitted.
        assert!(
            std::time::Instant::now() < deadline,
            "{key} did not reach {}; host states: {listing:?}",
            if reach == Reach::Admitted {
                "Running"
            } else {
                "a launched state"
            }
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    ctx.state.borrow_mut().plugins.disable(key);
    wait_for(
        || ctx.state.borrow().plugins.supervised_count() == 0,
        Duration::from_secs(10),
    );
}

#[test]
#[ignore = "requires check-development-install: an owned display, prepared candidate inputs and its own process"]
fn candidate_prerelease_replaces_an_install_and_a_stable_release_of_its_bytes_rebinds_it() {
    assert!(crate::plugin::development::fix(true));
    let observed = observed();
    assert_eq!(observed.inputs["candidate"], true);
    let server = candidate_server(&observed);
    let root = crate::plugin::inventory::root().unwrap();
    let registry = crate::plugin::install::registry::root().unwrap();
    let app = application("com.github.among-terminal.candidate-prerelease-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    let preferences = preferences_page(&ctx);
    present_prerelease_for_test(
        preferences.upcast_ref(),
        Rc::new({
            let server = server.clone();
            move |id| server.refresh(id)
        }),
        Rc::new({
            let server = server.clone();
            move |id| server.refresh_prerelease(id)
        }),
        Rc::new({
            let server = server.clone();
            let root = root.clone();
            move |selection| server.install(selection, root.clone())
        }),
        changed(&ctx),
        registry,
        details(&ctx),
    );
    let view = inspector();
    focus(&view);

    // An ordinary install first: Refresh shows the stable candidate.
    press(&button(&view, "Refresh"));
    wait_for_clock_row(&view);
    press(&button(&clock_row(&view), "Install disabled"));
    wait_for(
        || has_label(view.upcast_ref(), INSTALLED),
        Duration::from_secs(30),
    );
    wait_for(
        || ctx.state.borrow().plugins.managed_details(KEY).is_some(),
        Duration::from_secs(5),
    );
    let ordinary = selected(&ctx);
    assert!(!ordinary.requires_development());
    ctx.state
        .borrow_mut()
        .settings
        .managed_review_for_installation(
            KEY,
            ordinary.review(vec!["status".into()]),
            ordinary.installation.as_deref(),
        )
        .unwrap();
    ctx.state
        .borrow_mut()
        .settings
        .plugin_changed(KEY, false)
        .unwrap();

    // The prerelease replaces it only through its confirmation.
    press(&button(&view, "Use latest prerelease"));
    wait_for(
        || has_label(view.upcast_ref(), "Newest prerelease ready for review"),
        Duration::from_secs(20),
    );
    wait_for_clock_row(&view);
    press(&button(&clock_row(&view), "Replace with prerelease…"));
    wait_for(|| view.visible_dialog().is_some(), Duration::from_secs(2));
    press(&button(&view.visible_dialog().unwrap(), "Replace"));
    wait_for(
        || selected(&ctx).requires_development(),
        Duration::from_secs(30),
    );
    let replaced = selected(&ctx);
    assert_ne!(replaced.installation, ordinary.installation);
    assert!(!replaced.matches_review(&ordinary.review(vec!["status".into()])));
    assert!(!ctx.state.borrow().plugins.desired_enabled(KEY));
    // The replaced approval must not wait in the settings for the stable
    // update of this generation, which keeps its token.
    let config = crate::settings::directory()
        .unwrap()
        .join("managed-plugins.json");
    let saved: Value = serde_json::from_slice(&std::fs::read(config).unwrap()).unwrap();
    assert!(saved["plugins"][KEY].get("review").is_none());
    starts_under_the_flag(&ctx, KEY, Reach::Admitted);

    // The stable release of the same bytes and commit rebinds it.
    press(&button(&view, "Refresh"));
    wait_for(
        || {
            widgets(view.upcast_ref())
                .into_iter()
                .filter_map(|widget| widget.downcast::<gtk::Button>().ok())
                .any(|button| button.label().as_deref() == Some("Apply update…"))
        },
        Duration::from_secs(20),
    );
    press(&button(&clock_row(&view), "Apply update…"));
    wait_for(|| view.visible_dialog().is_some(), Duration::from_secs(2));
    press(&button(&view.visible_dialog().unwrap(), "Apply update"));
    wait_for(
        || !selected(&ctx).requires_development(),
        Duration::from_secs(30),
    );
    let rebound = selected(&ctx);
    assert_eq!(rebound.package_sha256, replaced.package_sha256);
    assert_eq!(rebound.installation, replaced.installation);
    assert_eq!(
        rebound
            .release()
            .unwrap()
            .source
            .as_ref()
            .unwrap()
            .release_binding
            .commit,
        replaced.revision
    );
    view.destroy();
    preferences.destroy();
    ctx.window.destroy();
    println!("PASS development_prerelease replace_rebind=true");
}

#[test]
#[ignore = "requires check-development-install: an owned display, a CI-built catalogue and its own process"]
fn candidate_packages_install_from_a_local_catalogue_and_start_under_the_flag() {
    assert!(crate::plugin::development::fix(true));
    let assets = std::path::PathBuf::from(
        std::env::var_os("AMONG_LOCAL_PLUGIN_CATALOGUE").expect("the runner names the catalogue"),
    );
    // The package files stay exactly as built; only the directory is the
    // author's own, not writable by others.
    let author = crate::test_isolation::check_root().join("candidate-catalogue");
    std::fs::DirBuilder::new()
        .mode(0o755)
        .create(&author)
        .unwrap();
    for entry in std::fs::read_dir(&assets).unwrap() {
        let entry = entry.unwrap();
        let target = author.join(entry.file_name());
        std::fs::copy(entry.path(), &target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let root = crate::plugin::inventory::root().unwrap();
    let app = application("com.github.among-terminal.candidate-local-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    let preferences = preferences_page(&ctx);
    crate::prefs::local_catalogue::present_path_for_test(
        preferences.upcast_ref(),
        &author.join("among-plugins.json"),
        changed(&ctx),
        details(&ctx),
    );
    let view = super::local_catalogue::local_window();
    focus(&view);
    let row = widgets(view.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
        .find(|row| {
            row.subtitle()
                .is_some_and(|subtitle| subtitle.starts_with("development/clock"))
        })
        .unwrap();
    press(&button(&row, "Review…"));
    wait_for(|| view.visible_dialog().is_some(), Duration::from_secs(2));
    press(&button(&view.visible_dialog().unwrap(), "Install"));
    wait_for(
        || {
            ctx.state
                .borrow()
                .plugins
                .managed_details("development/clock")
                .is_some()
        },
        Duration::from_secs(30),
    );
    let entries = crate::plugin::inventory::read(&root).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].receipt.local().is_some());
    assert!(entries[0].inspected.is_ok());
    starts_under_the_flag(&ctx, "development/clock", Reach::Launched);
    view.destroy();
    preferences.destroy();
    ctx.window.destroy();
    println!("PASS development_local_catalogue install_start=true");
}
