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
