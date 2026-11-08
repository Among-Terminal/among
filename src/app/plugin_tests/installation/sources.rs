use super::*;
use crate::plugin::install::{
    listing::tests::{digest, document},
    source::tests::package_route_at,
};

fn store_source(root: &std::path::Path, inspection: crate::plugin::install::source::Inspection) {
    use crate::plugin::install::registry::{self, Request};
    let job = registry::Job::start(root.to_path_buf(), Request::Add(Box::new(inspection))).unwrap();
    let result = RefCell::new(None);
    wait_for(
        || {
            let outcome = job.poll();
            if outcome.is_some() {
                *result.borrow_mut() = outcome;
                true
            } else {
                false
            }
        },
        Duration::from_secs(5),
    );
    result.into_inner().unwrap().unwrap();
}

fn row(view: &adw::PreferencesWindow, identity: &str) -> adw::ActionRow {
    widgets(view.upcast_ref())
        .into_iter()
        .find_map(|widget| {
            widget
                .downcast::<adw::ActionRow>()
                .ok()
                .filter(|row| row.widget_name() == identity)
        })
        .unwrap()
}

fn shows_version(row: &adw::ActionRow, version: &str) -> bool {
    widgets(row.upcast_ref()).iter().any(|widget| {
        widget
            .downcast_ref::<gtk::Label>()
            .is_some_and(|label| label.text().starts_with(&format!("{version} · ")))
    })
}

fn catalogue_row_is_mapped(view: &adw::PreferencesWindow, identity: &str) -> bool {
    widgets(view.upcast_ref()).iter().any(|widget| {
        widget
            .downcast_ref::<adw::ActionRow>()
            .is_some_and(|row| row.widget_name() == identity && row.is_mapped())
    })
}

#[track_caller]
fn assert_source_reference(
    view: &adw::PreferencesWindow,
    identity: &str,
    expected: &str,
    phase: &str,
) {
    let installed = row(view, identity);
    let labels: Vec<_> = widgets(installed.upcast_ref())
        .iter()
        .filter_map(|widget| {
            widget.downcast_ref::<gtk::Label>().map(|label| {
                (
                    label.text().to_string(),
                    label.is_visible(),
                    label.is_mapped(),
                )
            })
        })
        .collect();
    eprintln!("source_reference phase={phase} identity={identity} row_visible={} row_mapped={} labels={labels:?}", installed.is_visible(), installed.is_mapped());
    let expected_mapped = labels
        .iter()
        .any(|(text, _, mapped)| text == expected && *mapped);
    let stale_removed = expected != "source removed"
        && labels
            .iter()
            .any(|(text, _, mapped)| text == "source removed" && *mapped);
    if !expected_mapped || stale_removed {
        if view.is_mapped() {
            screenshot(view, &format!("source-reference-{phase}-failure"));
        } else {
            let path = crate::test_isolation::check_root()
                .join(format!("source-reference-{phase}-unmapped.png"));
            eprintln!(
                "source_reference window was unmapped; owned-display screenshot result={:?}",
                std::process::Command::new("import")
                    .args(["-window", "root"])
                    .arg(path)
                    .status()
            );
        }
        panic!("source reference for {identity} at {phase}: expected mapped {expected:?}, labels={labels:?}");
    }
}

#[test]
#[ignore = "requires an isolated display, real keyboard input and owned TLS fixtures"]
fn adding_and_removing_a_source_updates_the_open_plugin_list_without_changing_installs() {
    const KEY: &str = "github:12/sample";
    let root = crate::plugin::inventory::root().unwrap();
    crate::test_isolation::assert_below_check_root(&root);
    crate::plugin::inventory::tests::fixture_version(&root, "1.0.0");
    let before = std::fs::read(root.join("installed.json")).unwrap();
    let metadata = document(&[
        ("sample", "Sample", "1.1.0", &digest('b')),
        ("notes", "Notes", "1.0.0", &digest('b')),
    ]);
    let server = Rc::new(Server::new(move |request| {
        package_route_at(metadata.clone(), Vec::new(), &"a".repeat(40), request)
    }));
    let app = application("com.github.among-terminal.source-list-reconciliation-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    let settings = crate::settings::directory()
        .unwrap()
        .join("managed-plugins.json");
    let read_settings = || match std::fs::read(&settings) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("cannot inspect managed plugin settings: {error}"),
    };
    let settings_before = read_settings();
    let page = crate::prefs::plugins_page_with_source_lookup_for_test(
        &preferences_context(&ctx),
        crate::prefs::PluginServices {
            root: crate::plugin::install::registry::root(),
            refresh: Rc::new(|_| Err(crate::plugin::data::Problem("refresh was not requested"))),
            prerelease: Rc::new(|_| {
                Err(crate::plugin::data::Problem("prerelease was not requested"))
            }),
            install: Rc::new(|_| panic!("source registration must not install packages")),
        },
        Rc::new({
            let server = server.clone();
            move |repository| server.job(repository)
        }),
    );
    let view = adw::PreferencesWindow::builder()
        .transient_for(&ctx.window)
        .title("Source reconciliation")
        .search_enabled(false)
        .default_width(760)
        .default_height(700)
        .build();
    view.add(&page);
    view.present();
    focus(&view);
    wait_for(
        || has_label(view.upcast_ref(), "source removed"),
        Duration::from_secs(5),
    );
    assert!(server.requests().is_empty());
    assert_source_reference(&view, KEY, "source removed", "before-add");
    let enabled = ctx.state.borrow().plugins.desired_enabled(KEY);
    press(&named_button(&view, "plugins-add-source"));
    wait_for(|| view.visible_dialog().is_some(), Duration::from_secs(3));
    let dialog = view.visible_dialog().unwrap();
    let entry = widgets(dialog.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<adw::EntryRow>().ok())
        .unwrap();
    wait_for(|| entry.is_sensitive(), Duration::from_secs(3));
    assert!(entry.grab_focus());
    xdo(&["type", "--clearmodifiers", "--delay", "0", "example/sample"]);
    press(&named_button(&dialog, "source-lookup"));
    let add = named_button(&dialog, "source-add");
    wait_for(|| add.is_sensitive(), Duration::from_secs(6));
    eprintln!(
        "source_preview raw_notes={} mapped_catalogue_notes={}",
        has_list_row(&view, "notes"),
        catalogue_row_is_mapped(&view, "github:12/notes")
    );
    press(&add);
    wait_for(|| view.visible_dialog().is_none(), Duration::from_secs(5));
    // The closing Add dialog also contains a Notes preview row. Only the
    // identity-scoped row in Available proves the registry reload reached
    // this manager; a title-only search can match that old preview early.
    wait_for(
        || catalogue_row_is_mapped(&view, "github:12/notes"),
        Duration::from_secs(5),
    );
    assert_source_reference(&view, KEY, "example/sample", "after-add");
    assert_eq!(std::fs::read(root.join("installed.json")).unwrap(), before);
    assert_eq!(ctx.state.borrow().plugins.desired_enabled(KEY), enabled);
    assert_eq!(
        read_settings(),
        settings_before,
        "adding a source changed managed plugin settings"
    );
    screenshot(&view, "source-added-list-updated");
    let sources = widgets(view.upcast_ref())
        .into_iter()
        .find_map(|widget| {
            widget
                .downcast::<adw::ActionRow>()
                .ok()
                .filter(|row| row.widget_name() == "plugins-sources")
        })
        .unwrap();
    assert!(sources.grab_focus());
    xdo(&["key", "--clearmodifiers", "Return"]);
    wait_for(
        || {
            widgets(view.upcast_ref())
                .iter()
                .any(|widget| widget.widget_name() == "source-remove:12" && widget.is_mapped())
        },
        Duration::from_secs(5),
    );
    press(&named_button(&view, "source-remove:12"));
    wait_for(|| view.visible_dialog().is_some(), Duration::from_secs(3));
    press(&button(&view.visible_dialog().unwrap(), "Remove source"));
    wait_for(
        || {
            !widgets(view.upcast_ref())
                .iter()
                .any(|widget| widget.widget_name() == "source-remove:12")
        },
        Duration::from_secs(5),
    );
    xdo(&["key", "--clearmodifiers", "Escape"]);
    wait_for(
        || {
            !widgets(view.upcast_ref())
                .iter()
                .any(|widget| widget.widget_name() == "plugin-sources-page" && widget.is_mapped())
        },
        Duration::from_secs(3),
    );
    assert!(
        view.is_mapped(),
        "Escape from Sources closed Preferences instead of returning to the plugin list"
    );
    wait_for(
        || {
            !catalogue_row_is_mapped(&view, "github:12/notes")
                && has_label(row(&view, KEY).upcast_ref(), "source removed")
        },
        Duration::from_secs(5),
    );
    assert_source_reference(&view, KEY, "source removed", "after-remove");
    assert_eq!(
        server.requests().len(),
        5,
        "removal or reopening used the network"
    );
    assert_eq!(std::fs::read(root.join("installed.json")).unwrap(), before);
    assert_eq!(ctx.state.borrow().plugins.desired_enabled(KEY), enabled);
    assert_eq!(
        read_settings(),
        settings_before,
        "removing a source changed managed plugin settings"
    );
    screenshot(&view, "source-removed-list-updated");
    view.destroy();
    ctx.window.destroy();
    println!("PASS Add/Remove reconciles the already open Plugins list from local snapshots; installed bytes, settings and desired enablement stay unchanged");
}

#[test]
#[ignore = "requires an isolated display, keyboard input and owned multi-source prerelease HTTPS fixtures"]
fn several_sources_keep_distinct_session_only_prerelease_catalogues_in_the_selected_channel() {
    use crate::plugin::install::{
        listing::tests::inspection, registry, source::tests::release_document,
    };
    assert!(crate::plugin::development::fix(true));
    let sources = registry::root().unwrap();
    for id in [12, 13] {
        store_source(
            &sources,
            inspection(
                crate::plugin::origin::Origin {
                    repository_id: id,
                    publisher_id: 34,
                    owner: "example".into(),
                    repository: format!("plugins-{id}"),
                },
                &[("notes", "Notes", "1.0.0", &digest('b'))],
            ),
        );
    }
    let before = std::fs::read(sources.join("sources.json")).unwrap();
    let server = Rc::new(Server::new(|request| {
        let path = request.split_whitespace().nth(1).unwrap();
        let id = path.split('/').nth(2).unwrap().parse::<u64>().unwrap();
        assert!([12, 13].contains(&id));
        if path == format!("/repositories/{id}") {
            return Reply::json(
                serde_json::json!({"id":id,"owner":{"login":"example","id":34},"name":format!("plugins-{id}"),"private":false,"default_branch":"main"}),
            );
        }
        let metadata = document(&[(
            "notes",
            "Notes",
            if id == 12 { "2.0.0" } else { "3.0.0" },
            &digest('b'),
        )]);
        let mut release = release_document(&metadata);
        release["prerelease"] = serde_json::json!(true);
        release["created_at"] = serde_json::json!("2026-11-07T00:00:00Z");
        if path.ends_with("/releases?per_page=100&page=1") {
            return Reply::json(serde_json::json!([release]));
        }
        if path.ends_with("/releases/7") {
            return Reply::json(release);
        }
        let normalized = request.replacen(&format!("/repositories/{id}/"), "/repositories/12/", 1);
        package_route_at(metadata, Vec::new(), &"a".repeat(40), &normalized)
    }));
    let app = application("com.github.among-terminal.multiple-prerelease-sources-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    let services = crate::prefs::PluginServices {
        root: Some(sources.clone()),
        refresh: Rc::new(|_| panic!("Pre-release selection used the stable client")),
        prerelease: Rc::new({
            let server = server.clone();
            move |id| server.refresh_prerelease(id)
        }),
        install: Rc::new(|_| panic!("checking a source must not install a plugin")),
    };
    let preferences = {
        open_preferences(&ctx);
        gtk::Window::list_toplevels()
            .into_iter()
            .find_map(|widget| widget.downcast::<adw::PreferencesWindow>().ok())
            .unwrap()
    };
    let view = list_window(&ctx, &preferences, services.clone());
    focus(&view);
    wait_for(|| has_list_row(&view, "notes"), Duration::from_secs(5));
    assert!(server.requests().is_empty());
    assert!(channel_active(&view, "Pre-release"));
    assert!(shows_version(&row(&view, "github:12/notes"), "1.0.0"));
    assert!(shows_version(&row(&view, "github:13/notes"), "1.0.0"));
    let sources_row = widgets(view.upcast_ref())
        .into_iter()
        .find_map(|widget| {
            widget
                .downcast::<adw::ActionRow>()
                .ok()
                .filter(|row| row.widget_name() == "plugins-sources")
        })
        .unwrap();
    assert!(sources_row.grab_focus());
    xdo(&["key", "--clearmodifiers", "Return"]);
    wait_for(
        || {
            widgets(view.upcast_ref())
                .iter()
                .any(|widget| widget.widget_name() == "source-check:12" && widget.is_mapped())
        },
        Duration::from_secs(5),
    );
    for id in [12, 13] {
        let check = named_button(&view, &format!("source-check:{id}"));
        wait_for(|| check.is_sensitive(), Duration::from_secs(5));
        press(&check);
        let last = RefCell::new(None::<String>);
        let diagnostic_at = std::time::Instant::now() + Duration::from_secs(7);
        let photographed = Cell::new(false);
        wait_for(
            || {
                // A source's state is the final line of its multiline
                // subtitle, not a standalone label. Scope the observation
                // to this source so a preceding successful check cannot
                // satisfy the next source's assertion.
                let status = widgets(view.upcast_ref())
                    .into_iter()
                    .find_map(|widget| {
                        widget
                            .downcast::<adw::ActionRow>()
                            .ok()
                            .filter(|row| row.widget_name() == format!("plugin-source:{id}"))
                            .and_then(|row| row.subtitle().map(|text| text.to_string()))
                    })
                    .unwrap_or_default();
                if last.borrow().as_ref() != Some(&status) {
                    eprintln!(
                        "source_prerelease repository={id} subtitle={status:?} paths={:?}",
                        server
                            .requests()
                            .iter()
                            .map(|request| request.split_whitespace().nth(1).unwrap())
                            .collect::<Vec<_>>()
                    );
                    *last.borrow_mut() = Some(status.clone());
                }
                if status.ends_with("\nPre-release list checked; not saved") {
                    return true;
                }
                let refused = status.contains("Could not check:")
                    || status.contains("Rate limited")
                    || status.contains("Publisher changed.");
                if (refused || std::time::Instant::now() >= diagnostic_at)
                    && !photographed.replace(true)
                {
                    screenshot(&view, &format!("source-prerelease-{id}-failure"));
                }
                assert!(
                    !refused,
                    "source {id} refused its genuine prerelease observation: {status}"
                );
                false
            },
            Duration::from_secs(8),
        );
        wait_for(
            || named_button(&view, "sources-check-all").is_sensitive(),
            Duration::from_secs(5),
        );
    }
    xdo(&["key", "--clearmodifiers", "Escape"]);
    wait_for(
        || {
            shows_version(&row(&view, "github:12/notes"), "2.0.0")
                && shows_version(&row(&view, "github:13/notes"), "3.0.0")
        },
        Duration::from_secs(5),
    );
    assert_eq!(
        std::fs::read(sources.join("sources.json")).unwrap(),
        before,
        "prerelease catalogues leaked into persistent source metadata"
    );
    assert!(
        crate::plugin::inventory::read(&crate::plugin::inventory::root().unwrap())
            .unwrap()
            .is_empty()
    );
    let requests = server.requests();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.contains("/releases?per_page=100&page=1 "))
            .count(),
        2
    );
    assert!(requests
        .iter()
        .all(|request| !request.contains("/releases/latest")));
    screenshot(&view, "multiple-prerelease-source-overlays");
    view.destroy();
    let reopened = list_window(&ctx, &preferences, services);
    focus(&reopened);
    wait_for(|| has_list_row(&reopened, "notes"), Duration::from_secs(5));
    assert!(shows_version(&row(&reopened, "github:12/notes"), "1.0.0"));
    assert!(shows_version(&row(&reopened, "github:13/notes"), "1.0.0"));
    assert_eq!(
        server.requests().len(),
        requests.len(),
        "reopening the list requested prerelease data"
    );
    reopened.destroy();
    preferences.destroy();
    ctx.window.destroy();
    println!("PASS selected Pre-release checks preserve two source-specific overlays, never save prereleases and reopen offline from the unchanged stable snapshots");
}
