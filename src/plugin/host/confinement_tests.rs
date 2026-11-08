//! the host's confinement behaviour on real processes: the hostile fixture
//! runs as a managed plugin under a test host that is asked to confine.

use super::*;
use crate::plugin::sandbox::level::{Probe, Protection};
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const KEY: &str = "github:12/sample";
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Confined {
    host: PluginHost,
    root: PathBuf,
    data_home: PathBuf,
}

impl Drop for Confined {
    fn drop(&mut self) {
        self.host.shutdown();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.host.is_stopped() && Instant::now() < deadline {
            self.host.poll();
            std::thread::sleep(Duration::from_millis(2));
        }
        if self.host.is_stopped() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

struct Setup<'a> {
    review: &'a [&'a str],
    actions: &'a str,
    probe: Option<Probe>,
    local: Value,
}

impl Default for Setup<'_> {
    fn default() -> Self {
        Self {
            review: &["status"],
            actions: "environment\ndata_dir",
            probe: None,
            local: json!({}),
        }
    }
}

pub(super) fn kernel_offers_full_protection() -> bool {
    let best = crate::plugin::sandbox::level::probe().best();
    if best.is_full() {
        return true;
    }
    assert!(
        std::env::var_os("AMONG_REQUIRE_SANDBOX").is_none(),
        "AMONG_REQUIRE_SANDBOX is set but this kernel offers only {}",
        best.label()
    );
    eprintln!("NOT VERIFIED: this kernel offers only {}", best.label());
    false
}

fn confined(setup: Setup<'_>) -> Confined {
    let base = std::env::var_os("AMONG_CHECK_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = base.join(format!(
        "confined-host-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut receipt = inventory::tests::fixture(&root);
    let package = receipt.package_path(&root);
    let executable = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/sandbox-hostile");
    assert!(
        executable.is_file(),
        "build the sandbox-hostile example first"
    );
    std::fs::copy(executable, package.join("plugin")).unwrap();
    std::fs::set_permissions(
        package.join("plugin"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    std::fs::write(package.join("actions"), setup.actions).unwrap();
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(package.join("among-plugin.json")).unwrap()).unwrap();
    manifest.as_object_mut().unwrap().remove("settings");
    manifest["required_features"] = json!(["status"]);
    manifest["optional_features"] = json!(["system.metrics"]);
    let bytes = manifest.to_string();
    std::fs::write(package.join("among-plugin.json"), &bytes).unwrap();
    receipt.manifest_sha256 = inventory::checksum(bytes.as_bytes()).unwrap();
    std::fs::write(
        root.join("installed.json"),
        json!({"version":1,"installations":[receipt]}).to_string(),
    )
    .unwrap();
    let entry = inventory::read(&root).unwrap().remove(0);
    let data_home = root.join("data-home");
    std::fs::create_dir(&data_home).unwrap();
    std::fs::set_permissions(&data_home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut local = json!({
        "enabled": true,
        "review": {"owner":"example","repository":"sample","features": setup.review}
    });
    for (key, value) in setup.local.as_object().into_iter().flatten() {
        local[key] = value.clone();
    }
    let mut host = PluginHost::new();
    host.confine_in_tests(data_home.clone());
    if let Some(probe) = setup.probe {
        host.probe_in_tests(probe);
    }
    host.register_managed(entry, Some(&local));
    Confined {
        host,
        root,
        data_home,
    }
}

impl Confined {
    fn wait(&mut self, predicate: impl Fn(&PluginHost) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            self.host.poll();
            if predicate(&self.host) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "confined fixture timed out: {:?}",
                self.host.listing()
            );
            std::thread::sleep(Duration::from_millis(3));
        }
    }

    fn state(&self) -> PluginState {
        self.host.plugins[0].state.clone()
    }

    /// Asks the running plugin for a status and returns what its actions said.
    fn answers(&mut self) -> Vec<String> {
        self.wait(|host| host.plugins[0].state.is_healthy());
        self.host.request_status(KEY, "app_header").unwrap();
        self.wait(|host| !host.results.is_empty());
        let text = self.host.results[0].fragments[0].text.clone().unwrap();
        self.host.drain_results();
        text.split(';').map(str::to_string).collect()
    }

    fn data_dir(&self) -> PathBuf {
        self.data_home.join("among/plugin-data/12/sample")
    }
}

#[test]
fn a_confined_start_gives_the_plugin_its_own_directory_and_a_minimal_environment() {
    if !kernel_offers_full_protection() {
        return;
    }
    let mut confined = confined(Setup::default());
    let answers = confined.answers();
    // the parent's TZ is forwarded only when it has one.
    let names = answers[0].strip_prefix("environment ok:").unwrap();
    let names: Vec<&str> = names.split(',').filter(|name| *name != "TZ").collect();
    assert_eq!(names, ["AMONG_PLUGIN_DATA_DIR", "LANG", "PATH"]);
    assert_eq!(
        answers[1],
        format!("data_dir ok:{}", confined.data_dir().display())
    );
    let mode = std::fs::metadata(confined.data_dir())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o7777, 0o700);
}

#[test]
fn the_launch_record_names_identity_generation_grants_digest_and_what_the_child_applied() {
    if !kernel_offers_full_protection() {
        return;
    }
    let mut confined = confined(Setup::default());
    confined.answers();
    let generation = confined.host.running_generation(KEY).unwrap();
    let record = confined.host.confinement_record(KEY).unwrap();
    assert_eq!(record.identity, KEY);
    assert_eq!(record.generation.as_deref(), Some(generation.as_str()));
    assert_eq!(record.grants, ["status"]);
    let digest = record.digest.clone().unwrap();
    assert_eq!(digest.len(), 64);
    assert_eq!(
        digest,
        crate::plugin::sandbox::policy::digest(&["status".to_string()].into())
    );
    let applied = record.applied.clone().unwrap();
    assert!(applied.protection.is_full());
    assert!(applied.report.seccomp_loaded);
    assert_eq!(applied.digest, digest);
    assert!(record.refusal.is_none());
    let overview = confined.host.overview().remove(0);
    assert!(overview
        .protection
        .as_deref()
        .unwrap()
        .starts_with("Protection: full (Landlock ABI "));
    let details = overview.protection_details.unwrap();
    for expected in [
        "Identity: github:12/sample",
        "Generation: ",
        "Grants: status",
        "Policy digest: ",
        "Landlock: ABI ",
        "seccomp filter: loaded",
    ] {
        assert!(
            details.contains(expected),
            "{expected} missing from {details}"
        );
    }
}

#[test]
fn system_metrics_opens_exactly_the_three_files_and_only_with_the_grant() {
    if !kernel_offers_full_protection() {
        return;
    }
    let actions = "read /proc/stat\nread /proc/meminfo\nread /proc/loadavg\nread /proc/self/environ\nread /proc/self/status";
    let mut without = confined(Setup {
        actions,
        ..Setup::default()
    });
    let denied = without.answers();
    assert!(
        denied.iter().all(|line| line.ends_with("err:EACCES")),
        "{denied:?}"
    );
    let mut with = confined(Setup {
        actions,
        review: &["status", "system.metrics"],
        ..Setup::default()
    });
    let granted = with.answers();
    for line in &granted[..3] {
        assert!(line.contains(" ok:"), "{line}");
    }
    for line in &granted[3..] {
        assert!(line.ends_with("err:EACCES"), "{line}");
    }
    assert_eq!(
        with.host.confinement_record(KEY).unwrap().grants,
        ["status", "system.metrics"]
    );
}

#[test]
fn a_changed_grant_applies_to_the_next_process_after_the_current_one_stops() {
    if !kernel_offers_full_protection() {
        return;
    }
    let mut confined = confined(Setup {
        actions: "read /proc/stat",
        ..Setup::default()
    });
    assert!(confined.answers()[0].ends_with("err:EACCES"));
    // changing access while the generation runs is refused.
    let review =
        json!({"owner":"example","repository":"sample","features":["status","system.metrics"]});
    assert!(confined.host.review_access(KEY, review.clone()).is_err());
    confined.host.disable(KEY);
    confined.wait(|host| host.plugins[0].io.is_none());
    confined.host.review_access(KEY, review).unwrap();
    confined.host.enable(KEY);
    assert!(confined.answers()[0].contains(" ok:"));
}

#[test]
fn a_symbolic_link_in_the_data_directory_path_refuses_the_start() {
    if !kernel_offers_full_protection() {
        return;
    }
    let base = std::env::var_os("AMONG_CHECK_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let outside = base.join(format!("link-target-{}", std::process::id()));
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o700)).unwrap();
    let confined = confined_with_link(&outside);
    let reason = match confined.state() {
        PluginState::Unavailable { reason } => reason,
        other => panic!("expected an unavailable plugin, got {other:?}"),
    };
    assert!(reason.contains("link"), "{reason}");
    assert!(confined.host.plugins[0].io.is_none());
    assert!(
        std::fs::read_dir(&outside).unwrap().next().is_none(),
        "nothing was created through the link"
    );
    let record = confined.host.confinement_record(KEY).unwrap();
    assert_eq!(record.refusal.as_deref(), Some(reason.as_str()));
    let _ = std::fs::remove_dir_all(&outside);
    drop(confined.host.overview());
}

fn confined_with_link(outside: &std::path::Path) -> Confined {
    // lay the store out, then make `among` in the data home a link before the
    // start happens: registration starts the plugin, so the link is placed by
    // a setup that registers disabled first.
    let confined = confined(Setup {
        local: json!({"enabled": false}),
        ..Setup::default()
    });
    std::os::unix::fs::symlink(outside, confined.data_home.join("among")).unwrap();
    let mut confined = confined;
    confined.host.enable(KEY);
    confined
}

#[test]
fn an_older_kernel_does_not_start_the_plugin_without_consent_and_offers_the_level() {
    let mut confined = confined(Setup {
        probe: Some(Probe {
            landlock_abi: Some(2),
            seccomp: true,
        }),
        ..Setup::default()
    });
    let PluginState::Unavailable { reason } = confined.state() else {
        panic!("expected Unavailable, got {:?}", confined.state());
    };
    assert_eq!(
        reason,
        "the kernel supports only Landlock ABI 2 (required: ABI 3, kernel 6.2 or newer); \
         the plugin was not started. Allow starting with reduced protection to run it with \
         reduced (Landlock ABI 2, below the required 3, and seccomp)."
    );
    assert!(confined.host.plugins[0].io.is_none());
    let overview = confined.host.overview().remove(0);
    assert_eq!(overview.consent_offer, Some(Protection::Reduced { abi: 2 }));
    // consent for another level than the one offered is refused.
    assert!(confined
        .host
        .allow_reduced_protection(KEY, Protection::Reduced { abi: 1 })
        .is_err());
    assert!(confined
        .host
        .allow_reduced_protection(KEY, Protection::Full { abi: 3 })
        .is_err());
    assert_eq!(confined.host.consented_protection(KEY), None);
    drop(confined.state());
}

#[test]
fn consent_starts_the_plugin_and_the_card_keeps_marking_reduced_protection() {
    if !kernel_offers_full_protection() {
        // the child applies this kernel's rules; only the level logic is
        // injected, which needs a kernel that can run the plugin at all.
        return;
    }
    let mut confined = confined(Setup {
        probe: Some(Probe {
            landlock_abi: Some(2),
            seccomp: true,
        }),
        ..Setup::default()
    });
    confined
        .host
        .allow_reduced_protection(KEY, Protection::Reduced { abi: 2 })
        .unwrap();
    assert_eq!(
        confined.host.consented_protection(KEY),
        Some(Protection::Reduced { abi: 2 })
    );
    confined.answers();
    let overview = confined.host.overview().remove(0);
    assert_eq!(overview.consent_offer, None);
    let line = overview.protection.unwrap();
    assert!(line.contains("running with reduced protection"), "{line}");
    assert!(
        line.starts_with("Protection: reduced (Landlock ABI 2"),
        "{line}"
    );
}

#[test]
fn a_stored_consent_is_read_back_and_a_lower_level_asks_again() {
    if !kernel_offers_full_protection() {
        return;
    }
    let mut confined = confined(Setup {
        probe: Some(Probe {
            landlock_abi: Some(2),
            seccomp: true,
        }),
        local: json!({"protection_consent": "reduced:2"}),
        ..Setup::default()
    });
    confined.answers();
    assert!(confined.host.overview()[0].consent_offer.is_none());
    // the same consent on a kernel that now offers less asks again.
    let lower = self::confined(Setup {
        probe: Some(Probe {
            landlock_abi: Some(1),
            seccomp: true,
        }),
        local: json!({"protection_consent": "reduced:2"}),
        ..Setup::default()
    });
    let PluginState::Unavailable { reason } = lower.state() else {
        panic!("a lower level must not start");
    };
    assert!(
        reason.contains("the kernel supports only Landlock ABI 1"),
        "{reason}"
    );
    assert!(
        reason.contains("You allowed reduced (Landlock ABI 2"),
        "{reason}"
    );
    assert_eq!(
        lower.host.overview()[0].consent_offer,
        Some(Protection::Reduced { abi: 1 })
    );
}

#[test]
fn without_a_loadable_seccomp_filter_nothing_starts_even_with_consent() {
    let confined = confined(Setup {
        probe: Some(Probe {
            landlock_abi: Some(6),
            seccomp: false,
        }),
        local: json!({"protection_consent": "seccomp-only"}),
        ..Setup::default()
    });
    let PluginState::Unavailable { reason } = confined.state() else {
        panic!("expected Unavailable");
    };
    assert_eq!(
        reason,
        "the kernel cannot load a seccomp filter; the plugin is not started"
    );
    assert!(confined.host.plugins[0].io.is_none());
    assert_eq!(confined.host.overview()[0].consent_offer, None);
}

/// The officially released plugin binaries, run as managed plugins under
/// confinement. `AMONG_OFFICIAL_PLUGINS` names a directory holding one
/// extracted release package per plugin identifier (`clock/`, `pane-pwd/`,
/// `system-status/`); without it the test says so and checks nothing.
fn official(
    id: &str,
    review: &[&str],
    declare_metrics: bool,
    packages: &std::path::Path,
) -> Confined {
    let base = std::env::var_os("AMONG_CHECK_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = base.join(format!(
        "official-host-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut receipt = inventory::tests::fixture(&root);
    // the official plugins admit only the identity of their own repository.
    inventory::tests::release_of(&mut receipt).repository_id = 1_431_055_733;
    receipt.plugin_id = id.into();
    let package = receipt.package_path(&root);
    std::fs::create_dir_all(&package).unwrap();
    // a group-writable umask must not make the store directories unsafe.
    let mut directory = package.as_path();
    while directory.starts_with(&root) {
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        if directory == root {
            break;
        }
        directory = directory.parent().unwrap();
    }
    let copied = Command::new("cp")
        .arg("-a")
        .arg(format!("{}/.", packages.join(id).display()))
        .arg(&package)
        .status()
        .unwrap();
    assert!(copied.success());
    // the loader accepts only packages closed to other writers.
    assert!(Command::new("chmod")
        .args(["-R", "go-rwx"])
        .arg(&package)
        .status()
        .unwrap()
        .success());
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(package.join("among-plugin.json")).unwrap()).unwrap();
    let already_declared = ["required_features", "optional_features"]
        .iter()
        .filter_map(|key| manifest[key].as_array())
        .flatten()
        .any(|feature| feature == "system.metrics");
    if declare_metrics && !already_declared {
        // a manifest that declares the grant, as the updated release will.
        manifest["optional_features"] = json!(["system.metrics"]);
    }
    let bytes = manifest.to_string();
    std::fs::write(package.join("among-plugin.json"), &bytes).unwrap();
    receipt.manifest_sha256 = inventory::checksum(bytes.as_bytes()).unwrap();
    std::fs::write(
        root.join("installed.json"),
        json!({"version":1,"installations":[receipt]}).to_string(),
    )
    .unwrap();
    let entry = inventory::read(&root).unwrap().remove(0);
    let data_home = root.join("data-home");
    std::fs::create_dir(&data_home).unwrap();
    std::fs::set_permissions(&data_home, std::fs::Permissions::from_mode(0o700)).unwrap();
    // whatever the package requires has to be granted for it to start; the
    // caller names the optional access it wants on top.
    let mut granted: BTreeSet<String> = review.iter().map(|feature| feature.to_string()).collect();
    granted.extend(
        manifest["required_features"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|feature| feature.as_str().map(str::to_string)),
    );
    let local = json!({
        "enabled": true,
        "review": {"owner":"example","repository":"sample","features": granted}
    });
    let mut host = PluginHost::new();
    host.confine_in_tests(data_home.clone());
    host.update_context(
        vec![10],
        Context {
            active_workspace: 0,
            workspace_count: 1,
            focused_pane: Some(1),
            terminals: vec![crate::plugin::TerminalInfo {
                pane: 1,
                workspace: 0,
                cwd: Some("/first".into()),
                title: Some("First".into()),
                running: true,
            }],
        },
    );
    host.register_managed(entry, Some(&local));
    Confined {
        host,
        root,
        data_home,
    }
}

impl Confined {
    /// The text of the first fragment the plugin returns for `scope`.
    fn fragments(&mut self, key: &str, scope: &str) -> Vec<String> {
        self.wait(|host| host.plugins[0].state.is_healthy());
        self.host.results.clear();
        self.host.request_status(key, scope).unwrap();
        self.wait(|host| !host.results.is_empty());
        let texts = self.host.results[0]
            .fragments
            .iter()
            .filter_map(|fragment| fragment.text.clone())
            .collect();
        self.host.drain_results();
        texts
    }
}

#[test]
fn the_released_official_plugins_still_produce_their_fragments_under_confinement() {
    let Some(packages) = std::env::var_os("AMONG_OFFICIAL_PLUGINS").map(PathBuf::from) else {
        assert!(
            std::env::var_os("AMONG_REQUIRE_SANDBOX").is_none(),
            "AMONG_REQUIRE_SANDBOX is set, so the official plugin packages are required: \
             set AMONG_OFFICIAL_PLUGINS to a directory with one extracted package per plugin"
        );
        eprintln!(
            "NOT VERIFIED: AMONG_OFFICIAL_PLUGINS is not set; the released binaries were not run"
        );
        return;
    };
    if !kernel_offers_full_protection() {
        return;
    }
    let clock_key = "github:1431055733/clock";
    let mut clock = official("clock", &["status"], false, &packages);
    let text = clock.fragments(clock_key, "app_footer").remove(0);
    assert_eq!(text.len(), 8, "{text}");
    assert!(
        text.bytes().enumerate().all(|(i, b)| if i == 2 || i == 5 {
            b == b':'
        } else {
            b.is_ascii_digit()
        }),
        "{text}"
    );
    assert!(clock
        .host
        .confinement_record(clock_key)
        .unwrap()
        .applied
        .unwrap()
        .protection
        .is_full());

    let pwd_key = "github:1431055733/pane-pwd";
    let mut pwd = official(
        "pane-pwd",
        &["status", "context.terminals", "context.cwd"],
        false,
        &packages,
    );
    assert_eq!(pwd.fragments(pwd_key, "pane_footer:1"), ["/first"]);

    // without the grant the load bars that need /proc cannot be read; the disk
    // bar uses statvfs, which no filesystem rule covers.
    let status_key = "github:1431055733/system-status";
    // a release whose manifest already requires the grant (the candidate
    // packages do) cannot be started without it, so that case is only
    // exercised for a release that does not declare it.
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(packages.join("system-status/among-plugin.json")).unwrap(),
    )
    .unwrap();
    let declares_metrics = ["required_features", "optional_features"]
        .iter()
        .filter_map(|key| manifest[key].as_array())
        .flatten()
        .any(|feature| feature == "system.metrics");
    if !declares_metrics {
        let mut status = official("system-status", &["status"], false, &packages);
        let bars = status.fragments(status_key, "app_header");
        assert_eq!(bars[0], "CPU N/A");
        assert_eq!(bars[1], "MEM N/A");
        assert!(
            bars[2].starts_with("DISK ") && bars[2].ends_with('%'),
            "{bars:?}"
        );
    } else {
        eprintln!(
            "the packaged system-status declares system.metrics; the no-grant case is skipped"
        );
    }

    // with a manifest that declares the grant, and the user's grant, the same
    // binary reads exactly the files the grant opens.
    let mut granted = official(
        "system-status",
        &["status", "system.metrics"],
        true,
        &packages,
    );
    let bars = granted.fragments(status_key, "app_header");
    assert!(
        bars[1].starts_with("MEM ") && bars[1].ends_with('%'),
        "{bars:?}"
    );
    std::thread::sleep(Duration::from_millis(1100));
    let bars = granted.fragments(status_key, "app_header");
    assert!(
        bars[0].starts_with("CPU ") && bars[0].ends_with('%'),
        "{bars:?}"
    );
}

#[test]
fn a_stopped_plugin_with_a_stored_reduced_consent_still_carries_the_mark() {
    // The current kernel offers full protection. A stored weaker consent must
    // remain visible without predicting the next launch's applied level.
    for (token, level, expected) in [
        (
            "reduced:1",
            Protection::Reduced { abi: 1 },
            "Reduced protection was allowed · reduced (Landlock ABI 1, below the required 3, and seccomp)",
        ),
        (
            "reduced:2",
            Protection::Reduced { abi: 2 },
            "Reduced protection was allowed · reduced (Landlock ABI 2, below the required 3, and seccomp)",
        ),
        (
            "seccomp-only",
            Protection::SeccompOnly,
            "Reduced protection was allowed · reduced (seccomp only, no Landlock)",
        ),
    ] {
        let confined = confined(Setup {
            probe: Some(Probe { landlock_abi: Some(3), seccomp: true }),
            local: json!({"enabled": false, "protection_consent": token}),
            ..Setup::default()
        });
        assert!(confined.host.plugins[0].io.is_none());
        let overview = confined.host.overview().remove(0);
        assert_eq!(overview.protection.as_deref(), Some(expected));
        assert!(overview.weaker_protection);
        assert!(!overview.enabled);
        assert_eq!(overview.consent_offer, None);
        assert_eq!(confined.host.consented_protection(KEY), Some(level));
        assert!(confined.host.confinement_record(KEY).is_none());
    }
    // no consent, no mark.
    let plain = self::confined(Setup {
        local: json!({"enabled": false}),
        ..Setup::default()
    });
    assert_eq!(plain.host.overview()[0].protection, None);
}

#[test]
fn stored_reduced_consent_does_not_predict_a_later_full_start() {
    if !kernel_offers_full_protection() {
        return;
    }
    const ALLOWED: &str = "Reduced protection was allowed · reduced (Landlock ABI 2, below the required 3, and seccomp)";
    let mut confined = confined(Setup {
        probe: Some(Probe {
            landlock_abi: Some(3),
            seccomp: true,
        }),
        local: json!({"enabled": false, "protection_consent": "reduced:2"}),
        ..Setup::default()
    });
    assert_eq!(
        confined.host.overview()[0].protection.as_deref(),
        Some(ALLOWED)
    );
    assert!(confined.host.overview()[0].weaker_protection);
    assert!(confined.host.plugins[0].io.is_none());

    confined.host.enable(KEY);
    confined.answers();
    let running = confined.host.overview().remove(0);
    assert_eq!(
        running.protection.as_deref(),
        Some("Protection: full (Landlock ABI 3 and seccomp)")
    );
    assert!(
        running.weaker_protection,
        "stored consent keeps its visible mark"
    );
    assert!(running.enabled);
    assert_eq!(running.consent_offer, None);
    assert_eq!(
        confined.host.consented_protection(KEY),
        Some(Protection::Reduced { abi: 2 })
    );
    assert_eq!(
        confined
            .host
            .confinement_record(KEY)
            .unwrap()
            .applied
            .as_ref()
            .unwrap()
            .protection,
        Protection::Full { abi: 3 }
    );

    confined.host.disable(KEY);
    confined.wait(|host| host.plugins[0].io.is_none());
    let stopped = confined.host.overview().remove(0);
    assert_eq!(stopped.protection.as_deref(), Some(ALLOWED));
    assert!(stopped.weaker_protection);
    assert!(!stopped.enabled);
    assert_eq!(stopped.protection_details, running.protection_details);
    assert_eq!(
        confined.host.consented_protection(KEY),
        Some(Protection::Reduced { abi: 2 })
    );
}

#[test]
fn a_stopped_plugin_does_not_claim_the_protection_its_last_process_reached() {
    if !kernel_offers_full_protection() {
        return;
    }
    let mut confined = confined(Setup::default());
    confined.answers();
    assert!(confined.host.overview()[0]
        .protection
        .as_deref()
        .is_some_and(|line| line.starts_with("Protection: full")));
    confined.host.disable(KEY);
    confined.wait(|host| host.plugins[0].io.is_none());
    assert_eq!(
        confined.host.overview()[0].protection,
        None,
        "a stopped plugin has no process whose level could be shown"
    );
}
