use among_tools::ignored_tests;

#[test]
fn cargo_records_reject_malformed_fields_and_deduplicate_artifacts() -> among_tools::Result<()> {
    let record =
        r#"{"reason":"compiler-artifact","profile":{"test":true},"executable":"/bin/true"}"#;
    assert_eq!(
        ignored_tests::executables(&format!("{record}\n{record}\n"))?.len(),
        1
    );
    for invalid in [
        "not JSON",
        "{}",
        r#"{"reason":"compiler-artifact","profile":{"test":true},"executable":42}"#,
        r#"{"reason":"compiler-artifact","profile":{"test":true},"executable":"relative"}"#,
    ] {
        assert!(ignored_tests::executables(invalid).is_err());
    }
    Ok(())
}

#[test]
fn phase_based_preferences_are_not_misrun_and_invalid_names_fail() -> among_tools::Result<()> {
    assert_eq!(ignored_tests::names("first: test\napp::tests::preferences_persist_across_fresh_processes: test\nsecond: test\n2 tests, 0 benchmarks")?, ["first", "second"]);
    assert!(ignored_tests::names("first: test\nfirst: test").is_err());
    assert!(ignored_tests::names(
        "actual_dpkg_source_extraction_records_build_inputs_and_refuses_owner_output: test"
    )?
    .is_empty());
    // Collecting APT metadata rewrites the system APT configuration as root;
    // on a host runner that would change the runner itself.
    assert!(ignored_tests::names(
        "actual_collection_preserves_a_local_signed_repository_in_a_root_container: test"
    )?
    .is_empty());
    assert!(ignored_tests::names("-bad: test").is_err());
    Ok(())
}

#[test]
fn only_the_exact_parent_owned_worker_is_excluded() -> among_tools::Result<()> {
    let worker =
        "plugin::install::source::tests::installation_authority::cooperating_reinstall_worker";
    let suffix = format!("{worker}_suffix");
    let prefix = format!("other::{worker}");
    let sibling =
        "plugin::install::source::tests::installation_authority::ordinary_graphical_regression";
    assert_eq!(
        ignored_tests::names(&format!(
            "z_last: test\n{suffix}: test\n{worker}: test\n{sibling}: test\n{prefix}: test\na_first: test\n6 tests, 0 benchmarks"
        ))?,
        ["a_first", &prefix, &suffix, sibling, "z_last"]
    );
    assert!(ignored_tests::names(": test").is_err());
    assert!(ignored_tests::names(&format!("{suffix}: test\n{suffix}: test")).is_err());
    assert!(ignored_tests::names(&format!("{worker}: test\n-bad: test")).is_err());
    Ok(())
}

#[test]
fn only_the_exact_dedicated_enospc_case_is_routed_out_of_graphics() -> among_tools::Result<()> {
    let name = among_tools::installer_enospc::TEST;
    let suffix = format!("{name}_suffix");
    let prefix = format!("other::{name}");
    assert_eq!(
        ignored_tests::names(&format!("{name}: test\n{suffix}: test\n{prefix}: test"))?,
        [prefix, suffix]
    );
    Ok(())
}

#[test]
fn only_the_exact_published_install_case_uses_its_dedicated_runner() -> among_tools::Result<()> {
    let name = among_tools::plugin_install::NATIVE_TEST;
    let suffix = format!("{name}_suffix");
    let prefix = format!("other::{name}");
    assert_eq!(
        ignored_tests::names(&format!("{name}: test\n{suffix}: test\n{prefix}: test"))?,
        [prefix, suffix]
    );
    Ok(())
}

struct SelectionFixture(std::path::PathBuf);

impl Drop for SelectionFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl SelectionFixture {
    fn new(failing: bool) -> among_tools::Result<Self> {
        let fixture = Self(among_tools::storage::private_directory(
            &std::env::temp_dir(),
            "among-ignored-selection-test",
        )?);
        let executable = fixture.0.join("tests");
        let mut compile = std::process::Command::new("rustc");
        compile
            .args(["--edition=2021", "--test"])
            .arg(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/ignored-selection.rs"),
            )
            .arg("-o")
            .arg(&executable);
        if failing {
            compile.args(["--cfg", "ordinary_failure"]);
        }
        let output = compile.output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::write(
            fixture.0.join("cargo.jsonl"),
            serde_json::to_vec(&serde_json::json!({
                "reason": "compiler-artifact",
                "profile": {"test": true},
                "executable": executable,
            }))?,
        )?;
        Ok(fixture)
    }

    fn run(&self) -> among_tools::Result<std::process::Output> {
        Ok(
            std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
                .env("AMONG_CHECK_ROOT", self.0.join("checks"))
                .arg("run-ignored-tests")
                .arg(self.0.join("cargo.jsonl"))
                .output()?,
        )
    }
}

#[test]
#[ignore = "requires the isolated display test container; exercised through the Rust ignored-test runner"]
fn actual_cli_excludes_only_the_worker_and_counts_real_standalone_tests() -> among_tools::Result<()>
{
    let fixture = SelectionFixture::new(false)?;
    let output = fixture.run()?;
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("Executed 2 ignored tests, one owned display per test"),
        "{stdout}"
    );
    assert!(stderr.contains("is excluded from standalone graphics; this is not an acceptance pass; its owning parent regression supplies coverage"), "{stderr}");
    assert!(fixture.0.join("ordinary-ran").is_file());
    assert!(fixture.0.join("near-name-ran").is_file());
    assert!(!fixture.0.join("worker-ran").exists());
    Ok(())
}

#[test]
#[ignore = "requires the isolated display test container; exercised through the Rust ignored-test runner"]
fn actual_cli_still_fails_and_stops_on_an_ordinary_test_failure() -> among_tools::Result<()> {
    let fixture = SelectionFixture::new(true)?;
    let output = fixture.run()?;
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(!output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("ordinary probe failed deliberately"),
        "{stdout}"
    );
    assert!(
        stderr.contains("ignored graphical test failed: ordinary_standalone_probe"),
        "{stderr}"
    );
    assert!(!stdout.contains("Executed "), "{stdout}");
    assert!(fixture.0.join("ordinary-ran").is_file());
    assert!(!fixture.0.join("near-name-ran").exists());
    assert!(!fixture.0.join("worker-ran").exists());
    Ok(())
}

#[test]
fn actual_cli_refuses_a_failed_test_listing() -> among_tools::Result<()> {
    let path =
        std::env::temp_dir().join(format!("among-cargo-listing-{}.jsonl", std::process::id()));
    std::fs::write(
        &path,
        r#"{"reason":"compiler-artifact","profile":{"test":true},"executable":"/usr/bin/false"}"#,
    )?;
    let checks = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("among-ignored-refusal-{}", std::process::id()));
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env("AMONG_CHECK_ROOT", &checks)
        .arg("run-ignored-tests")
        .arg(&path)
        .output()?;
    std::fs::remove_file(path)?;
    std::fs::remove_dir_all(&checks)?;
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)?.contains("test executable listing failed"));
    Ok(())
}

#[test]
#[ignore = "requires the isolated display test container; exercised through the Rust ignored-test runner"]
fn actual_x_query_on_an_owned_display() -> among_tools::Result<()> {
    let output = std::process::Command::new("/usr/bin/xprop")
        .args(["-root", "_NET_SUPPORTING_WM_CHECK"])
        .output()?;
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)?.contains("window id"));
    assert!(std::env::var("HOME")?.contains("among-rust-display-"));
    Ok(())
}

#[test]
#[ignore = "requires the isolated display test container; exercised through the Rust ignored-test runner"]
fn second_independent_owned_display() -> among_tools::Result<()> {
    let display = std::env::var("DISPLAY")?;
    assert!(display.starts_with(':'));
    assert!(!std::env::var("DBUS_SESSION_BUS_ADDRESS")?.is_empty());
    let output = std::process::Command::new("/usr/bin/xdotool")
        .args(["getdisplaygeometry"])
        .output()?;
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout)?.trim(), "1920 1200");
    Ok(())
}
