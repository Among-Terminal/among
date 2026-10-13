use among_tools::{deb822, native_sources, storage, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
impl Fixture {
    fn new() -> Result<Self> {
        let f = Self(storage::private_directory(
            &std::env::temp_dir(),
            "among-sources",
        )?);
        fs::create_dir_all(f.0.join("fixture"))?;
        fs::write(
            f.0.join("fixture/fixture.tar.xz"),
            b"Synthetic source archive member",
        )?;
        let digest = storage::sha256(b"Synthetic source archive member".as_slice())?;
        let size = fs::metadata(f.0.join("fixture/fixture.tar.xz"))?.len();
        fs::write(f.0.join("fixture/fixture.dsc"),format!("Source: fixture\nVersion: 1.0-2\nChecksums-Sha256:\n {digest} {size} fixture.tar.xz\n"))?;
        f.save()?;
        Ok(f)
    }
    fn save(&self) -> Result<()> {
        let mut files = Vec::new();
        for name in ["fixture/fixture.dsc", "fixture/fixture.tar.xz"] {
            let path = self.0.join(name);
            files.push(json!({"path":name,"bytes":path.metadata()?.len(),"sha256":storage::sha256(fs::File::open(path)?)?}));
        }
        fs::write(
            self.0.join("inventory.json"),
            serde_json::to_vec(
                &json!({"schema":1,"sources":[{"source_package":"fixture","source_version":"1.0-2","downloaded":true,"files":files}]}),
            )?,
        )?;
        Ok(())
    }
    fn check(&self) -> Result<Value> {
        native_sources::check(&native(), &self.0)
    }
}
fn native() -> Value {
    json!({"schema":1,"packages":[{"source_package":"fixture","source_version":"1.0-2"}]})
}

#[test]
fn actual_cli_accepts_exact_files_and_parses_signed_cleartext_without_claiming_trust() -> Result<()>
{
    let f = Fixture::new()?;
    assert_eq!(f.check()?["archive_members"], 1);
    let dsc = f.0.join("fixture/fixture.dsc");
    let text = fs::read_to_string(&dsc)?;
    fs::write(dsc,format!("-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA256\n\n{text}\n-----BEGIN PGP SIGNATURE-----\nfixture armor, not trusted\n"))?;
    f.save()?;
    let input = f.0.join("native.json");
    fs::write(&input, serde_json::to_vec(&native())?)?;
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .arg("check-snap-sources")
        .arg(input)
        .arg(&f.0)
        .output()?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let record = storage::parse_json(&result.stdout)?;
    assert_eq!(record, f.check()?);
    let relative = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .current_dir(&f.0)
        .args(["check-snap-sources", "native.json", "."])
        .output()?;
    assert!(
        relative.status.success(),
        "{}",
        String::from_utf8_lossy(&relative.stderr)
    );
    assert_eq!(storage::parse_json(&relative.stdout)?, record);
    assert!(record["signature_trust"]
        .as_str()
        .ok_or("trust scope absent")?
        .starts_with("not checked"));
    Ok(())
}

#[test]
fn changed_archive_remains_rejected_after_self_consistent_inventory_update() -> Result<()> {
    let f = Fixture::new()?;
    fs::write(f.0.join("fixture/fixture.tar.xz"), b"changed")?;
    assert!(f.check().is_err());
    f.save()?;
    assert!(f
        .check()
        .err()
        .ok_or("changed descriptor accepted")?
        .to_string()
        .contains("descriptor digest/size"));
    Ok(())
}

#[test]
fn wrong_versions_symlinks_duplicate_identities_and_missing_coverage_are_rejected() -> Result<()> {
    let f = Fixture::new()?;
    let dsc = f.0.join("fixture/fixture.dsc");
    let original = fs::read_to_string(&dsc)?;
    fs::write(&dsc, original.replace("1.0-2", "1.0-3"))?;
    f.save()?;
    assert!(f.check().is_err());
    fs::write(&dsc, original)?;
    f.save()?;
    let inventory = storage::parse_json(&fs::read(f.0.join("inventory.json"))?)?;
    let mut changed = inventory.clone();
    changed["sources"]
        .as_array_mut()
        .ok_or("fixture sources absent")?
        .push(inventory["sources"][0].clone());
    fs::write(f.0.join("inventory.json"), serde_json::to_vec(&changed)?)?;
    assert!(f.check().is_err());
    changed["sources"] = json!([]);
    fs::write(f.0.join("inventory.json"), serde_json::to_vec(&changed)?)?;
    assert!(f.check().is_err());
    f.save()?;
    fs::remove_file(f.0.join("fixture/fixture.tar.xz"))?;
    std::os::unix::fs::symlink(&dsc, f.0.join("fixture/fixture.tar.xz"))?;
    // The symlink itself must be the reason, not the digest mismatch that a
    // link to other bytes would also cause.
    assert_eq!(
        f.check().err().map(|error| error.to_string()).as_deref(),
        Some("owned path contains a symlink")
    );
    Ok(())
}

#[test]
fn debian_parser_rejects_ambiguous_fields_and_malformed_checksum_records() -> Result<()> {
    for text in [
        "Source: one\nsource: two\n",
        " continued without header\n",
        "Source fixture\n",
    ] {
        assert!(deb822::fields(text).is_err());
    }
    let fields = deb822::fields(
        "Source: fixture\r\nVersion: 1.0\r\nChecksums-Sha256:\r\n abc 12 archive\r\n",
    )?;
    assert_eq!(deb822::required(&fields, "source")?, "fixture");
    assert!(deb822::required(&fields, "absent").is_err());
    for line in [
        "short 12 file",
        &format!("{} nope file", "a".repeat(64)),
        &format!("{} 12 file extra", "a".repeat(64)),
    ] {
        assert!(deb822::checksum(line).is_err());
    }
    Ok(())
}
