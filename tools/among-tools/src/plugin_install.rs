//! native acceptance delegates installation to the actual host UI, never a
//! standalone copier or a hand-written installed.json receipt.

use crate::{
    process::{allowed_environment, OwnedProcess},
    storage, Result,
};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub mod package;
pub mod release;

pub const NATIVE_TEST: &str = "app::plugin_tests::installation::real_published_official_packages_install_review_enable_disable_uninstall";

fn diagnostic_tail(output: &str) -> &str {
    // Native logs are not uploaded by every caller; keep the refusal visible
    // without flooding CI or slicing a UTF-8 code point.
    let mut start = output.len().saturating_sub(16 * 1024);
    while !output.is_char_boundary(start) {
        start += 1;
    }
    &output[start..]
}

/// Acceptance over the prepared candidate. Each runs here, in its own process
/// and isolated profile, never as a standalone ignored test: each needs the
/// prepared inputs, and the development cases also fix development mode.
pub const DEVELOPMENT_TESTS: [(&str, &str); 3] = [
    (
        "app::plugin_tests::installation::published_selection::published_native_selection_is_shown_installed_disabled_and_reviewed_without_implicit_access",
        "PASS native ",
    ),
    (
        "app::plugin_tests::installation::candidate::candidate_prerelease_replaces_an_install_and_a_stable_release_of_its_bytes_rebinds_it",
        "PASS development_prerelease replace_rebind=true",
    ),
    (
        "app::plugin_tests::installation::candidate::candidate_packages_install_from_a_local_catalogue_and_start_under_the_flag",
        "PASS development_local_catalogue install_start=true",
    ),
];

fn native_environment(base: &Path, inputs: &Path) -> Result<BTreeMap<OsString, OsString>> {
    for name in ["home", "config", "data", "cache", "state", "run"] {
        fs::DirBuilder::new().mode(0o700).create(base.join(name))?;
    }
    let mut environment = allowed_environment(
        &std::env::vars_os().collect(),
        &[
            "PATH",
            "LANG",
            "DISPLAY",
            "LIBGL_ALWAYS_SOFTWARE",
            "GSK_RENDERER",
        ],
    );
    for (key, directory) in [
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_RUNTIME_DIR", "run"),
    ] {
        environment.insert(key.into(), base.join(directory).into_os_string());
    }
    for (key, value) in [
        ("GDK_BACKEND", "x11"),
        ("GTK_A11Y", "test"),
        ("G_DEBUG", "fatal-criticals"),
        ("GIO_USE_VFS", "local"),
        ("ADW_DISABLE_PORTAL", "1"),
        ("SHELL", "/bin/sh"),
        ("HISTFILE", "/dev/null"),
    ] {
        environment.insert(key.into(), value.into());
    }
    environment.insert("AMONG_CHECK_ROOT".into(), base.as_os_str().to_owned());
    environment.insert(
        "AMONG_OFFICIAL_PLUGIN_INPUTS".into(),
        inputs.as_os_str().to_owned(),
    );
    Ok(environment)
}

// Runs one exact ignored host test on the owned display and returns its log.
fn native_test(
    executable: &Path,
    test: &str,
    environment: &BTreeMap<OsString, OsString>,
    base: &Path,
) -> Result<(std::process::ExitStatus, String)> {
    let list_path = base.join("test-names.txt");
    let list = File::create(&list_path)?;
    let mut listing = OwnedProcess::spawn(executable, &["--list"], environment, base, &list)?;
    if !listing.wait(Duration::from_secs(10))?.success()
        || fs::read_to_string(list_path)?
            .lines()
            .filter(|line| *line == format!("{test}: test"))
            .count()
            != 1
    {
        return Err(format!("current host does not provide the exact native test {test}").into());
    }
    let log_path = base.join("native-install.log");
    let log = File::create(&log_path)?;
    let mut child = OwnedProcess::spawn(
        Path::new("/usr/bin/dbus-run-session"),
        &[
            "--",
            executable.to_str().ok_or("host test path is not UTF-8")?,
            test,
            "--exact",
            "--ignored",
            "--test-threads=1",
            "--nocapture",
            "--color=never",
        ],
        environment,
        base,
        &log,
    )?;
    let status = child.wait(Duration::from_secs(240))?;
    Ok((status, fs::read_to_string(&log_path)?))
}

/// The required acceptance cases over the same candidate bytes.
pub fn development(executable: &Path, inputs: &Path, assets: &Path, scratch: &Path) -> Result<()> {
    development_cases(
        executable,
        inputs,
        assets,
        scratch,
        &DEVELOPMENT_TESTS,
        false,
    )
}

pub fn published_development(
    executable: &Path,
    inputs: &Path,
    assets: &Path,
    scratch: &Path,
) -> Result<()> {
    release::published::verify(inputs)?;
    development_cases(
        executable,
        inputs,
        assets,
        scratch,
        &[DEVELOPMENT_TESTS[0], DEVELOPMENT_TESTS[2]],
        true,
    )
}

pub fn synthetic_replacement(
    executable: &Path,
    inputs: &Path,
    assets: &Path,
    scratch: &Path,
) -> Result<()> {
    if release::verify(inputs)?["candidate"] != true {
        return Err("synthetic replacement requires explicitly synthetic candidate inputs".into());
    }
    development_cases(
        executable,
        inputs,
        assets,
        scratch,
        &[DEVELOPMENT_TESTS[1]],
        false,
    )
}

fn development_cases(
    executable: &Path,
    inputs: &Path,
    assets: &Path,
    scratch: &Path,
    tests: &[(&str, &str)],
    published: bool,
) -> Result<()> {
    crate::preferences::owned_display()?;
    let executable = executable.canonicalize()?;
    let inputs = inputs.canonicalize()?;
    let assets = assets.canonicalize()?;
    let prepared = if published {
        release::published::verify(&inputs)?
    } else {
        release::verify(&inputs)?
    };
    fs::create_dir_all(scratch)?;
    for &(test, marker) in tests {
        let base = storage::private_directory(scratch, "among-development-install-check")?;
        let mut environment = native_environment(&base, &inputs)?;
        if published {
            environment.insert("AMONG_PUBLISHED_PRERELEASE_ACCEPTANCE".into(), "1".into());
        }
        environment.insert("AMONG_CHECK_ROOT".into(), base.clone().into_os_string());
        environment.insert(
            "AMONG_LOCAL_PLUGIN_CATALOGUE".into(),
            assets.clone().into_os_string(),
        );
        let (status, output) = native_test(&executable, test, &environment, &base)?;
        if !status.success()
            || !output.contains("test result: ok. 1 passed; 0 failed; 0 ignored;")
            || !output.contains(marker)
            || output.contains("CRITICAL")
            || output.contains("panicked")
        {
            eprintln!(
                "Development installation refusal (last at most 16384 bytes of native-install.log):\n{}",
                diagnostic_tail(&output)
            );
            return Err(format!("{test} not accepted; evidence: {}", base.display()).into());
        }
        let final_inputs = if published {
            release::published::verify(&inputs)?
        } else {
            release::verify(&inputs)?
        };
        if final_inputs != prepared {
            return Err("prepared inputs changed during development acceptance".into());
        }
        println!(
            "{marker}; {}",
            if published {
                "actual published prerelease bytes"
            } else {
                "synthetic candidate acceptance"
            }
        );
    }
    Ok(())
}

pub fn run(executable: &Path, inputs: &Path, scratch: &Path) -> Result<PathBuf> {
    lifecycle(executable, inputs, scratch, false)
}

pub fn published_prerelease(executable: &Path, inputs: &Path, scratch: &Path) -> Result<PathBuf> {
    release::published::verify(inputs)?;
    lifecycle(executable, inputs, scratch, true)
}

fn lifecycle(executable: &Path, inputs: &Path, scratch: &Path, published: bool) -> Result<PathBuf> {
    crate::preferences::owned_display()?;
    let executable = executable.canonicalize()?;
    let inputs = inputs.canonicalize()?;
    let prepared = if published {
        release::published::verify(&inputs)?
    } else {
        release::verify(&inputs)?
    };
    // Ordinary installation remains stable-only. Published prerelease testing
    // needs both its explicit wrapper and the verified published receipt.
    if !published && prepared["release_snapshot"]["prerelease"] == true {
        return Err("published installation acceptance refuses prerelease inputs".into());
    }
    fs::create_dir_all(scratch)?;
    let base = storage::private_directory(scratch, "among-release-install-check")?;
    let mut environment = native_environment(&base, &inputs)?;
    if published {
        environment.insert("AMONG_PUBLISHED_PRERELEASE_ACCEPTANCE".into(), "1".into());
    }
    let (status, output) = native_test(&executable, NATIVE_TEST, &environment, &base)?;
    if !status.success()
        || !output.contains("test result: ok. 1 passed; 0 failed; 0 ignored;")
        || !output.contains("PASS published_release_ui install_review_enable_disable_uninstall=true terminal_input=true")
        || output.contains("CRITICAL")
        || output.contains("panicked")
    {
        eprintln!(
            "Native installation refusal (last at most 16384 bytes of native-install.log):\n{}",
            diagnostic_tail(&output)
        );
        return Err(format!(
            "published-package native installation not accepted; evidence: {}",
            base.display()
        )
        .into());
    }
    let final_inputs = if published {
        release::published::verify(&inputs)?
    } else {
        release::verify(&inputs)?
    };
    if final_inputs != prepared {
        return Err("prepared release observation changed during native installation".into());
    }
    let profile = base.join(".runtime-published-live-profile");
    let metadata = storage::parse_json(&fs::read(profile.join("profile.json"))?)?;
    if metadata["kind"] != "observed_published_installation_profile"
        || metadata["source_revision"] != prepared["source_revision"]
        || metadata["catalogue_revision"] != prepared["catalogue_revision"]
        || release::profile_inputs(&profile)? != prepared
    {
        return Err(
            "native test did not export its observed published installation profile".into(),
        );
    }
    if published {
        storage::atomic_write(
            &profile,
            Path::new("publisher-observation.json"),
            &serde_json::to_vec_pretty(&release::published::publication_record(&inputs)?)?,
            0o600,
        )?;
    }
    println!("Published profile: {}", profile.display());
    println!(
        "PASS native installation of {}; evidence: {}",
        if prepared["candidate"] == true {
            "CI-built candidate packages, not published bytes"
        } else {
            "actual published packages"
        },
        base.display()
    );
    Ok(base)
}

#[cfg(test)]
mod tests {
    use super::diagnostic_tail;

    #[test]
    fn refusal_diagnostics_keep_short_output_and_empty_logs() {
        assert_eq!(diagnostic_tail(""), "");
        assert_eq!(
            diagnostic_tail("a native test failed\n"),
            "a native test failed\n"
        );
    }

    #[test]
    fn refusal_diagnostics_bound_bytes_and_keep_the_failure_tail() {
        let output = format!("{}native assertion failed\n", "x".repeat(32 * 1024));
        let tail = diagnostic_tail(&output);
        assert_eq!(tail.len(), 16 * 1024);
        assert!(tail.ends_with("native assertion failed\n"));
    }

    #[test]
    fn refusal_diagnostics_do_not_split_multibyte_characters() {
        let output = format!("{}failed\n", "🦀".repeat(8192));
        let tail = diagnostic_tail(&output);
        assert!(tail.len() <= 16 * 1024);
        assert!(tail.len() > 16 * 1024 - 4);
        assert!(tail.starts_with('🦀'));
        assert!(tail.ends_with("failed\n"));
    }
}
