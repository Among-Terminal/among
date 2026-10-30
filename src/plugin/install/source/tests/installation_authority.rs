use super::*;
use crate::plugin::install::{
    mutation::{Action, Job as Mutation},
    package::tests::OwnedRoot,
};
use crate::plugin::{
    host::PluginHost,
    inventory::{self, Receipt},
};
use serde_json::json;
use std::{cell::Cell, path::Path, rc::Rc};

const KEY: &str = "github:12/sample";
const CHILD: &str =
    "plugin::install::source::tests::installation_authority::cooperating_reinstall_worker";
const CHILD_ROOT: &str = "AMONG_L1_WRITER_ROOT";
const CHILD_VERSION: &str = "AMONG_L1_WRITER_VERSION";

fn wait(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "real installation-authority boundary timed out"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn stop(host: &mut PluginHost) {
    host.shutdown();
    wait(|| {
        host.poll();
        host.is_stopped()
    });
}

fn install(root: &Path, version: &str, previous: Option<&Receipt>) -> Receipt {
    let (metadata, archive) =
        super::super::super::package::tests::managed_fixture(version, "managed-resource", |_| {});
    let server =
        Server::new(move |request| package_route(metadata.clone(), archive.clone(), request));
    let mut selected = server
        .inspect(true)
        .unwrap()
        .select("sample", version)
        .unwrap();
    if let Some(previous) = previous {
        let entry = inventory::read(root)
            .unwrap()
            .into_iter()
            .find(|entry| &entry.receipt == previous)
            .unwrap();
        selected = selected.for_update(&entry, None).unwrap();
    }
    let job = server.install(selected, root.into()).unwrap();
    let mut result = None;
    wait(|| {
        result = job.poll();
        result.is_some()
    });
    let receipt = result.unwrap().unwrap().receipt;
    assert!(server
        .requests()
        .iter()
        .any(|request| request.contains("/github-production-release-asset/12/")));
    assert_eq!(
        receipt
            .release()
            .and_then(|release| release.source.as_ref())
            .unwrap()
            .release_binding
            .commit,
        receipt.revision
    );
    receipt
}

fn remove_with_actual_stop_ack(root: &Path, expected: &Receipt) {
    let mut owner = PluginHost::new();
    owner.register_managed(inventory::read(root).unwrap().remove(0), None);
    let job = Mutation::start(root.into(), expected.clone(), Action::Uninstall, None).unwrap();
    owner.begin_removal(expected).unwrap();
    wait(|| {
        owner.poll();
        owner.supervised_stopped(KEY)
    });
    job.supervised_stopped();
    let mut result = None;
    wait(|| {
        result = job.poll();
        result.is_some()
    });
    result.unwrap().unwrap();
    assert!(inventory::read(root).unwrap().is_empty());
}

fn preferences(root: &Path, receipt: &Receipt) -> (std::path::PathBuf, Value) {
    use std::os::unix::fs::PermissionsExt;
    let config = root.join("config");
    std::fs::create_dir(&config).unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o700)).unwrap();
    let local = json!({"version":1,"future":"keep","plugins":{KEY:{"enabled":true,"installation":receipt.installation,
        "review":receipt.review(vec!["status".into(),"settings".into()]),"future":"keep",
        "settings":{"version":1,"values":{"label":"retained","unknown":"preserve"},"future":42}},
        "github:13/sample":{"enabled":true,"settings":{"unrelated":"keep"}}}});
    let path = config.join("managed-plugins.json");
    std::fs::write(&path, local.to_string()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(config.join("plugins.json"), b"opaque legacy settings bytes").unwrap();
    let loaded = crate::settings::load(&path, crate::settings::Document::ManagedPlugins).unwrap();
    (path, loaded["plugins"][KEY].clone())
}

fn reinstall(root: &Path, old: &Receipt, version: &str, separate_process: bool) -> Receipt {
    if separate_process {
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                CHILD,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_ROOT, root)
            .env(CHILD_VERSION, version)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{} {}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        print!("{}", String::from_utf8_lossy(&child.stdout));
        inventory::read(root).unwrap().remove(0).receipt
    } else {
        remove_with_actual_stop_ack(root, old);
        install(root, version, None)
    }
}

#[test]
#[ignore = "called only by the owning authority regression with an isolated shared root"]
fn cooperating_reinstall_worker() {
    let root = PathBuf::from(std::env::var_os(CHILD_ROOT).unwrap());
    assert!(root.is_absolute() && root.starts_with(std::env::temp_dir()));
    let version = std::env::var(CHILD_VERSION).unwrap();
    let old = inventory::read(&root).unwrap().remove(0).receipt;
    remove_with_actual_stop_ack(&root, &old);
    let fresh = install(&root, &version, None);
    assert_ne!(fresh.installation, old.installation);
    assert!(!fresh.package_path(&root).join("executed").exists());
    println!("L1 separate writer pid={} old={:?} fresh={:?}; actual stop acknowledgement, TLS fresh commit, no execution",std::process::id(), old.installation, fresh.installation);
}

fn missed_absence_case(
    separate_process: bool,
    observer_running: bool,
    identical_payload: bool,
) -> (bool, bool) {
    let root = OwnedRoot::new();
    let old = install(&root.0, "1.0", None);
    let (path, local) = preferences(&root.0, &old);
    let before = std::fs::read(&path).unwrap();
    let gate = Rc::new(Cell::new(observer_running));
    let mut observer = PluginHost::with_start_gate(gate.clone());
    observer.register_managed(inventory::read(&root.0).unwrap().remove(0), Some(&local));
    assert!(observer.desired_enabled(KEY));
    if observer_running {
        wait(|| {
            observer.poll();
            observer.running_generation(KEY).is_some()
        });
        observer.request_status(KEY, "app_header").unwrap();
    } else {
        assert_eq!(observer.supervised_count(), 0);
    }
    let fresh = reinstall(
        &root.0,
        &old,
        if identical_payload { "1.0" } else { "2.0" },
        separate_process,
    );
    assert_ne!(fresh.installation, old.installation);
    if identical_payload {
        assert_eq!(fresh.package_sha256, old.package_sha256);
        assert_eq!(fresh.manifest_sha256, old.manifest_sha256);
    }
    let entries = inventory::read(&root.0).unwrap();
    observer.reconcile_removed(&entries.iter().map(|entry| entry.key()).collect());
    observer.reconcile_managed(entries[0].clone(), Some(&local));
    let desired = observer.desired_enabled(KEY);
    gate.set(true);
    wait(|| {
        observer.poll();
        observer.running_generation(KEY).is_some() || !observer.desired_enabled(KEY)
    });
    let until = Instant::now() + Duration::from_millis(200);
    while Instant::now() < until {
        observer.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    let pids = observer.supervised_pids();
    stop(&mut observer);
    let executed = fresh.package_path(&root.0).join("executed").exists();
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        std::fs::read(path.parent().unwrap().join("plugins.json")).unwrap(),
        b"opaque legacy settings bytes"
    );
    assert_eq!(
        observer.managed_details(KEY).unwrap().1,
        Some(local["settings"].clone())
    );
    println!("L1 observed missed absence: separate_process={separate_process} old_running={observer_running} identical_payload={identical_payload} old={:?} fresh={:?} desired={desired} executed={executed} pids={pids:?}",old.installation,fresh.installation);
    (desired, executed)
}

#[test]
fn existing_observer_must_not_treat_a_fresh_installation_token_as_an_update() {
    let _serial = JOB_TESTS.lock().unwrap();
    let (desired, executed) = missed_absence_case(false, false, false);
    assert!(
        !desired && !executed,
        "fresh/reinstalled selection inherited existing host enablement without new-token consent"
    );
}

#[test]
fn missed_absence_separate_writer_live_process_and_identical_payload_all_require_fresh_consent() {
    let _serial = JOB_TESTS.lock().unwrap();
    let mut violations = Vec::new();
    for (separate, running, identical) in [
        (true, false, false),
        (false, true, false),
        (false, false, true),
        (true, true, true),
    ] {
        let (desired, executed) = missed_absence_case(separate, running, identical);
        if desired || executed {
            violations.push((separate, running, identical));
        }
    }
    assert!(
        violations.is_empty(),
        "changed installation authority inherited old enablement: {violations:?}"
    );
}

#[test]
fn same_installation_updates_preserve_live_enablement_and_later_disable_instead_of_stale_preferences(
) {
    let _serial = JOB_TESTS.lock().unwrap();
    let root = OwnedRoot::new();
    let old = install(&root.0, "1.0", None);
    let (path, local) = preferences(&root.0, &old);
    let before = std::fs::read(&path).unwrap();
    let gate = Rc::new(Cell::new(false));
    let mut observer = PluginHost::with_start_gate(gate.clone());
    observer.register_managed(inventory::read(&root.0).unwrap().remove(0), Some(&local));
    let update = install(&root.0, "2.0", Some(&old));
    assert_eq!(update.installation, old.installation);
    observer.reconcile_managed(inventory::read(&root.0).unwrap().remove(0), Some(&local));
    assert!(observer.desired_enabled(KEY));
    gate.set(true);
    wait(|| {
        observer.poll();
        observer.running_generation(KEY).is_some()
            && update.package_path(&root.0).join("executed").exists()
    });
    observer.disable(KEY);
    wait(|| {
        observer.poll();
        observer.supervised_stopped(KEY)
    });
    gate.set(false);
    let later = install(&root.0, "3.0", Some(&update));
    assert_eq!(later.installation, old.installation);
    observer.reconcile_managed(inventory::read(&root.0).unwrap().remove(0), Some(&local));
    assert!(
        !observer.desired_enabled(KEY),
        "a stale same-token enabled preference cannot undo the current live disable"
    );
    gate.set(true);
    let until = Instant::now() + Duration::from_millis(200);
    while Instant::now() < until {
        observer.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!later.package_path(&root.0).join("executed").exists());
    assert_eq!(observer.supervised_count(), 0);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    stop(&mut observer);
    println!("PASS actual TLS same-installation updates retain explicit live ON, then live OFF despite stale same-token saved ON; only the explicitly desired generation executes");
}

#[test]
fn fresh_token_matching_current_explicit_preference_is_admitted_without_reusing_old_authority() {
    let _serial = JOB_TESTS.lock().unwrap();
    let root = OwnedRoot::new();
    let old = install(&root.0, "1.0", None);
    let (path, local) = preferences(&root.0, &old);
    let gate = Rc::new(Cell::new(false));
    let mut observer = PluginHost::with_start_gate(gate.clone());
    observer.register_managed(inventory::read(&root.0).unwrap().remove(0), Some(&local));
    let fresh = reinstall(&root.0, &old, "2.0", true);
    let mut storage = crate::settings::PreferencesStorage::new(Some(path.parent().unwrap().into()));
    storage
        .managed_enable_changed(KEY, true, fresh.installation.as_deref())
        .unwrap();
    let document = storage
        .load(crate::settings::Document::ManagedPlugins)
        .unwrap();
    let current = inventory::local(&document, KEY).unwrap();
    assert_ne!(current["installation"], local["installation"]);
    assert_eq!(current["settings"], local["settings"]);
    assert_eq!(current["review"], local["review"]);
    observer.reconcile_managed(inventory::read(&root.0).unwrap().remove(0), Some(current));
    assert!(
        observer.desired_enabled(KEY),
        "only the matching fresh-token preference supplies this enable authority"
    );
    assert_eq!(observer.supervised_count(), 0);
    assert!(observer
        .drain_invalidated_sources()
        .iter()
        .any(|key| key == KEY));
    gate.set(true);
    wait(|| {
        observer.poll();
        observer.running_generation(KEY).is_some()
            && fresh.package_path(&root.0).join("executed").exists()
    });
    assert_eq!(
        observer.managed_details(KEY).unwrap().1,
        Some(local["settings"].clone())
    );
    stop(&mut observer);
    println!("PASS real separately reinstalled ELF waits for the closed gate, then runs under a newly persisted explicit fresh-token preference, not the retained old token");
}

fn recover(root: &Path) {
    let job = super::super::super::store::RecoveryJob::start(root.into()).unwrap();
    let mut result = None;
    wait(|| {
        result = job.poll();
        result.is_some()
    });
    result.unwrap().unwrap();
}

#[test]
fn live_old_authority_is_revoked_and_reaped_before_fresh_manual_activation_and_cleanup() {
    use crate::plugin::host::Configuration;
    let _serial = JOB_TESTS.lock().unwrap();
    let root = OwnedRoot::new();
    let old = install(&root.0, "1.0", None);
    let (path, local) = preferences(&root.0, &old);
    let before = std::fs::read(&path).unwrap();
    let mut observer = PluginHost::new();
    observer.register_managed(inventory::read(&root.0).unwrap().remove(0), Some(&local));
    wait(|| {
        observer.poll();
        observer.running_generation(KEY).is_some()
            && observer.configuration(KEY) == Configuration::Applied
    });
    let old_generation = observer.running_generation(KEY).unwrap();
    let old_pid = observer.supervised_pids()[0];
    observer.drain_invalidated_sources();
    observer.request_status(KEY, "app_header").unwrap();
    let fresh = reinstall(&root.0, &old, "2.0", true);
    assert_eq!(
        unsafe { libc::kill(old_pid as i32, 0) },
        0,
        "another owner's stop acknowledgement does not stop this observer's real old process"
    );
    recover(&root.0);
    assert!(
        old.package_path(&root.0).exists(),
        "the observer/process lease prevents cleanup beneath the still-live old generation"
    );
    observer.reconcile_managed(inventory::read(&root.0).unwrap().remove(0), Some(&local));
    assert!(!observer.desired_enabled(KEY));
    assert!(observer.drain_results().is_empty());
    assert!(observer.drain_notices().is_empty());
    assert!(observer
        .drain_invalidated_sources()
        .iter()
        .any(|key| key == KEY));
    assert!(observer.request_status(KEY, "app_header").is_none());
    assert_eq!(observer.configuration(KEY), Configuration::NotApplied);
    wait(|| {
        observer.poll();
        observer.supervised_stopped(KEY)
    });
    assert_ne!(unsafe { libc::kill(old_pid as i32, 0) }, 0);
    assert!(!fresh.package_path(&root.0).join("executed").exists());
    assert_eq!(
        observer.managed_details(KEY).unwrap().1,
        Some(local["settings"].clone())
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let deadline = Instant::now() + Duration::from_secs(5);
    while old.package_path(&root.0).exists() {
        assert!(Instant::now() < deadline);
        recover(&root.0);
        std::thread::sleep(Duration::from_millis(2));
    }
    let mut storage = crate::settings::PreferencesStorage::new(Some(path.parent().unwrap().into()));
    storage
        .managed_enable_changed(KEY, true, fresh.installation.as_deref())
        .unwrap();
    observer.enable(KEY);
    wait(|| {
        observer.poll();
        observer.running_generation(KEY).is_some()
            && observer.configuration(KEY) == Configuration::Applied
    });
    assert_ne!(observer.running_generation(KEY).unwrap(), old_generation);
    assert!(fresh.package_path(&root.0).join("executed").exists());
    observer.request_status(KEY, "app_header").unwrap();
    wait(|| {
        observer.poll();
        fresh
            .package_path(&root.0)
            .join("resource-observed")
            .exists()
    });
    assert_eq!(
        std::fs::read_to_string(fresh.package_path(&root.0).join("resource-observed")).unwrap(),
        "2.0"
    );
    assert_eq!(
        std::fs::read(path.parent().unwrap().join("plugins.json")).unwrap(),
        b"opaque legacy settings bytes"
    );
    stop(&mut observer);
    println!("PASS live old runtime, pending status and process are revoked/reaped by shared reconciliation before fresh activation; source invalidation reaches the sink seam, occupied generation stays leased then cleans only after reaping, actual explicit fresh enable starts/configures the new ELF with retained settings");
}

#[test]
fn fresh_selection_survives_an_observed_absence_while_the_old_process_is_still_stopping() {
    let _serial = JOB_TESTS.lock().unwrap();
    let root = OwnedRoot::new();
    let old = install(&root.0, "1.0", None);
    let (_, local) = preferences(&root.0, &old);
    let mut observer = PluginHost::new();
    observer.register_managed(inventory::read(&root.0).unwrap().remove(0), Some(&local));
    wait(|| {
        observer.poll();
        observer.running_generation(KEY).is_some()
    });
    remove_with_actual_stop_ack(&root.0, &old);
    observer.reconcile_removed(&Default::default());
    assert!(
        observer.managed_details(KEY).is_some(),
        "the retained old IO still owns its pending removal entry"
    );
    let fresh = install(&root.0, "2.0", None);
    observer.reconcile_managed(inventory::read(&root.0).unwrap().remove(0), Some(&local));
    wait(|| {
        observer.poll();
        observer.supervised_stopped(KEY)
    });
    assert_eq!(observer.managed_details(KEY).unwrap().0.receipt, fresh);
    assert!(!observer.desired_enabled(KEY));
    assert!(!fresh.package_path(&root.0).join("executed").exists());
    stop(&mut observer);
    println!("PASS fresh selected entry is not discarded by the old removal marker after the previous IO is reaped");
}

#[test]
fn changed_installation_authority_does_not_replace_identity_scoped_accepted_settings() {
    let _serial = JOB_TESTS.lock().unwrap();
    for observation in [
        "stale saved values",
        "only consent fields",
        "preferences unavailable",
    ] {
        let root = OwnedRoot::new();
        let old = install(&root.0, "1.0", None);
        let (path, mut local) = preferences(&root.0, &old);
        let before = std::fs::read(&path).unwrap();
        let gate = Rc::new(Cell::new(false));
        let mut observer = PluginHost::with_start_gate(gate.clone());
        observer.register_managed(inventory::read(&root.0).unwrap().remove(0), Some(&local));
        // the same cache consumer accepts session-only Save results. package
        // authority changes must not roll it back to a stale disk observation.
        let accepted =
            json!({"version":1,"values":{"label":"session-only","unknown":"retain"},"future":42});
        observer.stored_settings_changed(KEY, accepted.clone());
        let fresh = reinstall(&root.0, &old, "2.0", false);
        if observation == "only consent fields" {
            local.as_object_mut().unwrap().remove("settings");
        }
        observer.reconcile_managed(
            inventory::read(&root.0).unwrap().remove(0),
            if observation == "preferences unavailable" {
                None
            } else {
                Some(&local)
            },
        );
        assert!(!observer.desired_enabled(KEY));
        assert_eq!(observer.managed_details(KEY).unwrap().1, Some(accepted));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        gate.set(true);
        observer.poll();
        assert_eq!(observer.supervised_count(), 0);
        assert!(!fresh.package_path(&root.0).join("executed").exists());
        stop(&mut observer);
        println!("PASS authority reset preserves accepted identity-scoped cache with {observation}, without accepting old-token enablement or changing preference bytes");
    }
}
