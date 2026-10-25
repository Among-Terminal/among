use crate::process::{allowed_environment, OwnedProcess};
use crate::{storage, Result};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn executables(records: &str) -> Result<BTreeSet<PathBuf>> {
    let mut executables = BTreeSet::new();
    for line in records.lines().filter(|line| !line.trim().is_empty()) {
        let item = storage::parse_json(line.as_bytes())?;
        let reason = item["reason"]
            .as_str()
            .ok_or("Cargo record lacks a reason")?;
        if reason != "compiler-artifact" {
            continue;
        }
        if !item["profile"]["test"]
            .as_bool()
            .ok_or("Cargo artifact lacks a boolean test profile")?
        {
            continue;
        }
        if item["executable"].is_null() {
            continue;
        }
        let executable = item["executable"]
            .as_str()
            .ok_or("Cargo test executable is not a string")?;
        let path = PathBuf::from(executable);
        if !path.is_absolute() {
            return Err("Cargo test executable must be absolute".into());
        }
        executables.insert(path);
    }
    if executables.is_empty() {
        return Err("Cargo records contain no test executables".into());
    }
    Ok(executables)
}

pub fn names(listing: &str) -> Result<Vec<String>> {
    let mut names = BTreeSet::new();
    for line in listing.lines() {
        if let Some(name) = line.strip_suffix(": test") {
            if name.is_empty() || name.starts_with('-') {
                return Err("invalid Rust test name".into());
            }
            if name == "app::tests::preferences_persist_across_fresh_processes" {
                eprintln!("Preferences phases require their dedicated orchestration, not a standalone ignored invocation");
                continue;
            }
            if name == "actual_dpkg_source_extraction_records_build_inputs_and_refuses_owner_output"
            {
                eprintln!("Third-party extraction requires its dedicated disposable audit container, not a host graphical test; this skip is not an acceptance pass");
                continue;
            }
            if name == "actual_collection_preserves_a_local_signed_repository_in_a_root_container" {
                eprintln!("APT collection rewrites APT configuration as root and runs only in its dedicated offline audit container; this skip is not an acceptance pass");
                continue;
            }
            if name == "plugin::install::source::tests::installation_authority::cooperating_reinstall_worker" {
                eprintln!("{name} is excluded from standalone graphics; this is not an acceptance pass; its owning parent regression supplies coverage");
                continue;
            }
            if name == "plugin::install::registry::tests::official_source::effective_sources_fresh_process_worker" {
                eprintln!("{name} is excluded from standalone graphics; this is not an acceptance pass; a_fresh_process_reconstructs_official_authority_after_manual_omission supplies its root and coverage");
                continue;
            }
            if name == crate::installer_enospc::TEST {
                eprintln!("{name} requires check-installer-enospc-from-cargo in its mandatory bounded tmpfs CI container; exclusion from graphics is not an acceptance pass");
                continue;
            }
            if !names.insert(name.to_string()) {
                return Err("duplicate Rust test name".into());
            }
        }
    }
    Ok(names.into_iter().collect())
}

pub fn run(records: &Path, scratch: &Path) -> Result<usize> {
    let executables = executables(&fs::read_to_string(records)?)?;
    fs::create_dir_all(scratch)?;
    let base = scratch.join(format!(
        "among-ignored-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    fs::DirBuilder::new().mode(0o700).create(&base)?;
    let mut env = allowed_environment(&std::env::vars_os().collect(), &["PATH", "LANG"]);
    env.insert("HOME".into(), base.clone().into_os_string());
    for key in [
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
    ] {
        env.insert(key.into(), base.join(key).into_os_string());
    }
    let mut count = 0;
    for (index, executable) in executables.into_iter().enumerate() {
        if fs::symlink_metadata(&executable)?.file_type().is_symlink() || !executable.is_file() {
            return Err("Cargo test executable must be a regular file".into());
        }
        let log_path = base.join(format!("listing-{index}.log"));
        let log = File::create(&log_path)?;
        let mut listing =
            OwnedProcess::spawn(&executable, &["--list", "--ignored"], &env, &base, &log)?;
        if !listing.wait(Duration::from_secs(30))?.success() {
            return Err("test executable listing failed".into());
        }
        for name in names(&fs::read_to_string(&log_path)?)? {
            let mut args = Vec::new();
            if name == "prefs::repository_sources::tests::official_row_survives_offline_refresh_custom_removal_reopen_and_corrupt_storage" {
                // the display defaults deliberately disable accessibility. only
                // this exact case needs gtk's test backend and owned captures.
                let temporary = storage::private_directory(&base, "official-source-widget")?;
                let captures = temporary.join("captures");
                fs::DirBuilder::new().mode(0o700).create(&captures)?;
                args.extend([
                    "/usr/bin/env".into(),
                    "GTK_A11Y=test".into(),
                    format!("TMPDIR={}", temporary.to_str().ok_or("test temporary path is not UTF-8")?),
                    format!("AMONG_OFFICIAL_SOURCE_CAPTURES={}", captures.to_str().ok_or("test capture path is not UTF-8")?),
                ]);
            }
            args.extend([
                executable
                    .to_str()
                    .ok_or("test executable path is not UTF-8")?
                    .to_string(),
                name.clone(),
                "--exact".into(),
                "--ignored".into(),
                "--test-threads=1".into(),
                "--nocapture".into(),
            ]);
            let status = crate::display::run(
                crate::display::Mode::Ci,
                &args,
                scratch,
                Duration::from_secs(90),
            )?;
            if !status.success() {
                return Err(format!("ignored graphical test failed: {name}").into());
            }
            count += 1;
        }
    }
    Ok(count)
}
