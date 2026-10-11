use among_tools::{native_inventory as inventory, native_notices, storage, Result};
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
    let f = Fixture(storage::private_directory(
        &std::env::temp_dir(),
        "among-native-inventory",
    )?);
    fs::create_dir_all(f.0.join("prime/usr/share/doc/synthetic-library"))?;
    fs::create_dir(f.0.join("output"))?;
    Ok(f)
}
const NOTICE: &[u8] =
    b"Synthetic fixture\nLicense: LGPL-3+\nSee /usr/share/common-licenses/LGPL-3.\n";

#[test]
fn exact_source_versions_payload_changes_and_verbatim_notices_are_observed_without_package_construction(
) -> Result<()> {
    let f = fixture()?;
    let prime = f.0.join("prime");
    let output = f.0.join("output");
    let name = "usr/share/doc/synthetic-library/copyright";
    fs::write(prime.join(name), NOTICE)?;
    let archive = storage::deterministic_tar(&BTreeMap::from([(name.into(), NOTICE.to_vec())]))?;
    let (files, notices, references) =
        inventory::payload(archive.as_slice(), &prime, &output, "synthetic-library")?;
    assert_eq!(
        inventory::source_identity(
            "synthetic-library",
            "1.2.3-4",
            Some("synthetic-source (1.2.3-2)")
        )?,
        ("synthetic-source".into(), "1.2.3-2".into())
    );
    assert_eq!(references.into_iter().collect::<Vec<_>>(), vec!["LGPL-3"]);
    assert_eq!(files[0]["sha256"], storage::sha256(NOTICE)?);
    assert_eq!(files[0]["modified"], false);
    assert_eq!(
        fs::read(output.join(notices[0]["path"].as_str().ok_or("notice path absent")?))?,
        NOTICE
    );
    let document = json!({"schema":1,"packages":[{"package":"synthetic-library","notices":notices,"common_license_references":["LGPL-3"]}]});
    assert!(native_notices::check(&document, &prime).is_err());
    fs::create_dir_all(prime.join("usr/share/common-licenses"))?;
    let license = prime.join("usr/share/common-licenses/LGPL-3");
    fs::write(
        &license,
        b"Synthetic nonempty fixture license, not for distribution",
    )?;
    assert_eq!(native_notices::check(&document, &prime)?["notices"], 1);
    fs::remove_file(&license)?;
    fs::write(f.0.join("foreign"), b"outside payload")?;
    std::os::unix::fs::symlink(f.0.join("foreign"), &license)?;
    assert!(native_notices::check(&document, &prime).is_err());
    fs::write(prime.join(name), b"changed copyright")?;
    let (changed, _, _) =
        inventory::payload(archive.as_slice(), &prime, &output, "synthetic-library")?;
    assert_eq!(changed[0]["modified"], true);
    assert!(native_notices::check(&document, &prime).is_err());
    Ok(())
}

#[test]
fn absent_prime_files_missing_notices_and_unsafe_source_names_fail() -> Result<()> {
    let f = fixture()?;
    let archive = storage::deterministic_tar(&BTreeMap::from([(
        "usr/bin/fixture".into(),
        b"fixture".to_vec(),
    )]))?;
    assert!(inventory::payload(
        archive.as_slice(),
        &f.0.join("prime"),
        &f.0.join("output"),
        "synthetic-library"
    )
    .is_err());
    fs::create_dir_all(f.0.join("prime/usr/bin"))?;
    fs::write(f.0.join("prime/usr/bin/fixture"), b"fixture")?;
    assert!(inventory::payload(
        archive.as_slice(),
        &f.0.join("prime"),
        &f.0.join("output"),
        "synthetic-library"
    )
    .is_err());
    for source in ["../foreign", "Synthetic", "name ()", "name (1.0))"] {
        assert!(inventory::source_identity("synthetic-library", "1.0", Some(source)).is_err());
    }
    assert!(inventory::inventory(
        &f.0.join("prime"),
        &f.0.join("missing"),
        &f.0.join("new"),
        &f.0
    )
    .is_err());
    assert!(!f.0.join("new").exists());
    Ok(())
}

#[test]
fn conventional_dpkg_root_and_directory_entries_are_not_unsafe_file_members() -> Result<()> {
    let f = fixture()?;
    let prime = f.0.join("prime");
    fs::write(
        prime.join("usr/share/doc/synthetic-library/copyright"),
        NOTICE,
    )?;
    let mut archive = tar::Builder::new(Vec::new());
    for name in ["./", "./usr/", "./usr/share/"] {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_mode(0o755);
        header.set_cksum();
        archive.append_data(&mut header, name, b"".as_slice())?;
    }
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(NOTICE.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    archive.append_data(
        &mut header,
        "./usr/share/doc/synthetic-library/copyright",
        NOTICE,
    )?;
    let archive = archive.into_inner()?;
    let (files, notices, _) = inventory::payload(
        archive.as_slice(),
        &prime,
        &f.0.join("output"),
        "synthetic-library",
    )?;
    assert_eq!(files.len(), 1);
    assert_eq!(notices.len(), 1);
    Ok(())
}
