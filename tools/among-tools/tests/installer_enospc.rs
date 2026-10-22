use among_tools::Result;
use std::path::Path;

#[test]
fn checks_require_the_dedicated_consumer_on_both_architectures() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let workflow: serde_yaml_ng::Value = serde_yaml_ng::from_str(&std::fs::read_to_string(
        root.join(".github/workflows/checks.yml"),
    )?)?;
    let native = &workflow["jobs"]["native"];
    assert_eq!(
        native["strategy"]["matrix"]["runner"]
            .as_sequence()
            .ok_or("native runner matrix missing")?
            .len(),
        2
    );
    let steps = native["steps"]
        .as_sequence()
        .ok_or("native steps missing")?;
    let index = steps
        .iter()
        .position(|step| {
            step["name"].as_str()
                == Some("Original installer ENOSPC assertions on a dedicated bounded tmpfs")
        })
        .ok_or("mandatory dedicated ENOSPC step missing")?;
    let step = &steps[index];
    assert!(step["if"].is_null());
    assert!(step["continue-on-error"].is_null());
    let script = step["run"].as_str().ok_or("dedicated command missing")?;
    for required in [
        "cargo test --locked --no-run --message-format=json",
        "check-installer-enospc-from-cargo",
        "--tmpfs /owned-full:rw,nosuid,nodev,noexec,mode=1777,size=128m",
        "--network none --read-only --cap-drop ALL --security-opt no-new-privileges",
        "--user \"$(id -u):$(id -g)\"",
        "target=/tmp",
        "target=/run",
        "target=/dev/shm",
    ] {
        assert!(script.contains(required), "missing {required}");
    }
    assert!(!script.contains("|| true"));
    assert!(!script.contains("--privileged"));
    assert!(!script.contains("CAP_SYS_ADMIN"));
    assert!(steps[index + 1]["name"]
        .as_str()
        .ok_or("following graphical step missing")?
        .contains("Ignored Rust graphical tests"));
    Ok(())
}

#[test]
fn the_cli_refuses_a_missing_owned_mount_without_running_a_cargo_executable() -> Result<()> {
    if Path::new("/owned-full").exists() {
        return Err("this negative control must run outside the dedicated ENOSPC mount".into());
    }
    let scratch = among_tools::storage::private_directory(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        "among-enospc-refusal",
    )?;
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env("AMONG_CHECK_ROOT", &scratch)
        .arg("check-installer-enospc-from-cargo")
        .arg(scratch.join("nonexistent-cargo-records.jsonl"))
        .output()?;
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        stderr.contains("disposable Docker container")
            || stderr.contains("dedicated /owned-full mount"),
        "{stderr}"
    );
    assert!(!String::from_utf8(output.stdout)?.contains("Executed "));
    assert_eq!(std::fs::read_dir(&scratch)?.count(), 0);
    Ok(())
}
