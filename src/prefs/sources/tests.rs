use super::*;
use crate::plugin::install::{
    listing::tests::{digest, inspection},
    source::tests::{normal, Reply, Server},
    TestOwnedRoot,
};
use std::time::{Duration, Instant};

fn pump() {
    glib::MainContext::default().iteration(false);
    std::thread::sleep(Duration::from_millis(3));
}

#[track_caller]
fn wait(predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !predicate() {
        assert!(Instant::now() < deadline, "source UI condition timed out");
        pump();
    }
}

fn widgets(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut found = vec![widget.clone()];
    let mut child = widget.first_child();
    while let Some(widget) = child {
        found.extend(widgets(&widget));
        child = widget.next_sibling();
    }
    found
}

fn named<T: IsA<gtk::Widget>>(root: &impl IsA<gtk::Widget>, name: &str) -> T {
    widgets(root.as_ref())
        .into_iter()
        .find(|widget| widget.widget_name() == name)
        .unwrap_or_else(|| panic!("missing source control {name}"))
        .downcast::<T>()
        .ok()
        .unwrap()
}

fn says(root: &impl IsA<gtk::Widget>, text: &str) -> bool {
    widgets(root.as_ref()).iter().any(|widget| {
        widget
            .downcast_ref::<gtk::Label>()
            .is_some_and(|label| label.text().contains(text))
    })
}

fn xdo(args: &[&str]) -> String {
    let output = std::process::Command::new("xdotool")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "xdotool failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn window() -> adw::PreferencesWindow {
    adw::init().unwrap();
    let window = adw::PreferencesWindow::builder()
        .title("Sources native acceptance")
        .default_width(760)
        .default_height(700)
        .search_enabled(false)
        .build();
    let page = adw::PreferencesPage::new();
    page.set_title("Plugins");
    window.add(&page);
    window.present();
    wait(|| window.is_mapped());
    let id = xdo(&[
        "search",
        "--all",
        "--onlyvisible",
        "--pid",
        &std::process::id().to_string(),
        "--name",
        "^Sources native acceptance$",
    ]);
    assert_eq!(
        id.lines().count(),
        1,
        "the source test must focus its own window"
    );
    let id = id.lines().last().unwrap();
    xdo(&["windowactivate", "--sync", id]);
    xdo(&["windowfocus", "--sync", id]);
    wait(|| window.is_active());
    window
}

fn press(button: &gtk::Button) {
    wait(|| button.is_mapped() && button.is_sensitive());
    assert!(button.grab_focus());
    xdo(&["key", "--clearmodifiers", "Return"]);
}

fn respond(dialog: &adw::Dialog, label: &str) {
    let button = widgets(dialog.upcast_ref())
        .into_iter()
        .find_map(|widget| {
            widget
                .downcast::<gtk::Button>()
                .ok()
                .filter(|button| button.label().as_deref() == Some(label))
        })
        .unwrap();
    press(&button);
}

fn screenshot(window: &adw::PreferencesWindow, name: &str) {
    let title = window.title().unwrap();
    assert_eq!(title, "Sources native acceptance");
    let until = Instant::now() + Duration::from_millis(350);
    while Instant::now() < until {
        pump();
    }
    let id = xdo(&[
        "search",
        "--all",
        "--onlyvisible",
        "--pid",
        &std::process::id().to_string(),
        "--name",
        &format!("^{title}$"),
    ]);
    assert_eq!(
        id.lines().count(),
        1,
        "the screenshot must capture the supplied source window"
    );
    let path = crate::test_isolation::check_root().join(format!("{name}.png"));
    assert!(std::process::Command::new("import")
        .args([
            "-window",
            id.lines().last().unwrap(),
            path.to_str().unwrap()
        ])
        .status()
        .unwrap()
        .success());
}

fn storage(root: &std::path::Path, request: Request) -> Result<Outcome, Problem> {
    let job = registry::Job::start(root.to_path_buf(), request).unwrap();
    let result = RefCell::new(None);
    wait(|| {
        let outcome = job.poll();
        if outcome.is_some() {
            *result.borrow_mut() = outcome;
            true
        } else {
            false
        }
    });
    result.into_inner().unwrap()
}

fn source(id: u64) -> Inspection {
    inspection(
        crate::plugin::origin::Origin {
            repository_id: id,
            publisher_id: 34,
            owner: "example".into(),
            repository: format!("plugins-{id}"),
        },
        &[("sample", "Sample", "1.0.0", &digest('b'))],
    )
}

#[test]
fn a_rate_limit_never_invents_the_remaining_count() {
    let source = registry::official_source();
    assert!(description(
        &source,
        Some(&Notice::Limited(RateLimit {
            remaining: None,
            retry_after: None
        })),
        None
    )
    .contains("remaining unknown"));
    assert!(description(
        &source,
        Some(&Notice::Limited(RateLimit {
            remaining: Some(0),
            retry_after: Some(7)
        })),
        None
    )
    .contains("remaining 0 · retry after 7 s"));
}

#[test]
#[ignore = "requires an isolated display, xdotool, ImageMagick and owned TLS fixtures"]
fn source_page_is_cache_only_and_check_all_stops_at_actual_403_and_429_limits() {
    let window = window();
    for (status, secondary) in [(403, false), (429, false), (403, true)] {
        let label = if secondary {
            "403-secondary".into()
        } else {
            status.to_string()
        };
        let root = TestOwnedRoot::new();
        let registry = root.0.join("sources");
        for id in 12..16 {
            storage(&registry, Request::Add(Box::new(source(id)))).unwrap();
        }
        let before = std::fs::read(registry.join("sources.json")).unwrap();
        let permission = Rc::new(Server::new(|_| Reply::refused(403, Some("9"), None)));
        let not_found = Rc::new(Server::new(|_| Reply::refused(404, None, None)));
        let limited = Rc::new(Server::new(move |_| {
            if secondary {
                Reply::forbidden("You have exceeded a secondary rate limit.")
            } else {
                Reply::refused(status, if status == 403 { Some("0") } else { None }, None)
            }
        }));
        let calls = Rc::new(RefCell::new(Vec::new()));
        present(
            &window,
            Services {
                root: Some(registry.clone()),
                lookup: Rc::new(|_| Err(Problem("look-up was not requested"))),
                refresh: Rc::new({
                    let permission = permission.clone();
                    let not_found = not_found.clone();
                    let limited = limited.clone();
                    let calls = calls.clone();
                    move |id| {
                        calls.borrow_mut().push(id);
                        match id {
                            1_431_055_733 => permission.refresh(id),
                            12 => not_found.refresh(id),
                            13 => limited.refresh(id),
                            _ => panic!("Check all passed a rate refusal"),
                        }
                    }
                }),
            },
            Rc::new(|| {}),
            Rc::new(|_| panic!("these refused checks cannot publish a prerelease")),
        );
        let all: gtk::Button = named(&window, "sources-check-all");
        wait(|| all.is_sensitive());
        assert!(
            calls.borrow().is_empty(),
            "opening Sources used the network"
        );
        assert!(widgets(window.upcast_ref())
            .iter()
            .all(|widget| widget.widget_name() != "source-remove:1431055733"));
        assert_eq!(
            std::fs::read(registry.join("sources.json")).unwrap(),
            before
        );
        screenshot(&window, &format!("sources-offline-{label}"));
        press(&all);
        wait(|| says(&window, "Check all stopped at the GitHub rate limit"));
        wait(|| all.is_sensitive());
        assert_eq!(*calls.borrow(), [1_431_055_733, 12, 13]);
        assert_eq!(
            (
                permission.requests().len(),
                not_found.requests().len(),
                limited.requests().len()
            ),
            (1, 1, 1)
        );
        assert!(says(
            &window,
            if status == 403 && !secondary {
                "remaining 0"
            } else {
                "remaining unknown"
            }
        ));
        assert_eq!(
            std::fs::read(registry.join("sources.json")).unwrap(),
            before
        );
        screenshot(&window, &format!("sources-rate-limited-{label}"));
        assert!(window.pop_subpage());
        let until = Instant::now() + Duration::from_millis(350);
        while Instant::now() < until {
            pump();
        }
    }
    window.destroy();
    println!("PASS cache-only Sources, ordinary permission refusal continues, observed 403/429 rate refusal stops the queue and retained lists remain byte-identical");
}

#[test]
#[ignore = "requires an isolated display, xdotool, ImageMagick and owned TLS fixtures"]
fn add_source_lookup_cancel_is_inert_and_explicit_add_saves_without_installing() {
    let window = window();
    let root = TestOwnedRoot::new();
    let sources = root.0.join("sources");
    let installed = root.0.join("managed");
    crate::plugin::inventory::tests::fixture(&installed);
    let before = std::fs::read(installed.join("installed.json")).unwrap();
    let server = Rc::new(Server::new(normal));
    let changes = Rc::new(Cell::new(0));
    let services = Services {
        root: Some(sources.clone()),
        refresh: Rc::new(|_| Err(Problem("refresh was not requested"))),
        lookup: Rc::new({
            let server = server.clone();
            move |repository| server.job(repository)
        }),
    };
    for commit in [false, true] {
        add_dialog(
            window.upcast_ref(),
            services.clone(),
            Rc::new({
                let changes = changes.clone();
                move || changes.set(changes.get() + 1)
            }),
        );
        let dialog = window.visible_dialog().unwrap();
        let entry: adw::EntryRow = named(&dialog, "source-repository");
        let lookup: gtk::Button = named(&dialog, "source-lookup");
        wait(|| entry.is_sensitive());
        entry.set_text("example/sample");
        wait(|| lookup.is_sensitive());
        assert_eq!(
            server.requests().len(),
            if commit { 5 } else { 0 },
            "opening Add source requested metadata"
        );
        press(&lookup);
        let add: gtk::Button = named(&dialog, "source-add");
        wait(|| add.is_sensitive());
        assert!(says(&dialog, "repository 12 · publisher 34"));
        assert!(widgets(dialog.upcast_ref()).iter().all(|widget| widget
            .downcast_ref::<gtk::Button>()
            .is_none_or(|button| button.label().as_deref() != Some("Install"))));
        assert!(!sources.exists(), "look-up wrote source storage");
        assert_eq!(
            std::fs::read(installed.join("installed.json")).unwrap(),
            before
        );
        if commit {
            press(&add);
            wait(|| window.visible_dialog().is_none());
        } else {
            screenshot(&window, "source-add-readonly-preview");
            xdo(&["key", "--clearmodifiers", "Escape"]);
            wait(|| window.visible_dialog().is_none());
            assert!(!sources.exists());
            assert_eq!(changes.get(), 0);
        }
    }
    assert_eq!(changes.get(), 1);
    let Outcome::EffectiveList {
        registrations,
        problem,
    } = storage(&sources, Request::EffectiveList).unwrap()
    else {
        panic!("effective source list missing")
    };
    assert!(problem.is_none());
    assert!(registrations
        .iter()
        .any(|source| source.origin.repository_id == 12 && source.snapshot_count() == 1));
    assert_eq!(
        std::fs::read(installed.join("installed.json")).unwrap(),
        before
    );
    window.destroy();
    println!("PASS Add source opens without network, look-up is read only, Cancel saves nothing and explicit Add saves a snapshot without changing installed plugins");
}

#[test]
#[ignore = "requires an isolated display and xdotool"]
fn source_removal_confirms_and_stale_callbacks_cannot_remove_official_or_newer_sources() {
    let window = window();
    let root = TestOwnedRoot::new();
    let sources = root.0.join("sources");
    storage(&sources, Request::Add(Box::new(source(12)))).unwrap();
    let installed = root.0.join("managed");
    crate::plugin::inventory::tests::fixture(&installed);
    let before = std::fs::read(installed.join("installed.json")).unwrap();
    let changes = Rc::new(Cell::new(0));
    present(
        &window,
        Services {
            root: Some(sources.clone()),
            lookup: Rc::new(|_| panic!("removal must not look up metadata")),
            refresh: Rc::new(|_| panic!("removal must not refresh metadata")),
        },
        Rc::new({
            let changes = changes.clone();
            move || changes.set(changes.get() + 1)
        }),
        Rc::new(|_| {}),
    );
    wait(|| named::<gtk::Button>(&window, "sources-check-all").is_sensitive());
    let remove: gtk::Button = named(&window, "source-remove:12");
    press(&remove);
    wait(|| window.visible_dialog().is_some());
    let dialog = window
        .visible_dialog()
        .unwrap()
        .downcast::<adw::AlertDialog>()
        .unwrap();
    assert!(dialog.body().contains("Installed plugins keep running"));
    let Outcome::EffectiveList { registrations, .. } =
        storage(&sources, Request::EffectiveList).unwrap()
    else {
        panic!("effective sources missing")
    };
    let expected = registrations
        .iter()
        .find(|source| source.origin.repository_id == 12)
        .unwrap()
        .clone();
    storage(
        &sources,
        Request::Refresh {
            expected,
            inspection: Box::new(source(12)),
        },
    )
    .unwrap();
    respond(dialog.upcast_ref(), "Remove source");
    wait(|| says(&window, "stale operation was refused"));
    assert_eq!(changes.get(), 0);
    assert_eq!(
        std::fs::read(installed.join("installed.json")).unwrap(),
        before
    );
    let result = storage(&sources, Request::Remove(registry::official_source()));
    assert!(
        result.is_err(),
        "a stale official remove callback acquired authority"
    );
    window.pop_subpage();
    let until = Instant::now() + Duration::from_millis(350);
    while Instant::now() < until {
        pump();
    }
    present(
        &window,
        Services {
            root: Some(sources.clone()),
            lookup: Rc::new(|_| panic!("removal used network")),
            refresh: Rc::new(|_| panic!("removal used network")),
        },
        Rc::new({
            let changes = changes.clone();
            move || changes.set(changes.get() + 1)
        }),
        Rc::new(|_| {}),
    );
    wait(|| named::<gtk::Button>(&window, "sources-check-all").is_sensitive());
    press(&named::<gtk::Button>(&window, "source-remove:12"));
    wait(|| window.visible_dialog().is_some());
    respond(&window.visible_dialog().unwrap(), "Remove source");
    wait(|| {
        !widgets(window.upcast_ref())
            .iter()
            .any(|widget| widget.widget_name() == "source-remove:12")
    });
    assert_eq!(
        std::fs::read(installed.join("installed.json")).unwrap(),
        before
    );
    window.destroy();
    println!("PASS confirmed source removal keeps installed bytes; stale registration and official-removal callbacks cannot remove authoritative sources");
}

#[test]
#[ignore = "requires an isolated display, xdotool and an owned TLS publisher fixture"]
fn a_source_publisher_transfer_keeps_the_old_snapshot_until_explicit_review() {
    let window = window();
    let root = TestOwnedRoot::new();
    let sources = root.0.join("sources");
    storage(&sources, Request::Add(Box::new(source(12)))).unwrap();
    let before = std::fs::read(sources.join("sources.json")).unwrap();
    let server = Rc::new(Server::new(|request| {
        if request.starts_with("GET /repositories/12 ") {
            Reply::json(
                serde_json::json!({"id":12,"owner":{"login":"new-publisher","id":99},"name":"sample","private":false,"default_branch":"main"}),
            )
        } else {
            normal(request)
        }
    }));
    let changes = Rc::new(Cell::new(0));
    present(
        &window,
        Services {
            root: Some(sources.clone()),
            lookup: Rc::new(|_| panic!("publisher review must use its observed snapshot")),
            refresh: Rc::new({
                let server = server.clone();
                move |id| server.refresh(id)
            }),
        },
        Rc::new({
            let changes = changes.clone();
            move || changes.set(changes.get() + 1)
        }),
        Rc::new(|_| panic!("this fixture is stable")),
    );
    wait(|| named::<gtk::Button>(&window, "sources-check-all").is_sensitive());
    press(&named::<gtk::Button>(&window, "source-check:12"));
    wait(|| {
        says(
            &window,
            "Publisher changed. Review before replacing the saved list.",
        )
    });
    assert_eq!(std::fs::read(sources.join("sources.json")).unwrap(), before);
    assert_eq!(changes.get(), 0);
    let review = widgets(window.upcast_ref())
        .into_iter()
        .find_map(|widget| {
            widget
                .downcast::<gtk::Button>()
                .ok()
                .filter(|button| button.label().as_deref() == Some("Review…"))
        })
        .unwrap();
    press(&review);
    wait(|| window.visible_dialog().is_some());
    let dialog = window.visible_dialog().unwrap();
    assert!(says(&dialog, "from 34 to 99"));
    respond(&dialog, "Cancel");
    wait(|| window.visible_dialog().is_none());
    assert_eq!(std::fs::read(sources.join("sources.json")).unwrap(), before);
    press(&review);
    wait(|| window.visible_dialog().is_some());
    respond(
        &window.visible_dialog().unwrap(),
        "Approve publisher change",
    );
    wait(|| changes.get() == 1);
    let Outcome::EffectiveList { registrations, .. } =
        storage(&sources, Request::EffectiveList).unwrap()
    else {
        panic!("effective sources missing")
    };
    let current = registrations
        .iter()
        .find(|source| source.origin.repository_id == 12)
        .unwrap();
    assert_eq!(current.origin.publisher_id, 99);
    assert_eq!(current.snapshot_count(), 1);
    assert_eq!(
        current.cached_inspection().unwrap().unwrap().publisher_id,
        99
    );
    assert_eq!(
        server.requests().len(),
        5,
        "review must reuse the checked snapshot without another network request"
    );
    window.destroy();
    println!("PASS actual publisher transfer preserves the saved list until renewed review; Cancel changes nothing and explicit approval saves the new publisher once");
}
