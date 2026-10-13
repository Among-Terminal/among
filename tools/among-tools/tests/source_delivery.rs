use among_tools::{source_delivery, storage, Result};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn fixture() -> Result<Fixture> {
    let path = storage::private_directory(&std::env::temp_dir(), "among-delivery")?;
    fs::write(
        path.join("fixture.snap"),
        b"archive integrity fixture, not a real Snap",
    )?;
    Ok(Fixture(path))
}
fn bundle(f: &Fixture, entries: &[(&str, &[u8])], inventory: serde_json::Value) -> Result<()> {
    let mut tar = tar::Builder::new(Vec::new());
    for (name, bytes) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, *name, *bytes)?;
    }
    let bytes = serde_json::to_vec(&inventory)?;
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, "delivery-inventory.json", bytes.as_slice())?;
    fs::write(f.0.join("source.tar"), tar.into_inner()?)?;
    associate(f)
}
fn associate(f: &Fixture) -> Result<()> {
    fs::write(
        f.0.join("association.json"),
        serde_json::to_vec(
            &json!({"schema":1,"original_source_sha256":storage::sha256(b"original".as_slice())?,"snap":{"filename":"fixture.snap","sha256":storage::sha256(fs::File::open(f.0.join("fixture.snap"))?)?},"source_bundle":{"filename":"source.tar","sha256":storage::sha256(fs::File::open(f.0.join("source.tar"))?)?}}),
        )?,
    )?;
    Ok(())
}
fn check(f: &Fixture) -> Result<serde_json::Value> {
    source_delivery::check(
        &f.0.join("association.json"),
        &f.0.join("source.tar"),
        &f.0.join("fixture.snap"),
    )
}

#[test]
fn actual_cli_checks_complete_inventory_and_changed_binary_source_and_association() -> Result<()> {
    let f = fixture()?;
    let digest = storage::sha256(b"original".as_slice())?;
    let inventory = json!({"schema":1,"files":{"original-source.tar":digest}});
    bundle(
        &f,
        &[("original-source.tar", b"original")],
        inventory.clone(),
    )?;
    assert_eq!(check(&f)?["files"], 2);
    let cli = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .arg("check-source-delivery")
        .arg(f.0.join("association.json"))
        .arg(f.0.join("source.tar"))
        .arg(f.0.join("fixture.snap"))
        .output()?;
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    assert_eq!(storage::parse_json(&cli.stdout)?, check(&f)?);
    fs::write(f.0.join("fixture.snap"), b"changed")?;
    assert!(check(&f).is_err());
    associate(&f)?;
    bundle(
        &f,
        &[("original-source.tar", b"changed")],
        inventory.clone(),
    )?;
    assert!(check(&f).is_err());
    bundle(
        &f,
        &[("original-source.tar", b"original")],
        json!({"schema":1,"files":{}}),
    )?;
    assert!(check(&f).is_err());
    bundle(&f, &[("original-source.tar", b"original")], inventory)?;
    let mut record = storage::parse_json(&fs::read(f.0.join("association.json"))?)?;
    record["original_source_sha256"] = "b".repeat(64).into();
    fs::write(f.0.join("association.json"), serde_json::to_vec(&record)?)?;
    assert!(check(&f).is_err());
    Ok(())
}

#[test]
fn duplicate_traversal_absolute_and_symlink_archives_are_refused() -> Result<()> {
    let f = fixture()?;
    let digest = storage::sha256(b"original".as_slice())?;
    bundle(
        &f,
        &[
            ("original-source.tar", b"original"),
            ("original-source.tar", b"original"),
        ],
        json!({"schema":1,"files":{"original-source.tar":digest}}),
    )?;
    assert_eq!(refusal(&f), "duplicate source archive member");
    for name in ["../outside", "/absolute"] {
        // Raw header avoids tar::Builder rejecting the unsafe fixture itself.
        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o644);
        let name_bytes = name.as_bytes();
        header.as_mut_bytes()[..name_bytes.len()].copy_from_slice(name_bytes);
        header.set_cksum();
        let mut archive = tar::Builder::new(Vec::new());
        archive.append(&header, b"x".as_slice())?;
        fs::write(f.0.join("source.tar"), archive.into_inner()?)?;
        associate(&f)?;
        assert_eq!(
            refusal(&f),
            "path must contain only relative normal components",
            "{name}"
        );
    }
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    header.set_mode(0o644);
    header.set_link_name("/outside")?;
    header.set_cksum();
    let mut archive = tar::Builder::new(Vec::new());
    archive.append_data(&mut header, "linked", b"".as_slice())?;
    fs::write(f.0.join("source.tar"), archive.into_inner()?)?;
    associate(&f)?;
    assert_eq!(refusal(&f), "source archive members must be regular files");
    fs::rename(f.0.join("source.tar"), f.0.join("saved.tar"))?;
    std::os::unix::fs::symlink(f.0.join("saved.tar"), f.0.join("source.tar"))?;
    assert_eq!(
        refusal(&f),
        "associated artifact must be a regular non-symlink file"
    );
    let files = BTreeMap::from([("ordinary".into(), b"x".to_vec())]);
    assert!(source_delivery::inventory(
        storage::deterministic_tar(&files)?.as_slice(),
        "delivery-inventory.json"
    )
    .is_err());
    Ok(())
}

/// Each refusal names its own cause, so a test can tell that the intended
/// check fired rather than an earlier, unrelated one.
fn refusal(f: &Fixture) -> String {
    check(f)
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default()
}

#[test]
fn association_schema_filenames_and_a_missing_original_are_refused_by_their_own_checks(
) -> Result<()> {
    let f = fixture()?;
    let digest = storage::sha256(b"original".as_slice())?;
    bundle(
        &f,
        &[("original-source.tar", b"original")],
        json!({"schema":1,"files":{"original-source.tar":digest}}),
    )?;
    check(&f)?;

    let original = storage::parse_json(&fs::read(f.0.join("association.json"))?)?;
    for (field, value, expected) in [
        ("schema", json!(2), "unsupported source association"),
        (
            "snap",
            json!({"filename":"other.snap","sha256":original["snap"]["sha256"]}),
            "wrong associated artifact filename",
        ),
        (
            "source_bundle",
            json!({"filename":"other.tar","sha256":original["source_bundle"]["sha256"]}),
            "wrong associated artifact filename",
        ),
    ] {
        let mut record = original.clone();
        record[field] = value;
        fs::write(f.0.join("association.json"), serde_json::to_vec(&record)?)?;
        assert_eq!(refusal(&f), expected, "{field}");
    }

    let other = storage::sha256(b"other".as_slice())?;
    bundle(
        &f,
        &[("other-member", b"other")],
        json!({"schema":1,"files":{"other-member":other}}),
    )?;
    assert_eq!(refusal(&f), "original source snapshot absent from delivery");

    bundle(
        &f,
        &[("original-source.tar", b"original")],
        json!({"schema":2,"files":{"original-source.tar":digest}}),
    )?;
    assert_eq!(
        refusal(&f),
        "source delivery inventory is incomplete or a delivered member changed"
    );
    Ok(())
}
