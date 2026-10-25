use super::*;
use crate::plugin::install::{
    source::tests::{Reply, Server},
    TestRoot as OwnedRoot,
};
use glib::translate::{from_glib_full, ToGlibPtr};
use std::path::Path;
use std::time::{Duration, Instant};

fn pump(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "official source widget deadline");
        while glib::MainContext::default().pending() {
            glib::MainContext::default().iteration(false);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn buttons(widget: &gtk::Widget) -> Vec<gtk::Button> {
    let mut found = Vec::new();
    if let Ok(button) = widget.clone().downcast::<gtk::Button>() {
        found.push(button);
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        found.extend(buttons(&widget));
        child = widget.next_sibling();
    }
    found
}

fn button(row: &adw::ActionRow, label: &str) -> gtk::Button {
    buttons(row.upcast_ref())
        .into_iter()
        .find(|button| button.label().as_deref() == Some(label))
        .unwrap()
}

fn xdo(arguments: &[&str]) -> String {
    let output = std::process::Command::new("xdotool")
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn show(root: PathBuf, refresh: Rc<Refresh>) -> (adw::PreferencesWindow, Rc<Panel>) {
    let page = adw::PreferencesPage::new();
    let preview = adw::PreferencesGroup::new();
    page.add(&preview);
    let panel = Panel::new(
        &page,
        &preview,
        Some(root),
        refresh,
        Rc::new(|_| {
            panic!("this test never opens catalogue cards");
        }),
    );
    let window = adw::PreferencesWindow::builder()
        .title("Among official source protection")
        .default_width(900)
        .default_height(800)
        .build();
    window.add(&page);
    window.present();
    pump(|| !panel.busy.get() && panel.rows.borrow().iter().all(|row| row.is_mapped()));
    (window, panel)
}

fn official(panel: &Panel) -> adw::ActionRow {
    let rows = panel.rows.borrow();
    let matching: Vec<_> = rows
        .iter()
        .filter(|row| row.title() == "Official plugins for Among Terminal")
        .collect();
    assert_eq!(matching.len(), 1);
    let row = matching[0].clone();
    assert!(row.is_mapped());
    assert!(row.subtitle().unwrap().contains("cannot be removed"));
    assert!(!buttons(row.upcast_ref())
        .iter()
        .any(|button| button.label().as_deref() == Some("Remove source")));
    let refresh = button(&row, "Refresh");
    assert!(gtk::test_accessible_has_role(
        &refresh,
        gtk::AccessibleRole::Button
    ));
    let label = std::ffi::CString::new("Refresh repository 1431055733").unwrap();
    let mismatch = unsafe {
        gtk::ffi::gtk_test_accessible_check_property(
            refresh.upcast_ref::<gtk::Accessible>().to_glib_none().0,
            gtk::ffi::GTK_ACCESSIBLE_PROPERTY_LABEL,
            label.as_ptr(),
        )
    };
    if !mismatch.is_null() {
        let message: glib::GString = unsafe { from_glib_full(mismatch) };
        panic!("official refresh accessible label mismatch: {message}");
    }
    row
}

fn activate(button: &gtk::Button) {
    let ids = xdo(&[
        "search",
        "--all",
        "--onlyvisible",
        "--pid",
        &std::process::id().to_string(),
        "--name",
        "^Among official source protection$",
    ]);
    let id = ids.lines().last().unwrap();
    xdo(&["windowactivate", "--sync", id]);
    xdo(&["windowfocus", "--sync", id]);
    assert!(button.grab_focus());
    pump(|| button.has_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
}

fn capture(directory: &Path, name: &str) {
    let ids = xdo(&[
        "search",
        "--all",
        "--onlyvisible",
        "--pid",
        &std::process::id().to_string(),
        "--name",
        "^Among official source protection$",
    ]);
    let path = directory.join(name);
    let status = std::process::Command::new("import")
        .arg("-window")
        .arg(ids.lines().last().unwrap())
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(std::fs::metadata(&path).unwrap().len() > 0);
    println!("official_source_capture={}", path.display());
}

#[test]
#[ignore = "requires an isolated GTK test-accessibility display and real keyboard/capture tools"]
fn official_row_survives_offline_refresh_custom_removal_reopen_and_corrupt_storage() {
    adw::init().expect("isolated GTK display");
    let captures = PathBuf::from(
        std::env::var_os("AMONG_OFFICIAL_SOURCE_CAPTURES").expect("owned capture directory"),
    );
    assert!(captures.starts_with(std::env::temp_dir()));
    std::fs::create_dir_all(&captures).unwrap();
    let root = OwnedRoot::new();
    let sources = root.0.join("sources");
    let installed = root.0.join("managed");
    crate::plugin::inventory::tests::fixture(&installed);
    let installed_before = std::fs::read(installed.join("installed.json")).unwrap();
    let configuration = root.0.join("settings-sentinel.json");
    std::fs::write(
        &configuration,
        br#"{"unknown":"retain","legacy":{"enabled":true}}"#,
    )
    .unwrap();
    let config_before = std::fs::read(&configuration).unwrap();
    let network = Rc::new(Server::new(|_| Reply::rate_limited()));
    let refresh: Rc<Refresh> = Rc::new({
        let network = network.clone();
        move |id| network.refresh(id)
    });

    let (window, panel) = show(sources.clone(), refresh.clone());
    let row = official(&panel);
    assert!(!button(&row, "Cached").is_sensitive());
    assert!(
        network.requests().is_empty(),
        "opening the list is not network consent"
    );
    assert!(
        !sources.exists(),
        "built-in authority is not persisted on first display"
    );
    capture(&captures, "official-source-fresh.png");
    window.destroy();
    drop(panel);

    let custom = Server::new(crate::plugin::install::source::tests::normal)
        .inspect(true)
        .unwrap();
    let save = registry::Job::start(sources.clone(), Request::Add(Box::new(custom))).unwrap();
    let mut saved = None;
    pump(|| {
        if let Some(outcome) = save.poll() {
            saved = Some(outcome);
        }
        saved.is_some()
    });
    assert!(matches!(saved.unwrap(), Ok(Outcome::Changed { .. })));
    let (window, panel) = show(sources.clone(), refresh.clone());
    assert_eq!(panel.rows.borrow().len(), 2);
    let row = official(&panel);
    let before = std::fs::read(sources.join("sources.json")).unwrap();
    activate(&button(&row, "Refresh"));
    pump(|| network.requests().len() == 1 && !panel.busy.get());
    assert!(panel
        .status
        .upgrade()
        .unwrap()
        .title()
        .contains("Refresh failed"));
    assert_eq!(std::fs::read(sources.join("sources.json")).unwrap(), before);
    official(&panel);
    let custom = panel
        .rows
        .borrow()
        .iter()
        .find(|row| row.title() != "Official plugins for Among Terminal")
        .unwrap()
        .clone();
    activate(&button(&custom, "Remove source"));
    pump(|| !panel.busy.get() && panel.rows.borrow().len() == 1);
    official(&panel);
    capture(&captures, "official-source-custom-removed.png");
    let after = std::fs::read(sources.join("sources.json")).unwrap();
    assert_ne!(after, before);
    window.destroy();
    drop(panel);

    let (window, panel) = show(sources.clone(), refresh.clone());
    assert_eq!(panel.rows.borrow().len(), 1);
    official(&panel);
    assert_eq!(std::fs::read(sources.join("sources.json")).unwrap(), after);
    window.destroy();
    drop(panel);
    std::fs::write(sources.join("sources.json"), b"retain corrupt user bytes").unwrap();
    let (window, panel) = show(sources.clone(), refresh);
    official(&panel);
    assert!(panel
        .status
        .upgrade()
        .unwrap()
        .title()
        .contains("User source storage is unavailable"));
    assert_eq!(
        std::fs::read(sources.join("sources.json")).unwrap(),
        b"retain corrupt user bytes"
    );
    capture(&captures, "official-source-corrupt-storage.png");
    assert_eq!(
        std::fs::read(installed.join("installed.json")).unwrap(),
        installed_before
    );
    assert_eq!(std::fs::read(configuration).unwrap(), config_before);
    assert_eq!(
        network.requests().len(),
        1,
        "no background retry after offline refresh"
    );
    println!("PASS official_source_widget built_in=true no_remove_control=true exact_accessible_refresh_label=true real_space_refresh_and_custom_remove=true offline_no_retry=true corrupt_bytes_retained=true installed_and_settings_bytes_unchanged=true");
    window.destroy();
}
