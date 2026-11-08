use super::*;
use crate::plugin::install::metadata::{Architecture, Catalogue, Platform};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn input_proof(ctx: &Window, terminal: &vte::Terminal, pty: &vte::Pty, phase: &str) {
    assert_eq!(terminal.pty().as_ref(), Some(pty));
    focus(&ctx.window);
    apply_focus(ctx);
    wait_for(|| terminal.has_focus(), Duration::from_secs(2));
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let proof = home.join(format!("published-input-{phase}"));
    crate::test_isolation::assert_below_check_root(&proof);
    assert!(!proof.exists());
    xdo(&["type", "--clearmodifiers", "--delay", "0", &format!("printf '{phase}' > \"$HOME/published-input-{phase}\"; printf '\\033]7;file://localhost%s\\007' \"$PWD\"")]);
    xdo(&["key", "--clearmodifiers", "Return"]);
    wait_for(
        || std::fs::read_to_string(&proof).ok().as_deref() == Some(phase),
        Duration::from_secs(3),
    );
    assert_eq!(terminal.pty().as_ref(), Some(pty));
    println!("published_release_terminal_input={phase} same_pty=true");
}

fn manager_row(preferences: &adw::PreferencesWindow, name: &str) -> adw::ActionRow {
    let find = || {
        widgets(preferences.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
            .find(|row| {
                row.title() == name
                    && widgets(row.upcast_ref())
                        .iter()
                        .any(|widget| widget.is::<gtk::Switch>())
            })
    };
    wait_for(|| find().is_some(), Duration::from_secs(3));
    find().unwrap()
}

fn toggle(preferences: &adw::PreferencesWindow, row: &adw::ActionRow) {
    focus(preferences);
    let switch = widgets(row.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::Switch>().ok())
        .unwrap();
    assert!(switch.is_sensitive() && switch.grab_focus());
    xdo(&["key", "--clearmodifiers", "space"]);
}

pub(super) fn read_regular(path: &Path, maximum: u64) -> Vec<u8> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .unwrap();
    let metadata = file.metadata().unwrap();
    assert!(metadata.is_file() && metadata.len() <= maximum);
    let mut bytes = Vec::new();
    file.take(maximum + 1).read_to_end(&mut bytes).unwrap();
    assert!(bytes.len() as u64 <= maximum);
    assert_eq!(
        bytes.len() as u64,
        metadata.len(),
        "profile input changed while reading"
    );
    bytes
}

fn source_bytes(path: &Path) -> Option<Vec<u8>> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Some(read_regular(path, 1024 * 1024)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("cannot inspect source cache: {error}"),
    }
}

#[test]
fn profile_reader_refuses_oversize_symlink_and_fifo_inputs_without_blocking() {
    use std::os::unix::{ffi::OsStrExt, fs::symlink};
    let root = crate::plugin::install::TestOwnedRoot::new();
    let regular = root.0.join("regular");
    std::fs::write(&regular, b"exact bytes").unwrap();
    assert_eq!(read_regular(&regular, 16), b"exact bytes");
    assert!(std::panic::catch_unwind(|| read_regular(&regular, 4)).is_err());
    let link = root.0.join("link");
    symlink(&regular, &link).unwrap();
    assert!(std::panic::catch_unwind(|| read_regular(&link, 16)).is_err());
    let fifo = root.0.join("fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let started = std::time::Instant::now();
    assert!(std::panic::catch_unwind(|| read_regular(&fifo, 16)).is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
}

fn capture_live_profile(
    root: &Path,
    config: &Path,
    inputs: &Path,
    receipts: &[crate::plugin::inventory::Receipt],
) {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let profile = crate::test_isolation::check_root().join(".runtime-published-live-profile");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&profile)
        .unwrap();
    let managed = profile.join("managed");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&managed)
        .unwrap();
    let mut remaining = 8 * 1024 * 1024u64;
    let mut copy = |source: &Path, target: &Path, executable: bool| {
        let bytes = read_regular(source, remaining);
        remaining = remaining.checked_sub(bytes.len() as u64).unwrap();
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(target.parent().unwrap())
            .unwrap();
        std::fs::write(target, bytes).unwrap();
        std::fs::set_permissions(
            target,
            std::fs::Permissions::from_mode(if executable { 0o700 } else { 0o600 }),
        )
        .unwrap();
    };
    copy(
        &root.join("installed.json"),
        &managed.join("installed.json"),
        false,
    );
    copy(config, &profile.join("managed-plugins.json"), false);
    copy(
        &inputs.join("among-plugins.json"),
        &profile.join("among-plugins.json"),
        false,
    );
    copy(
        &inputs.join("inputs.json"),
        &profile.join("release-inputs.json"),
        false,
    );
    for receipt in receipts {
        let source = receipt.package_path(root);
        let target = receipt.package_path(&managed);
        let manifest = crate::plugin::manifest::Manifest::parse(&read_regular(
            &source.join("among-plugin.json"),
            256 * 1024,
        ))
        .unwrap();
        // Replay needs the executable, inspected metadata and lease/seal only,
        // not a recursive archive/source/licence tree in screenshot artifacts.
        for path in [
            "among-plugin.json",
            ".among-receipt.json",
            ".among-lease",
            manifest.entrypoint.as_str(),
        ] {
            copy(
                &source.join(path),
                &target.join(path),
                path == manifest.entrypoint,
            );
        }
        if let Some(crate::plugin::manifest::Icon::File(path)) = &manifest.icon {
            copy(&source.join(path), &target.join(path), false);
        }
    }
    let installed = read_regular(&managed.join("installed.json"), 256 * 1024);
    let settings = read_regular(&profile.join("managed-plugins.json"), 256 * 1024);
    let observation: Value = crate::plugin::data::parse_unique(
        &read_regular(&inputs.join("inputs.json"), 256 * 1024),
        256 * 1024,
    )
    .unwrap();
    std::fs::write(profile.join("profile.json"), serde_json::to_vec_pretty(&json!({"kind":"observed_published_installation_profile","host_source_sha":option_env!("AMONG_CHECK_SOURCE_SHA"),"catalogue_revision":observation["catalogue_revision"],"source_revision":observation["source_revision"],"architecture":match Platform::current().unwrap().architecture {Architecture::Amd64=>"amd64",Architecture::Arm64=>"arm64"},"installed_size":installed.len(),"installed_sha256":crate::plugin::inventory::checksum(&installed).unwrap(),"settings_size":settings.len(),"settings_sha256":crate::plugin::inventory::checksum(&settings).unwrap()})).unwrap()).unwrap();
}

pub(super) fn inspect(
    ctx: &Window,
    preferences: &adw::PreferencesWindow,
    revision: &str,
) -> (
    adw::PreferencesWindow,
    crate::plugin::install::source::Inspection,
) {
    let prerelease = prerelease_acceptance();
    let observation = Rc::new(RefCell::new(None));
    let source_file = crate::plugin::install::registry::root()
        .unwrap()
        .join("sources.json");
    let sources_before = source_bytes(&source_file);
    let view = if prerelease {
        let page = crate::prefs::plugins_page_with_observation_for_test(
            &preferences_context(ctx),
            super::candidate::services(),
            observation.clone(),
        );
        let window = adw::PreferencesWindow::builder()
            .transient_for(preferences)
            .modal(false)
            .title("Plugin list")
            .search_enabled(false)
            .default_width(760)
            .default_height(640)
            .build();
        window.add(&page);
        window.present();
        window
    } else {
        list_window(ctx, preferences, super::candidate::services())
    };
    focus(&view);
    if widgets(view.upcast_ref()).iter().any(|widget| {
        widget
            .downcast_ref::<gtk::ToggleButton>()
            .is_some_and(|button| button.label().as_deref() == Some("Stable"))
    }) {
        press(&channel_button(
            &view,
            if prerelease { "Pre-release" } else { "Stable" },
        ));
    }
    if prerelease {
        assert!(channel_active(&view, "Pre-release"));
    }
    let refresh = named_button(&view, "plugins-refresh");
    press(&refresh);
    wait_for(|| !refresh.is_sensitive(), Duration::from_secs(3));
    wait_for(|| refresh.is_sensitive(), Duration::from_secs(65));
    let saved = if prerelease {
        if observation.borrow().is_none() {
            eprintln!(
                "published_prerelease_refresh_refused selected_prerelease={} refresh_sensitive={} development={}",
                channel_active(&view, "Pre-release"),
                refresh.is_sensitive(),
                crate::plugin::development::enabled()
            );
            for widget in widgets(view.upcast_ref()) {
                if let Some(label) = widget.downcast_ref::<gtk::Label>() {
                    eprintln!(
                        "published_prerelease_refresh_label mapped={} visible={} text={:?}",
                        label.is_mapped(),
                        label.is_visible(),
                        label.text()
                    );
                }
            }
            screenshot(&view, "published-prerelease-refresh-refused");
        }
        let inspected = observation
            .borrow_mut()
            .take()
            .expect("the selected channel must complete a validated real source observation");
        let expected = observed();
        let (snapshot, binding) = inspected.observed_release_for_test();
        assert!(snapshot.is_prerelease());
        assert_eq!(snapshot, &expected.snapshot);
        assert_eq!(binding, &expected.binding);
        assert_eq!(source_bytes(&source_file), sources_before);
        inspected
    } else {
        saved_catalogue()
    };
    assert_eq!(
        saved.catalogue_revision, revision,
        "the explicitly checked catalogue must match the independently prepared source binding"
    );
    assert_eq!(saved.repository_id, 1431055733);
    assert_eq!(saved.publisher_id, 339567100);
    (view, saved)
}

pub(super) fn saved_catalogue() -> crate::plugin::install::source::Inspection {
    use crate::plugin::install::registry::{self, Outcome, Request};
    let job = registry::Job::start(registry::root().unwrap(), Request::EffectiveList).unwrap();
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
    let Outcome::EffectiveList {
        registrations,
        problem,
    } = result.into_inner().unwrap().unwrap()
    else {
        panic!("effective registered sources missing")
    };
    assert!(problem.is_none());
    registrations
        .into_iter()
        .find(|source| source.is_official())
        .unwrap()
        .cached_inspection()
        .unwrap()
        .unwrap()
}

pub(super) fn catalogue_row(view: &adw::PreferencesWindow, identity: &str) -> adw::ActionRow {
    widgets(view.upcast_ref())
        .into_iter()
        .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
        .find(|row| row.widget_name() == identity)
        .unwrap()
}

fn assert_output(ctx: &Window, id: &str, key: &str) {
    let scope = match id {
        "clock" => Scope::AppFooter,
        "system-status" => Scope::AppHeader,
        "pane-pwd" => Scope::PaneFooter(PaneId(1)),
        _ => panic!("unexpected official plugin"),
    };
    wait_for(
        || {
            let state = ctx.state.borrow();
            let fragments = state
                .store
                .fragments(scope)
                .into_iter()
                .filter(|fragment| fragment.source == key)
                .collect::<Vec<_>>();
            match id {
                "clock" => {
                    fragments.len() == 1
                        && fragments[0].text.as_deref().is_some_and(|text| {
                            text.len() == 8
                                && text.bytes().filter(|byte| *byte == b':').count() == 2
                        })
                }
                "system-status" => {
                    fragments.len() == 3
                        && fragments.iter().all(|fragment| {
                            fragment.meter.is_some_and(|meter| {
                                meter.is_finite() && (0.0..=1.0).contains(&meter)
                            }) && fragment
                                .text
                                .as_deref()
                                .is_some_and(|text| text.ends_with('%'))
                        })
                }
                "pane-pwd" => {
                    fragments.len() == 1
                        && state
                            .pane(PaneId(1))
                            .and_then(|pane| pane.cwd())
                            .is_some_and(|cwd| {
                                fragments[0].tooltip.as_deref() == Some(cwd.as_str())
                                    && fragments[0].text.as_deref() == Some(cwd.as_str())
                            })
                }
                _ => false,
            }
        },
        Duration::from_secs(8),
    );
    let container: gtk::Widget = match scope {
        Scope::AppHeader => ctx.app_header.clone().upcast(),
        Scope::AppFooter => ctx.app_footer.clone().upcast(),
        Scope::PaneFooter(pane) => ctx
            .state
            .borrow()
            .pane(pane)
            .unwrap()
            .widget()
            .unwrap()
            .upcast(),
        _ => unreachable!(),
    };
    // store publication precedes frame-clock allocation. Re-read both on each
    // turn: a timer can replace the widgets or update a meter while we wait.
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    wait_for(
        || {
            let state = ctx.state.borrow();
            let fragments = state
                .store
                .fragments(scope)
                .into_iter()
                .filter(|fragment| fragment.source == key)
                .collect::<Vec<_>>();
            let tree = widgets(&container);
            let labels = tree
                .iter()
                .filter_map(|widget| widget.downcast_ref::<gtk::Label>())
                .collect::<Vec<_>>();
            let bars = tree
                .iter()
                .filter_map(|widget| widget.downcast_ref::<gtk::ProgressBar>())
                .collect::<Vec<_>>();
            let rendered = !fragments.is_empty()
                && fragments.iter().all(|fragment| {
                    labels.iter().any(|label| {
                        Some(label.text().as_str()) == fragment.text.as_deref()
                            && label.is_mapped()
                            && label.width() > 0
                    })
                })
                && (id != "system-status"
                    || (bars.len() == 3
                        && bars.iter().zip(&fragments).all(|(bar, fragment)| {
                            bar.is_mapped()
                                && bar.width() > 0
                                && bar.height() > 0
                                && Some(bar.fraction()) == fragment.meter
                        })));
            if !rendered && std::time::Instant::now() >= deadline {
                eprintln!(
                    "published_output_timeout plugin={id} scope={scope:?} expected={fragments:?}"
                );
                for label in labels {
                    eprintln!(
                        "observed_label text={:?} mapped={} visible={} width={} height={}",
                        label.text(),
                        label.is_mapped(),
                        label.is_visible(),
                        label.width(),
                        label.height()
                    );
                }
                for bar in bars {
                    eprintln!(
                        "observed_meter fraction={} mapped={} width={} height={}",
                        bar.fraction(),
                        bar.is_mapped(),
                        bar.width(),
                        bar.height()
                    );
                }
                let path = crate::test_isolation::check_root()
                    .join(format!("published-{id}-render-timeout.png"));
                eprintln!(
                    "published_output_failure_screenshot={} result={:?}",
                    path.display(),
                    std::process::Command::new("import")
                        .args(["-window", "root"])
                        .arg(&path)
                        .status()
                );
                panic!("published {id} full-text mapped/allocated output did not render");
            }
            rendered
        },
        Duration::from_secs(9),
    );
    println!("published_release_output={id} actual_mapped_widgets=true");
}

/// The prepared observation of the official release, checked against its own
/// catalogue bytes. Both native acceptance cases read the release, asset IDs
/// and package digests from here, never from constants of an older release.
pub(in crate::app) struct Observed {
    pub(super) inputs_root: PathBuf,
    pub(super) inputs: Value,
    pub(super) platform: Platform,
    pub(super) architecture: &'static str,
    pub(super) snapshot: crate::plugin::inventory::release::ReleaseSnapshot,
    pub(super) binding: crate::plugin::inventory::ReleaseBinding,
    pub(super) revision: String,
    pub(super) catalogue: Catalogue,
}

pub(in crate::app) fn prerelease_acceptance() -> bool {
    match std::env::var_os("AMONG_PUBLISHED_PRERELEASE_ACCEPTANCE") {
        None => false,
        Some(value) if value == "1" => true,
        Some(_) => panic!("published prerelease acceptance requires the exact value 1"),
    }
}

fn assert_prerelease_pin(
    inputs: &Value,
    snapshot: &crate::plugin::inventory::release::ReleaseSnapshot,
) {
    assert_eq!(
        inputs["candidate"], false,
        "synthetic candidates cannot enter published acceptance"
    );
    assert!(snapshot.is_prerelease());
    assert_eq!(
        inputs["published_pin"],
        json!({
            "repository_id":inputs["repository_id"],
            "publisher_id":inputs["publisher_id"],
            "release_id":snapshot.id,
            "tag":snapshot.tag,
            "source_revision":inputs["source_revision"],
            "catalogue_sha256":snapshot.asset("among-plugins.json").unwrap().sha256,
        })
    );
}

#[test]
fn published_prerelease_pin_refuses_candidates_and_each_mismatched_identity() {
    let snapshot: crate::plugin::inventory::release::ReleaseSnapshot =
        serde_json::from_value(json!({
            "id":42,"tag":"v1.0.0-rc.1","prerelease":true,
            "assets":[{"id":43,"name":"among-plugins.json","size":2,"sha256":"a".repeat(64)}]
        }))
        .unwrap();
    snapshot.validate().unwrap();
    let inputs = json!({
        "candidate":false,"repository_id":12,"publisher_id":34,"source_revision":"b".repeat(40),
        "published_pin":{"repository_id":12,"publisher_id":34,"release_id":42,
            "tag":"v1.0.0-rc.1","source_revision":"b".repeat(40),"catalogue_sha256":"a".repeat(64)}
    });
    assert_prerelease_pin(&inputs, &snapshot);
    let mut candidate = inputs.clone();
    candidate["candidate"] = json!(true);
    assert!(std::panic::catch_unwind(|| assert_prerelease_pin(&candidate, &snapshot)).is_err());
    for (field, value) in [
        ("repository_id", json!(13)),
        ("publisher_id", json!(35)),
        ("release_id", json!(44)),
        ("tag", json!("v1.0.0-rc.2")),
        ("source_revision", json!("c".repeat(40))),
        ("catalogue_sha256", json!("d".repeat(64))),
    ] {
        let mut changed = inputs.clone();
        changed["published_pin"][field] = value;
        assert!(
            std::panic::catch_unwind(|| assert_prerelease_pin(&changed, &snapshot)).is_err(),
            "a changed {field} must not enter published acceptance"
        );
    }
}

pub(in crate::app) fn observed() -> Observed {
    let inputs_root = PathBuf::from(
        std::env::var_os("AMONG_OFFICIAL_PLUGIN_INPUTS")
            .expect("dedicated runner must supply actual published Release inputs"),
    );
    assert!(inputs_root.is_absolute());
    let inputs: Value = crate::plugin::data::parse_unique(
        &read_regular(&inputs_root.join("inputs.json"), 256 * 1024),
        256 * 1024,
    )
    .unwrap();
    let platform = Platform::current().unwrap();
    let architecture = match platform.architecture {
        Architecture::Amd64 => "amd64",
        Architecture::Arm64 => "arm64",
    };
    assert_eq!(inputs["schema_version"], 2);
    assert_eq!(inputs["kind"], "published_release_inputs_not_installed");
    assert_eq!(inputs["repository_id"], 1431055733u64);
    assert_eq!(inputs["publisher_id"], 339567100u64);
    let snapshot: crate::plugin::inventory::release::ReleaseSnapshot =
        serde_json::from_value(inputs["release_snapshot"].clone()).unwrap();
    snapshot.validate().unwrap();
    let binding: crate::plugin::inventory::ReleaseBinding =
        serde_json::from_value(inputs["release_binding"].clone()).unwrap();
    let revision = inputs["source_revision"].as_str().unwrap().to_owned();
    assert_eq!(inputs["catalogue_revision"], revision);
    assert_eq!(binding.commit, revision);
    assert_eq!(binding.tag, snapshot.tag);
    assert_eq!(inputs["release_id"], snapshot.id);
    let catalogue_asset = snapshot.asset("among-plugins.json").unwrap();
    assert_eq!(inputs["catalogue_size"], catalogue_asset.size);
    assert_eq!(inputs["catalogue_sha256"], catalogue_asset.sha256);
    assert_eq!(inputs["architecture"], architecture);
    let bytes = read_regular(&inputs_root.join("among-plugins.json"), 1024 * 1024);
    assert_eq!(bytes.len() as u64, catalogue_asset.size);
    assert_eq!(
        crate::plugin::inventory::checksum(&bytes).unwrap(),
        catalogue_asset.sha256
    );
    let catalogue = Catalogue::parse(&bytes).unwrap();
    assert_eq!(catalogue.plugins.len(), 3);
    assert_eq!(inputs["plugins"].as_array().unwrap().len(), 3);
    if prerelease_acceptance() {
        assert_prerelease_pin(&inputs, &snapshot);
        let read = |name| {
            crate::plugin::data::parse_unique(
                &read_regular(&inputs_root.join(name), 1024 * 1024),
                1024 * 1024,
            )
            .unwrap()
        };
        for name in ["latest.json", "latest-final.json"] {
            let raw = read(name);
            assert_eq!(
                crate::plugin::inventory::release::ReleaseSnapshot::from_prerelease_value(&raw)
                    .unwrap(),
                snapshot,
                "the original immutable prerelease observation must match the pinned snapshot"
            );
        }
        for name in ["repository.json", "repository-final.json"] {
            let raw = read(name);
            assert_eq!(raw["id"], inputs["repository_id"]);
            assert_eq!(raw["owner"]["id"], inputs["publisher_id"]);
            assert_eq!(raw["private"], false);
        }
        for name in ["initial-ref.json", "final-ref.json"] {
            let raw = read(name);
            assert_eq!(raw["ref"], format!("refs/tags/{}", binding.tag));
            assert_eq!(raw["object"]["sha"], binding.object);
        }
        for name in ["initial-commit.json", "final-commit.json"] {
            assert_eq!(read(name)["sha"], binding.commit);
        }
        if !crate::plugin::development::enabled() {
            assert!(crate::plugin::development::fix(true));
        }
        assert!(crate::plugin::development::enabled());
        println!(
            "published_prerelease_acceptance release={} tag={} source={} candidate_bytes=false snapshot_prerelease=true",
            snapshot.id, snapshot.tag, revision
        );
    } else {
        assert!(
            !snapshot.is_prerelease(),
            "ordinary acceptance requires stable inputs"
        );
    }
    Observed {
        inputs_root,
        inputs,
        platform,
        architecture,
        snapshot,
        binding,
        revision,
        catalogue,
    }
}

pub(super) fn run() {
    let Observed {
        inputs_root,
        inputs,
        platform,
        architecture,
        snapshot,
        binding,
        revision,
        catalogue,
    } = observed();
    let revision = revision.as_str();
    let channel_suffix = if snapshot.is_prerelease() {
        " · prerelease"
    } else {
        ""
    };
    let catalogue_asset = snapshot.asset("among-plugins.json").unwrap();
    let root = crate::plugin::inventory::root().unwrap();
    crate::test_isolation::assert_below_check_root(&root);
    assert!(crate::plugin::inventory::read(&root).unwrap().is_empty());
    assert!(!root.join("installed.json").exists());
    let app = application("com.github.among-terminal.published-lifecycle-test");
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
    input_proof(&ctx, &terminal, &pty, "before-install");
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
    let (view, saved) = inspect(&ctx, &preferences, revision);
    assert_eq!(saved.catalogue, catalogue);
    let mut receipts = Vec::new();
    for plugin in &catalogue.plugins {
        let version = &plugin.versions[0];
        let artifact = version.select(&platform).unwrap();
        let expected_input = inputs["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|input| input["id"] == plugin.id)
            .unwrap();
        assert_eq!(expected_input["asset"], artifact.asset);
        let observed_asset_id = expected_input["asset_id"].as_u64().unwrap();
        assert_eq!(expected_input["version"], version.manifest.version);
        assert_eq!(expected_input["revision"], revision);
        assert_eq!(expected_input["architecture"], architecture);
        let asset = snapshot.asset(&artifact.asset).unwrap();
        assert_eq!(observed_asset_id, asset.id);
        assert_eq!(artifact.size, asset.size);
        assert_eq!(artifact.sha256, asset.sha256);
        let archive = inputs_root.join(format!("{}.tar.gz", plugin.id));
        assert_eq!(
            crate::plugin::inventory::checksum(&read_regular(&archive, 64 * 1024 * 1024)).unwrap(),
            artifact.sha256
        );
        let expected = format!(
            "github:1431055733/{} · version {} · linux/{architecture} · asset {} · {} bytes{channel_suffix}",
            plugin.id, version.manifest.version, artifact.asset, artifact.size
        );
        assert_eq!(
            saved
                .select(&plugin.id, &version.manifest.version)
                .unwrap()
                .description(),
            expected
        );
        let key = format!("github:1431055733/{}", plugin.id);
        let row = catalogue_row(&view, &key);
        assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
        press(&button(&row, "Install"));
        wait_for(
            || crate::plugin::inventory::read(&root).unwrap().len() == receipts.len() + 1,
            Duration::from_secs(65),
        );
        let key = format!("github:1431055733/{}", plugin.id);
        wait_for(
            || ctx.state.borrow().plugins.managed_details(&key).is_some(),
            Duration::from_secs(3),
        );
        let (entry, _) = ctx.state.borrow().plugins.managed_details(&key).unwrap();
        let receipt = &entry.receipt;
        assert_eq!(receipt.requires_development(), snapshot.is_prerelease());
        let release = receipt.release().unwrap();
        assert_eq!(release.repository_id, 1431055733);
        assert_eq!(release.release_id, snapshot.id);
        assert_eq!(release.asset_id, observed_asset_id);
        assert_eq!(receipt.revision, revision);
        assert_eq!(receipt.manifest_sha256, version.manifest_sha256);
        assert_eq!(receipt.package_sha256, artifact.sha256);
        let source = release.source.as_ref().unwrap();
        assert_eq!(source.catalogue_revision, revision);
        assert_eq!(source.catalogue_sha256, catalogue_asset.sha256);
        assert_eq!(source.publisher_id, 339567100);
        assert_eq!(*source.release_snapshot, snapshot);
        assert_eq!(source.release_binding, binding);
        let inspected = entry.inspected.as_ref().unwrap();
        assert_eq!(inspected.manifest, version.manifest);
        let member = artifact
            .members
            .iter()
            .find(|member| member.path == inspected.manifest.entrypoint)
            .unwrap();
        assert_eq!(
            crate::plugin::inventory::checksum(&read_regular(
                &inspected.executable,
                32 * 1024 * 1024
            ))
            .unwrap(),
            member.sha256
        );
        assert!(!ctx.state.borrow().plugins.desired_enabled(&key));
        assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
        receipts.push(receipt.clone());
    }
    view.destroy();
    let config = crate::settings::directory()
        .unwrap()
        .join("managed-plugins.json");
    let before_review = std::fs::read(&config).ok();
    for plugin in &catalogue.plugins {
        let version = &plugin.versions[0];
        let key = format!("github:1431055733/{}", plugin.id);
        let row = manager_row(&preferences, &version.manifest.name);
        toggle(&preferences, &row);
        wait_for(
            || preferences.visible_dialog().is_some(),
            Duration::from_secs(2),
        );
        let dialog = preferences.visible_dialog().unwrap();
        assert!(has_label(dialog.upcast_ref(), "Review plugin access"));
        let controls = widgets(dialog.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::CheckButton>().ok())
            .collect::<Vec<_>>();
        let labels = widgets(dialog.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
            .map(|label| label.text().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            controls.len(),
            version.manifest.required_features.len() + version.manifest.optional_features.len(),
            "review of {}: {labels:?}",
            plugin.id
        );
        for control in &controls {
            let required = control.label().unwrap().ends_with(" (required)");
            assert_eq!(control.is_active(), required);
        }
        assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
        assert_eq!(std::fs::read(&config).ok(), before_review);
        press(&button(&dialog, "Cancel"));
        wait_for(
            || {
                preferences.visible_dialog().is_none()
                    && !ctx.state.borrow().plugins.desired_enabled(&key)
            },
            Duration::from_secs(2),
        );
        assert_eq!(std::fs::read(&config).ok(), before_review);
    }
    input_proof(&ctx, &terminal, &pty, "after-review-cancel");
    for plugin in &catalogue.plugins {
        let version = &plugin.versions[0];
        let key = format!("github:1431055733/{}", plugin.id);
        let row = manager_row(&preferences, &version.manifest.name);
        toggle(&preferences, &row);
        wait_for(
            || preferences.visible_dialog().is_some(),
            Duration::from_secs(2),
        );
        press(&button(
            &preferences.visible_dialog().unwrap(),
            "Accept and enable",
        ));
        wait_for(
            || {
                ctx.state
                    .borrow()
                    .plugins
                    .listing()
                    .iter()
                    .any(|(name, state)| name == &key && state == &PluginState::Running)
            },
            Duration::from_secs(7),
        );
        let saved =
            crate::settings::load(&config, crate::settings::Document::ManagedPlugins).unwrap();
        assert_eq!(saved["plugins"][&key]["enabled"], true);
        assert_eq!(
            saved["plugins"][&key]["review"]["features"],
            json!(version.manifest.required_features)
        );
        assert_output(&ctx, &plugin.id, &key);
        input_proof(&ctx, &terminal, &pty, &format!("running-{}", plugin.id));
    }
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 3);
    screenshot(&ctx.window, "published-all-three-running");
    capture_live_profile(&root, &config, &inputs_root, &receipts);
    for plugin in &catalogue.plugins {
        let version = &plugin.versions[0];
        let key = format!("github:1431055733/{}", plugin.id);
        let row = manager_row(&preferences, &version.manifest.name);
        toggle(&preferences, &row);
        wait_for(
            || {
                ctx.state
                    .borrow()
                    .plugins
                    .running_generation(&key)
                    .is_none()
                    && !ctx.state.borrow().plugins.desired_enabled(&key)
            },
            Duration::from_secs(5),
        );
    }
    wait_for(
        || {
            ctx.state.borrow().plugins.supervised_count() == 0
                && [
                    Scope::AppHeader,
                    Scope::AppFooter,
                    Scope::PaneFooter(PaneId(1)),
                ]
                .iter()
                .all(|scope| ctx.state.borrow().store.is_empty(*scope))
        },
        Duration::from_secs(5),
    );
    input_proof(&ctx, &terminal, &pty, "after-disable");
    let saved_disabled = std::fs::read(&config).unwrap();
    for plugin in &catalogue.plugins {
        let row = manager_row(&preferences, &plugin.versions[0].manifest.name);
        focus(&preferences);
        press(&named_button(&row, "plugin-info"));
        // the plugin page is a subpage of the same window.
        let details = preferences.clone();
        wait_for(
            || {
                widgets(details.upcast_ref())
                    .iter()
                    .any(|widget| widget.is::<adw::ExpanderRow>() && widget.is_mapped())
            },
            Duration::from_secs(3),
        );
        focus(&details);
        press(&button(&details, "Uninstall"));
        wait_for(
            || details.visible_dialog().is_some(),
            Duration::from_secs(2),
        );
        press(&button(&details.visible_dialog().unwrap(), "Cancel"));
        wait_for(
            || details.visible_dialog().is_none(),
            Duration::from_secs(2),
        );
        assert!(crate::plugin::inventory::read(&root)
            .unwrap()
            .iter()
            .any(|entry| entry.receipt.plugin_id == plugin.id));
        press(&button(&details, "Uninstall"));
        wait_for(
            || details.visible_dialog().is_some(),
            Duration::from_secs(2),
        );
        press(&button(&details.visible_dialog().unwrap(), "Uninstall"));
        // the page closes when the removal commits; no result dialog appears.
        wait_for(
            || {
                !crate::plugin::inventory::read(&root)
                    .unwrap()
                    .iter()
                    .any(|entry| entry.receipt.plugin_id == plugin.id)
            },
            Duration::from_secs(5),
        );
        wait_for(
            || {
                !widgets(details.upcast_ref())
                    .iter()
                    .any(|widget| widget.is::<adw::ExpanderRow>() && widget.is_mapped())
            },
            Duration::from_secs(5),
        );
        assert!(!crate::plugin::inventory::read(&root)
            .unwrap()
            .iter()
            .any(|entry| entry.receipt.plugin_id == plugin.id));
        assert_eq!(std::fs::read(&config).unwrap(), saved_disabled);
    }
    assert!(crate::plugin::inventory::read(&root).unwrap().is_empty());
    assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
    assert!(ctx.state.borrow().plugins.listing().is_empty());
    input_proof(&ctx, &terminal, &pty, "after-uninstall");
    let evidence = crate::test_isolation::check_root().join("published-release-lifecycle.json");
    std::fs::write(&evidence, serde_json::to_vec_pretty(&json!({"catalogue_revision":revision,"source_revision":revision,"release_snapshot":snapshot,"release_binding":binding,"architecture":architecture,"installed_receipts":receipts,"actual_public_install":true,"candidate_bytes":observed().inputs["candidate"] == true,"explicit_review_cancel_accept":true,"three_mapped_outputs":true,"disabled_then_uninstalled":true,"same_pty_real_input":true,"scope":"native installed lifecycle, not package/source/legal or whole RC acceptance"})).unwrap()).unwrap();
    preferences.destroy();
    ctx.window.destroy();
    println!("PASS published_release_ui install_review_enable_disable_uninstall=true terminal_input=true");
    println!("published_release_ui_evidence={}", evidence.display());
}
