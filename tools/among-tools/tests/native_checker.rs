use among_tools::{native_checker as checker, storage, Result};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

struct Fixture(PathBuf);

#[test]
fn rc_checker_receipt_cannot_satisfy_stable_or_another_candidate() -> Result<()> {
    let f = fixture()?;
    let output = f.0.join("checker");
    let source = "a".repeat(40);
    let architecture = arch()?;
    checker::package(
        std::path::Path::new(env!("CARGO_BIN_EXE_among-tools")),
        &source,
        "v0.1.0-rc.1",
        "debian13",
        architecture,
        &output,
    )?;
    checker::validate(&output, &source, "v0.1.0-rc.1", "debian13", architecture)?;
    for tag in ["v0.1.0", "v0.1.0-rc.2"] {
        assert!(checker::validate(&output, &source, tag, "debian13", architecture).is_err());
    }
    Ok(())
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn fixture() -> Result<Fixture> {
    let base = storage::private_directory(&std::env::temp_dir(), "among-checker")?;
    Ok(Fixture(base))
}
fn arch() -> Result<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("amd64"),
        "aarch64" => Ok("arm64"),
        _ => Err("checker tests require supported native architecture".into()),
    }
}

#[test]
fn real_checker_cli_rejects_mutated_source_target_digest_and_inventory() -> Result<()> {
    let f = fixture()?;
    let directory = f.0.join("checker");
    let source = "a".repeat(40);
    let architecture = arch()?;
    let binary = std::path::Path::new(env!("CARGO_BIN_EXE_among-tools"));
    let packed = std::process::Command::new(binary)
        .env_clear()
        .arg("package-native-checker")
        .arg(binary)
        .args([&source, "v0.1.0", "debian13", architecture])
        .arg(&directory)
        .output()?;
    assert!(
        packed.status.success(),
        "{}",
        String::from_utf8_lossy(&packed.stderr)
    );
    let record: checker::Record = serde_json::from_slice(&packed.stdout)?;
    let retry = std::process::Command::new(binary)
        .env_clear()
        .arg("package-native-checker")
        .arg(binary)
        .args([&source, "v0.1.0", "debian13", architecture])
        .arg(&directory)
        .output()?;
    assert!(!retry.status.success());
    let executable = checker::validate(&directory, &source, "v0.1.0", "debian13", architecture)?;
    let result = std::process::Command::new(&executable)
        .env_clear()
        .args([
            "check-native-checker",
            directory.to_str().ok_or("fixture path is not UTF-8")?,
            &source,
            "v0.1.0",
            "debian13",
            architecture,
        ])
        .output()?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(checker::validate(
        &directory,
        &"b".repeat(40),
        "v0.1.0",
        "debian13",
        architecture
    )
    .is_err());
    assert!(checker::validate(&directory, &source, "v0.1.1", "debian13", architecture).is_err());
    assert!(checker::validate(&directory, &source, "v0.1.0", "ubuntu24.04", architecture).is_err());
    assert!(checker::validate(
        &directory,
        &source,
        "v0.1.0",
        "debian13",
        if architecture == "amd64" {
            "arm64"
        } else {
            "amd64"
        }
    )
    .is_err());
    let original = fs::read(&executable)?;
    fs::write(&executable, b"altered")?;
    assert!(checker::validate(&directory, &source, "v0.1.0", "debian13", architecture).is_err());
    fs::write(&executable, &original)?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o644))?;
    assert!(checker::validate(&directory, &source, "v0.1.0", "debian13", architecture).is_err());
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))?;
    fs::write(directory.join("private"), b"unexpected")?;
    assert!(checker::validate(&directory, &source, "v0.1.0", "debian13", architecture).is_err());
    fs::remove_file(directory.join("private"))?;
    let mut changed = serde_json::to_value(&record)?;
    changed["filename"] = "../foreign".into();
    fs::write(
        directory.join("checker-record.json"),
        serde_json::to_vec(&changed)?,
    )?;
    assert!(checker::validate(&directory, &source, "v0.1.0", "debian13", architecture).is_err());
    assert_eq!(storage::sha256(original.as_slice())?, record.sha256);
    Ok(())
}

#[test]
fn symlink_and_foreign_elf_are_refused_without_output() -> Result<()> {
    let f = fixture()?;
    let source = "a".repeat(40);
    let output = f.0.join("checker");
    let architecture = arch()?;
    let linked = f.0.join("linked");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_among-tools"), &linked)?;
    assert!(checker::package(
        &linked,
        &source,
        "v0.1.0",
        "debian13",
        architecture,
        &output
    )
    .is_err());
    assert!(!output.exists());
    assert!(checker::package(
        std::path::Path::new(env!("CARGO_BIN_EXE_among-tools")),
        &source,
        "v0.1.0",
        "debian13",
        if architecture == "amd64" {
            "arm64"
        } else {
            "amd64"
        },
        &output
    )
    .is_err());
    assert!(!output.exists());
    Ok(())
}

#[test]
fn user_payload_rejects_internal_checkers_even_in_nested_paths() -> Result<()> {
    let f = fixture()?;
    let payload = f.0.join("payload");
    fs::create_dir_all(payload.join("usr/bin"))?;
    fs::write(payload.join("usr/bin/among"), b"fixture")?;
    among_tools::deb_build::reject_checker_payload(&payload)?;
    for name in ["among-tools", "among-uninstall", "checker-record.json"] {
        let path = payload.join("usr/bin").join(name);
        fs::write(&path, b"must not ship")?;
        assert!(among_tools::deb_build::reject_checker_payload(&payload).is_err());
        fs::remove_file(path)?;
    }
    Ok(())
}
