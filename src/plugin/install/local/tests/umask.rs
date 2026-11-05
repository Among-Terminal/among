//! The managed store and the settings documents under the umasks real desktop
//! sessions use. Directories here are created by the process umask and never
//! chmod-ed afterwards, because that is how a user's `.config/among` and
//! managed store exist on a machine with private user groups (`umask 002`).
//!
//! The umask is process-wide, so the lifecycle body runs in a child test
//! process of its own, once per umask; the parent test only starts the
//! children and checks their exit status.

use super::*;
use crate::settings::{Document, PreferencesStorage};

const CHILD: &str =
    "plugin::install::local::tests::umask::lifecycle_under_the_umask_in_the_environment";
const CHILD_UMASK: &str = "AMONG_UMASK_TEST_MASK";

struct Home {
    config: PathBuf,
    managed: PathBuf,
}

impl Home {
    /// Creates the user's directories the way the process umask makes them.
    fn create(root: &OwnedRoot, name: &str) -> Self {
        let home = root.0.join(name);
        let config = home.join(".config/among");
        let managed = home.join(".local/share/among/managed");
        for directory in [&config, &managed] {
            std::fs::DirBuilder::new()
                .recursive(true)
                .create(directory)
                .unwrap();
        }
        Self { config, managed }
    }
}

fn run_wait(job: crate::plugin::install::store::RecoveryJob) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(outcome) = job.poll() {
            outcome.unwrap();
            return;
        }
        assert!(Instant::now() < deadline, "store recovery did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A group the user belongs to besides the primary one, so a directory can be
/// handed to it without privileges.
fn foreign_group() -> Option<libc::gid_t> {
    let primary = unsafe { libc::getegid() };
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    let mut groups = vec![0 as libc::gid_t; usize::try_from(count).unwrap()];
    let count = unsafe { libc::getgroups(count, groups.as_mut_ptr()) };
    groups.truncate(usize::try_from(count).unwrap());
    groups.into_iter().find(|group| *group != primary)
}

/// Install, record the review and the enabled choice through the real settings
/// writer, uninstall, run the store recovery the application starts after a
/// removal, and install the same version again.
#[test]
#[ignore = "run by settings_and_the_plugin_lifecycle_work_under_every_common_umask in a child process"]
fn lifecycle_under_the_umask_in_the_environment() {
    let mask = u32::from_str_radix(&std::env::var(CHILD_UMASK).unwrap(), 8).unwrap();
    unsafe { libc::umask(mask as libc::mode_t) };
    let root = OwnedRoot::new();
    let author = Author::new(&root);
    let home = Home::create(&root, "home");
    let expected = 0o777 & !mask;
    for directory in [&home.config, &home.managed] {
        assert_eq!(
            std::fs::metadata(directory).unwrap().permissions().mode() & 0o777,
            expected,
            "the directory must be what a real desktop session with umask {mask:o} creates"
        );
    }
    let mut settings = PreferencesStorage::new(Some(home.config.clone()));
    let install = || {
        finish(spawn(author.selection(), home.managed.clone()).unwrap())
            .unwrap_or_else(|problem| panic!("umask {mask:o}: install: {problem}"))
            .receipt
    };
    let record = |settings: &mut PreferencesStorage, receipt: &Receipt| {
        let key = receipt.identity().unwrap().key();
        settings
            .managed_review_for_installation(
                &key,
                receipt.review(vec!["status".into()]),
                receipt.installation.as_deref(),
            )
            .unwrap_or_else(|error| panic!("umask {mask:o}: review not saved: {error}"));
        settings
            .managed_enable_changed(&key, true, receipt.installation.as_deref())
            .unwrap_or_else(|error| panic!("umask {mask:o}: enable not saved: {error}"));
        let saved = settings.load(Document::ManagedPlugins).unwrap();
        let local = inventory::local(&saved, &key).unwrap();
        assert_eq!(local["enabled"], json!(true));
        assert!(receipt.matches_review(&local["review"]));
    };

    let first = install();
    record(&mut settings, &first);
    store::mutate(
        &home.managed,
        &first,
        crate::plugin::install::mutation::Action::Uninstall,
        None,
        &|| Ok(()),
        || Ok(()),
    )
    .unwrap();
    assert!(inventory::read(&home.managed).unwrap().is_empty());
    settings
        .plugin_changed(&first.identity().unwrap().key(), false)
        .unwrap();
    run_wait(store::RecoveryJob::start(home.managed.clone()).unwrap());
    let second = install();
    assert_eq!(second.plugin_id, first.plugin_id);
    assert_eq!(second.package_sha256, first.package_sha256);
    assert_eq!(inventory::read(&home.managed).unwrap().len(), 1);
    record(&mut settings, &second);
}

#[test]
fn settings_and_the_plugin_lifecycle_work_under_every_common_umask() {
    for mask in ["002", "022", "077"] {
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                CHILD,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_UMASK, mask)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "umask {mask}: {} {}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
    }
}

#[test]
fn a_world_writable_or_foreign_group_store_and_config_directory_is_refused() {
    let _serial = JOB_TESTS.lock().unwrap();
    let root = OwnedRoot::new();
    let home = Home::create(&root, "refused");
    let path = home.config.join("managed-plugins.json");
    let save = || {
        crate::settings::update(&path, Document::ManagedPlugins, |value| {
            value["plugins"] = json!({});
            Ok(())
        })
    };
    for directory in [&home.config, &home.managed] {
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o757)).unwrap();
    }
    assert!(save().is_err());
    assert!(store::open_existing_root(&home.managed).is_err());
    assert!(!path.exists());
    let Some(foreign) = foreign_group() else {
        eprintln!("NOT VERIFIED: the user has no group besides the primary one");
        return;
    };
    for directory in [&home.config, &home.managed] {
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o775)).unwrap();
        let name = std::ffi::CString::new(directory.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::chown(name.as_ptr(), u32::MAX, foreign) }, 0);
    }
    assert!(save().is_err());
    assert!(store::open_existing_root(&home.managed).is_err());
    assert!(!path.exists());
}
