use among_tools::platform;

#[test]
fn explicit_approval_and_both_hosted_markers_are_required() {
    assert!(platform::hosted_rehearsal(
        true,
        Some("true"),
        Some("github-hosted")
    ));
    for (actions, runner) in [
        (None, None),
        (Some("true"), None),
        (None, Some("github-hosted")),
        (Some("true"), Some("self-hosted")),
        (Some("false"), Some("github-hosted")),
    ] {
        assert!(!platform::hosted_rehearsal(true, actions, runner));
    }
    assert!(!platform::hosted_rehearsal(
        false,
        Some("true"),
        Some("github-hosted")
    ));
}

#[test]
fn both_native_architectures_and_distribution_targets_are_validated() -> among_tools::Result<()> {
    for (os, version) in [("debian", "13"), ("ubuntu", "24.04")] {
        for (machine, arch) in [("x86_64", "amd64"), ("aarch64", "arm64")] {
            let platform =
                platform::parse(&format!("ID={os}\nVERSION_ID=\"{version}\"\n"), machine)?;
            platform::validate_target(&platform, os, version, arch)?;
            let wrong = if arch == "amd64" { "arm64" } else { "amd64" };
            assert!(platform::validate_target(&platform, os, version, wrong).is_err());
            assert!(platform::validate_target(&platform, "ubuntu", "22.04", arch).is_err());
        }
    }
    assert!(platform::parse("ID=debian\nVERSION_ID=13", "riscv64").is_err());
    assert!(platform::parse("ID=debian\nID=ubuntu\nVERSION_ID=13", "amd64").is_err());
    assert!(platform::parse("broken", "amd64").is_err());
    assert!(platform::parse("ID=ubuntu", "amd64").is_err());
    Ok(())
}

#[test]
fn actual_cli_reports_discovery_and_refuses_wrong_target() -> among_tools::Result<()> {
    let binary = env!("CARGO_BIN_EXE_among-tools");
    let output = std::process::Command::new(binary)
        .args(["platform", "discover"])
        .output()?;
    assert!(output.status.success());
    let actual: platform::Platform = serde_json::from_slice(&output.stdout)?;
    assert!(matches!(actual.architecture.as_str(), "amd64" | "arm64"));
    let rejected = std::process::Command::new(binary)
        .args(["platform", "check", "ubuntu", "0", "amd64"])
        .output()?;
    assert!(!rejected.status.success());
    Ok(())
}
