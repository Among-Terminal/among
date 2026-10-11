use among_tools::{native_unpack, storage, Result};
use serde_json::json;
use std::fs;
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
#[ignore = "requires a disposable source-audit container; exercised by the dedicated CI container step"]
fn actual_dpkg_source_extraction_records_build_inputs_and_refuses_owner_output() -> Result<()> {
    let f = Fixture(storage::private_directory(
        &std::env::temp_dir(),
        "among-unpack",
    )?);
    fs::create_dir_all(f.0.join("fixture-1.0/debian/source"))?;
    fs::write(
        f.0.join("fixture-1.0/debian/source/format"),
        b"3.0 (native)\n",
    )?;
    fs::write(f.0.join("fixture-1.0/debian/control"),b"Source: fixture\nSection: misc\nPriority: optional\nMaintainer: Synthetic Fixture <fixture@example.invalid>\nStandards-Version: 4.7.0\n\nPackage: fixture\nArchitecture: all\nDescription: Synthetic extraction fixture, never a release asset\n")?;
    fs::write(
        f.0.join("fixture-1.0/debian/rules"),
        b"#!/usr/bin/make -f\n%:\n\ttrue\n",
    )?;
    fs::write(f.0.join("fixture-1.0/debian/changelog"),b"fixture (1.0) unstable; urgency=low\n\n  * Synthetic source extraction fixture.\n\n -- Synthetic Fixture <fixture@example.invalid>  Fri, 09 Oct 2026 00:00:00 +0000\n")?;
    // This produces only a synthetic Debian source descriptor/archive; no DEB
    // construction, installation or native dependency rebuild occurs locally.
    let built = std::process::Command::new("/usr/bin/dpkg-source")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C.UTF-8")
        .current_dir(&f.0)
        .args(["-b", "fixture-1.0"])
        .output()?;
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let files = fs::read_dir(&f.0)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|name| name.to_string_lossy().starts_with("fixture_"))
        .map(|name| json!({"path":name.to_string_lossy()}))
        .collect::<Vec<_>>();
    let inventory =
        json!({"sources":[{"source_package":"fixture","source_version":"1.0","files":files}]});
    fs::write(f.0.join("inventory.json"), serde_json::to_vec(&inventory)?)?;
    let output = f.0.join("unpacked");
    let cli = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .arg("check-native-source-unpack")
        .arg(&f.0)
        .arg(&output)
        .output()?;
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let evidence = storage::parse_json(&fs::read(output.join("unpack-evidence.json"))?)?;
    assert_eq!(evidence["sources"][0]["source_package"], "fixture");
    assert_eq!(
        evidence["sources"][0]["build_files"]["debian/control"],
        storage::sha256(fs::File::open(output.join("fixture/debian/control"))?)?
    );
    assert!(native_unpack::run(&f.0, &output).is_err());
    let mut unsafe_inventory = inventory;
    unsafe_inventory["sources"][0]["source_package"] = "../foreign".into();
    fs::write(
        f.0.join("inventory.json"),
        serde_json::to_vec(&unsafe_inventory)?,
    )?;
    assert!(native_unpack::run(&f.0, &f.0.join("must-not-exist")).is_err());
    assert!(!f.0.join("must-not-exist").exists());
    Ok(())
}
