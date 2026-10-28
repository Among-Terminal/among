use super::*;
use crate::plugin::install::metadata::{Architecture, Catalogue, Platform};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const CATALOGUE_REVISION: &str = "86f784e6a0eb794cf070edb676207b7964b8317c";
const CATALOGUE_SHA256: &str = "510bdef3b532ed266d10fd925bf15a0d7d536c1889f2454e8d9a7cb446cf8a5b";
const SOURCE: &str = "aa1ec272d4ed6326338f3de0e5fbf30bbe181e39";
const TAG_OBJECT: &str = "31b3ad165bb6a685dfe49fcde082949e12e5887b";

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

fn read_regular(path: &Path, maximum: u64) -> Vec<u8> {
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
    std::fs::write(profile.join("profile.json"), serde_json::to_vec_pretty(&json!({"kind":"observed_published_installation_profile","host_source_sha":option_env!("AMONG_CHECK_SOURCE_SHA"),"catalogue_revision":CATALOGUE_REVISION,"source_revision":SOURCE,"architecture":match Platform::current().unwrap().architecture {Architecture::Amd64=>"amd64",Architecture::Arm64=>"arm64"},"installed_size":installed.len(),"installed_sha256":crate::plugin::inventory::checksum(&installed).unwrap(),"settings_size":settings.len(),"settings_sha256":crate::plugin::inventory::checksum(&settings).unwrap()})).unwrap()).unwrap();
}

fn inspect(preferences: &adw::PreferencesWindow) -> adw::PreferencesWindow {
    focus(preferences);
    press(&button(preferences, "Inspect repository…"));
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
    assert!(has_label(view.upcast_ref(), "Metadata preview ready"));
    let output = widgets(view.upcast_ref())
        .into_iter()
        .find_map(|widget| widget.downcast::<gtk::TextView>().ok())
        .unwrap()
        .buffer();
    let preview = output.text(&output.start_iter(), &output.end_iter(), false);
    assert!(
        preview.contains(&format!("at {CATALOGUE_REVISION}")),
        "published catalogue binding mismatch: expected {CATALOGUE_REVISION}; preview prefix: {:?}",
        preview.chars().take(4096).collect::<String>()
    );
    view
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

pub(super) fn run() {
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
    assert_eq!(inputs["schema_version"], 1);
    assert_eq!(inputs["kind"], "published_release_inputs_not_installed");
    assert_eq!(inputs["repository_id"], 1431055733u64);
    assert_eq!(inputs["publisher_id"], 339567100u64);
    assert_eq!(inputs["catalogue_revision"], CATALOGUE_REVISION);
    assert_eq!(inputs["catalogue_sha256"], CATALOGUE_SHA256);
    assert_eq!(inputs["source_revision"], SOURCE);
    assert_eq!(inputs["release_id"], 424255611);
    assert_eq!(inputs["tag"], "v0.1.0-rc.1");
    assert_eq!(inputs["tag_object"], TAG_OBJECT);
    assert_eq!(inputs["architecture"], architecture);
    let bytes = read_regular(&inputs_root.join("among-plugins.json"), 1024 * 1024);
    assert_eq!(bytes.len(), 634525);
    assert_eq!(
        crate::plugin::inventory::checksum(&bytes).unwrap(),
        CATALOGUE_SHA256
    );
    let catalogue = Catalogue::parse(&bytes).unwrap();
    assert_eq!(catalogue.plugins.len(), 3);
    assert_eq!(inputs["plugins"].as_array().unwrap().len(), 3);
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
    let view = inspect(&preferences);
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
        assert_eq!(expected_input["asset_id"], artifact.asset_id);
        assert_eq!(expected_input["version"], version.manifest.version);
        assert_eq!(expected_input["revision"], SOURCE);
        assert_eq!(expected_input["architecture"], architecture);
        let archive = inputs_root.join(format!("{}.tar.gz", plugin.id));
        assert_eq!(
            crate::plugin::inventory::checksum(&read_regular(&archive, 64 * 1024 * 1024)).unwrap(),
            artifact.sha256
        );
        let expected = format!(
            "github:1431055733/{} · version {} · linux/{architecture} · asset {} · {} bytes",
            plugin.id, version.manifest.version, artifact.asset_id, artifact.size
        );
        let row = widgets(view.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<adw::ActionRow>().ok())
            .find(|row| row.subtitle().as_deref() == Some(expected.as_str()))
            .unwrap();
        assert_eq!(ctx.state.borrow().plugins.supervised_count(), 0);
        press(&button(&row, "Install disabled"));
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
        assert_eq!(receipt.repository_id, 1431055733);
        assert_eq!(receipt.release_id, 424255611);
        assert_eq!(receipt.asset_id, artifact.asset_id);
        assert_eq!(receipt.revision, SOURCE);
        assert_eq!(receipt.manifest_sha256, version.manifest_sha256);
        assert_eq!(receipt.package_sha256, artifact.sha256);
        let source = receipt.source.as_ref().unwrap();
        assert_eq!(source.catalogue_revision, CATALOGUE_REVISION);
        assert_eq!(source.catalogue_sha256, CATALOGUE_SHA256);
        assert_eq!(source.publisher_id, 339567100);
        let binding = source.release_binding.as_ref().unwrap();
        assert_eq!(binding.object, TAG_OBJECT);
        assert_eq!(binding.commit, SOURCE);
        assert_eq!(binding.tag, "v0.1.0-rc.1");
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
        assert_eq!(
            controls.len(),
            version.manifest.required_features.len() + version.manifest.optional_features.len()
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
        press(&button(&row, "Details…"));
        let find_details = || {
            gtk::Window::list_toplevels()
                .into_iter()
                .filter_map(|window| window.downcast::<adw::PreferencesWindow>().ok())
                .find(|window| {
                    window.title().as_deref() == Some("Plugin details") && window.is_visible()
                })
        };
        wait_for(|| find_details().is_some(), Duration::from_secs(2));
        let details = find_details().unwrap();
        focus(&details);
        press(&button(&details, "Uninstall…"));
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
        press(&button(&details, "Uninstall…"));
        wait_for(
            || details.visible_dialog().is_some(),
            Duration::from_secs(2),
        );
        press(&button(&details.visible_dialog().unwrap(), "Uninstall"));
        wait_for(
            || {
                details
                    .visible_dialog()
                    .is_some_and(|dialog| has_label(dialog.upcast_ref(), "Plugin uninstalled"))
            },
            Duration::from_secs(5),
        );
        press(&button(&details.visible_dialog().unwrap(), "Close"));
        details.destroy();
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
    std::fs::write(&evidence, serde_json::to_vec_pretty(&json!({"catalogue_revision":CATALOGUE_REVISION,"source_revision":SOURCE,"architecture":architecture,"installed_receipts":receipts,"actual_public_install":true,"explicit_review_cancel_accept":true,"three_mapped_outputs":true,"disabled_then_uninstalled":true,"same_pty_real_input":true,"scope":"native installed lifecycle, not package/source/legal or whole RC acceptance"})).unwrap()).unwrap();
    preferences.destroy();
    ctx.window.destroy();
    println!("PASS published_release_ui install_review_enable_disable_uninstall=true terminal_input=true");
    println!("published_release_ui_evidence={}", evidence.display());
}
