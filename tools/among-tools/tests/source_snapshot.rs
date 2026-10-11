use among_tools::{source_snapshot as snapshot, storage, Result};
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Result<Self> {
        let base = storage::private_directory(&std::env::temp_dir(), "among-source-fixture")?;
        let root = base.join("public");
        fs::create_dir(&root)?;
        for name in snapshot::FILES {
            let path = root.join(name);
            fs::create_dir_all(path.parent().ok_or("fixture has no parent")?)?;
            fs::write(path, format!("fixture public input: {name}"))?;
        }
        fs::create_dir_all(base.join("vendor/fixture"))?;
        fs::write(base.join("vendor/fixture/.cargo-checksum.json"), b"{}")?;
        Ok(Self(base))
    }
    fn root(&self) -> PathBuf {
        self.0.join("public")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn deterministic_archive_excludes_private_and_compiled_state_preserves_modes_and_inventory(
) -> Result<()> {
    let f = Fixture::new()?;
    let root = f.root();
    fs::write(root.join(".env"), b"synthetic secret not public")?;
    fs::create_dir_all(root.join("video/readme-promo"))?;
    fs::write(
        root.join("video/readme-promo/package.json"),
        b"private capture project",
    )?;
    fs::create_dir(root.join(".opencode"))?;
    fs::write(root.join(".opencode/private"), b"synthetic evidence")?;
    fs::create_dir_all(root.join("tools/among-tools/target"))?;
    fs::write(
        root.join("tools/among-tools/target/private"),
        b"compiled cache",
    )?;
    fs::create_dir_all(root.join("scripts/__pycache__"))?;
    fs::write(root.join("scripts/__pycache__/cache"), b"inert cache")?;
    fs::write(root.join("scripts/launcher.sh"), b"#!/bin/sh\nexit 0\n")?;
    fs::set_permissions(
        root.join("scripts/launcher.sh"),
        fs::Permissions::from_mode(0o755),
    )?;
    let one = f.0.join("one.tar");
    let two = f.0.join("two.tar");
    let receipts = [
        snapshot::snapshot(&root, &f.0.join("vendor"), &one)?,
        snapshot::snapshot(&root, &f.0.join("vendor"), &two)?,
    ];
    assert_eq!(receipts[0].archive_sha256, receipts[1].archive_sha256);
    assert_eq!(fs::read(&one)?, fs::read(&two)?);
    let bytes = fs::read(&one)?;
    let mut hashes = storage::inspect_tar(&bytes)?;
    for forbidden in [
        ".env",
        ".opencode/private",
        "tools/among-tools/target/private",
        "scripts/__pycache__/cache",
        "video/readme-promo/package.json",
    ] {
        assert!(!hashes.contains_key(forbidden));
    }
    for required in [
        "tools/among-tools/Cargo.toml",
        "tools/among-tools/Cargo.lock",
        ".cargo/config.toml",
    ] {
        assert!(hashes.contains_key(required));
    }
    assert_eq!(hashes.len(), receipts[0].files + 2);
    let mut archive = tar::Archive::new(bytes.as_slice());
    let mut inventory = None;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let name = entry.path()?.into_owned();
        assert_eq!(
            entry.header().mode()?,
            if name == std::path::Path::new("scripts/launcher.sh") {
                0o755
            } else {
                0o644
            }
        );
        assert_eq!(entry.header().uid()?, 0);
        assert_eq!(entry.header().gid()?, 0);
        assert_eq!(entry.header().mtime()?, 0);
        if name == std::path::Path::new("source-inventory.json") {
            let mut body = Vec::new();
            entry.read_to_end(&mut body)?;
            inventory = Some(storage::parse_json(&body)?);
        }
    }
    hashes.remove("source-inventory.json");
    assert_eq!(
        inventory.ok_or("source inventory absent")?["files"],
        serde_json::to_value(hashes)?
    );
    assert!(snapshot::snapshot(&root, &f.0.join("vendor"), &one).is_err());
    assert_eq!(
        storage::sha256(fs::File::open(&one)?)?,
        receipts[0].archive_sha256
    );
    Ok(())
}

#[test]
fn unsafe_missing_source_vendor_and_output_inputs_never_produce_a_new_archive() -> Result<()> {
    for failure in [
        "source-file",
        "directory-root",
        "nested-source",
        "vendor-root",
        "vendor-file",
        "missing",
        "no-checksum",
    ] {
        let f = Fixture::new()?;
        let root = f.root();
        let vendor = f.0.join("vendor");
        let output = f.0.join("output.tar");
        match failure {
            "source-file" => {
                fs::remove_file(root.join("LICENSE"))?;
                std::os::unix::fs::symlink("/bin/true", root.join("LICENSE"))?;
            }
            "directory-root" => std::os::unix::fs::symlink("/absent-fixture", root.join("src"))?,
            "nested-source" => {
                fs::create_dir(root.join("src"))?;
                std::os::unix::fs::symlink("/", root.join("src/foreign"))?;
            }
            "vendor-root" => {
                fs::rename(&vendor, f.0.join("saved-vendor"))?;
                std::os::unix::fs::symlink(f.0.join("saved-vendor"), &vendor)?;
            }
            "vendor-file" => {
                std::os::unix::fs::symlink("/bin/true", vendor.join("fixture/linked"))?
            }
            "missing" => fs::remove_file(root.join("LICENSE"))?,
            "no-checksum" => fs::remove_file(vendor.join("fixture/.cargo-checksum.json"))?,
            _ => return Err("unknown negative source fixture".into()),
        }
        assert!(
            snapshot::snapshot(&root, &vendor, &output).is_err(),
            "{failure}"
        );
        assert!(!output.exists());
    }
    let f = Fixture::new()?;
    let output = f.0.join("output.tar");
    let marker = f.0.join("user-file");
    fs::write(&marker, b"keep")?;
    std::os::unix::fs::symlink(&marker, &output)?;
    assert!(snapshot::snapshot(&f.root(), &f.0.join("vendor"), &output).is_err());
    assert_eq!(fs::read(marker)?, b"keep");
    Ok(())
}

#[test]
fn actual_snapshot_cli_records_real_archive_digest_without_overwriting_output() -> Result<()> {
    let f = Fixture::new()?;
    let archive = f.0.join("source.tar");
    let run = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
            .env_clear()
            .arg("source-snapshot")
            .arg(f.root())
            .arg(f.0.join("vendor"))
            .arg(&archive)
            .output()
    };
    let output = run()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt = storage::parse_json(&output.stdout)?;
    assert_eq!(
        receipt["archive_sha256"],
        storage::sha256(fs::File::open(&archive)?)?
    );
    let before = fs::read(&archive)?;
    let checked = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .arg("check-source-snapshot")
        .arg(&archive)
        .output()?;
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert!(!run()?.status.success());
    assert_eq!(fs::read(archive)?, before);
    Ok(())
}
