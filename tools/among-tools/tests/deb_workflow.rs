use among_tools::{deb_build, deb_workflow, Result};
use serde_json::{json, Value};

fn fixture() -> Value {
    let mut rows = Vec::new();
    for target in ["debian13", "ubuntu24.04"] {
        for arch in ["amd64", "arm64"] {
            rows.push(json!({"target":target,"architecture":arch,"runner":if arch=="amd64" {"ubuntu-24.04"} else {"ubuntu-24.04-arm"},"rust_host":if arch=="amd64" {"x86_64-unknown-linux-gnu"} else {"aarch64-unknown-linux-gnu"},"image":format!("{}@sha256:{}",if target=="debian13" {"debian"} else {"ubuntu"},"a".repeat(64))}));
        }
    }
    json!({"on":{"push":{"tags":["v*"]}},"permissions":{"contents":"read"},"concurrency":{"cancel-in-progress":false},"jobs":{"build":{"container":{"image":"${{ matrix.image }}"},"strategy":{"matrix":{"include":rows}},"steps":[{"uses":format!("actions/checkout@{}","a".repeat(40))},{"run":"rustup-init --default-toolchain 1.98.1\nuseradd --create-home --shell /bin/bash among-build\nchown among-build:among-build /tmp/opencode\namong-tools ci-deb-build . \"$BUILD_TARGET\" \"$BUILD_ARCH\" \"$BUILD_IMAGE\" output"}]},"installed":{"needs":"build","container":{"image":"${{ matrix.image }}"},"strategy":{"matrix":{"include":rows}},"steps":[{"uses":format!("actions/checkout@{}","b".repeat(40))},{"run":"sha256sum --check native-checker.sha256\nnative-checker/among-tools check-native-checker\nnative-checker/among-tools ci-deb-install . \"$BUILD_TARGET\""}]}}})
}

#[test]
fn release_only_exact_native_matrices_permissions_actions_and_installed_isolation() -> Result<()> {
    let valid = fixture();
    assert_eq!(deb_workflow::validate(&valid)?.len(), 2);
    for mutation in 0..10 {
        let mut changed = valid.clone();
        match mutation {
            0 => changed["on"]["pull_request"] = json!({}),
            1 => changed["permissions"]["contents"] = "write".into(),
            2 => changed["concurrency"]["cancel-in-progress"] = true.into(),
            3 => {
                changed["jobs"]["build"]["strategy"]["matrix"]["include"][1]["runner"] =
                    "ubuntu-24.04".into()
            }
            4 => {
                changed["jobs"]["build"]["strategy"]["matrix"]["include"][1]["rust_host"] =
                    "x86_64-unknown-linux-gnu".into()
            }
            5 => {
                changed["jobs"]["installed"]["strategy"]["matrix"]["include"][0]["image"] =
                    "debian@sha256:bad".into()
            }
            6 => changed["jobs"]["build"]["steps"][0]["uses"] = "actions/checkout@main".into(),
            7 => changed["jobs"]["installed"]["needs"] = "nothing".into(),
            8 => {
                changed["jobs"]["installed"]["steps"][1]["run"] =
                    "rustup-init; python3 scripts/ci-deb-install.py".into()
            }
            9 => {
                changed["jobs"]["build"]["steps"][1]["run"] = format!(
                    "{}\ngh release create",
                    changed["jobs"]["build"]["steps"][1]["run"]
                        .as_str()
                        .ok_or("missing fixture command")?
                )
                .into()
            }
            _ => return Err("unknown mutation".into()),
        }
        assert!(
            deb_workflow::validate(&changed).is_err(),
            "mutation {mutation}"
        );
    }
    let mut duplicate = valid.clone();
    duplicate["jobs"]["build"]["strategy"]["matrix"]["include"][1] =
        duplicate["jobs"]["build"]["strategy"]["matrix"]["include"][0].clone();
    assert!(deb_workflow::validate(&duplicate).is_err());
    Ok(())
}

#[test]
fn yaml_parser_retains_on_and_rejects_duplicate_policy_keys() -> Result<()> {
    assert_eq!(
        deb_workflow::read("on:\n  push:\n    tags: ['v*']\n")?["on"],
        json!({"push":{"tags":["v*"]}})
    );
    assert!(
        deb_workflow::read("permissions: {contents: read}\npermissions: {contents: write}\n")
            .is_err()
    );
    Ok(())
}

#[test]
fn actual_builder_refuses_local_operations_before_any_output() -> Result<()> {
    let base =
        among_tools::storage::private_directory(&std::env::temp_dir(), "among-must-not-build")?;
    let output = base.join("must-not-exist");
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .args([
            "ci-deb-build",
            "/absent-source",
            "debian13",
            "amd64",
            &format!("debian@sha256:{}", "a".repeat(64)),
            output.to_str().ok_or("output path is not UTF-8")?,
        ])
        .output()?;
    assert!(!result.status.success());
    assert!(String::from_utf8(result.stderr)?.contains("restricted to GitHub release-tag push CI"));
    assert!(!output.exists());
    std::fs::remove_dir(base)?;
    deb_build::image_identity(&format!("debian@sha256:{}", "a".repeat(64)), "debian13")?;
    assert!(
        deb_build::image_identity(&format!("ubuntu@sha256:{}", "a".repeat(64)), "debian13")
            .is_err()
    );
    assert!(deb_build::image_identity("debian:latest", "debian13").is_err());
    Ok(())
}

#[test]
fn actual_installed_runner_refuses_local_operations_before_creating_evidence() -> Result<()> {
    let output =
        std::env::temp_dir().join(format!("among-must-not-install-{}", std::process::id()));
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .args([
            "ci-deb-install",
            "/absent-source",
            "debian13",
            "amd64",
            "/absent-candidate",
            output.to_str().ok_or("output path is not UTF-8")?,
        ])
        .output()?;
    assert!(!result.status.success());
    assert!(String::from_utf8(result.stderr)?.contains("restricted to GitHub release-tag push CI"));
    assert!(!output.exists());
    let desktop = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .arg("check-deb-desktop")
        .output()?;
    assert!(!desktop.status.success());
    assert!(String::from_utf8(desktop.stderr)?.contains("only in release CI"));
    Ok(())
}
