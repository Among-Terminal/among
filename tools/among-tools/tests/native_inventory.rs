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

const LIBRARY: &[u8] = b"synthetic shared library bytes as packaged";

/// Build a real binary package with `dpkg-deb` from a synthetic tree: a
/// library, its copyright notice and the control fields an Ubuntu package
/// carries, including a source package with its own version.
fn build_deb(root: &std::path::Path, package: &str, output: &std::path::Path) -> Result<()> {
    let tree = root.join(format!("tree-{package}"));
    fs::create_dir_all(tree.join("DEBIAN"))?;
    fs::create_dir_all(tree.join("usr/lib"))?;
    fs::create_dir_all(tree.join(format!("usr/share/doc/{package}")))?;
    fs::write(
        tree.join("DEBIAN/control"),
        format!(
            "Package: {package}\nVersion: 1.2.3-4\nArchitecture: amd64\n\
             Source: synthetic-source (1.2.3-2)\nMaintainer: Fixture <fixture@example.invalid>\n\
             Description: synthetic native input\n"
        ),
    )?;
    fs::write(tree.join("usr/lib/libsynthetic.so.1"), LIBRARY)?;
    fs::write(
        tree.join(format!("usr/share/doc/{package}/copyright")),
        NOTICE,
    )?;
    let result = std::process::Command::new("/usr/bin/dpkg-deb")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .args(["--root-owner-group", "--build"])
        .arg(&tree)
        .arg(output)
        .output()?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr).to_string().into());
    }
    Ok(())
}

/// A prime this project's own payload marks as the shipped artifact.
fn own_prime(prime: &std::path::Path) -> Result<()> {
    fs::create_dir_all(prime.join("bin"))?;
    fs::create_dir_all(prime.join("usr/share/doc/among"))?;
    fs::create_dir_all(prime.join("usr/lib"))?;
    fs::write(prime.join("bin/among"), b"synthetic executable")?;
    fs::write(
        prime.join("usr/share/doc/among/common-licenses-provider.txt"),
        b"base-files 13ubuntu10\n",
    )?;
    Ok(())
}

fn run_inventory(root: &std::path::Path) -> Result<std::process::Output> {
    Ok(
        std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
            .env_clear()
            .env("AMONG_CHECK_ROOT", root.join("checks"))
            .arg("inventory-snap-native")
            .arg(root.join("prime"))
            .arg(root.join("debs"))
            .arg(root.join("audit"))
            .output()?,
    )
}

/// The inventory of a real package records its control identity, the
/// package file's digest, which packaged files reached prime and whether
/// staging changed them, and the verbatim notice with the common licenses
/// it names. A library whose staged bytes differ from the packaged ones, as
/// after Snapcraft rewrites an ELF interpreter, is reported as modified with
/// both digests.
#[test]
fn actual_cli_inventories_a_real_debian_package_against_prime() -> Result<()> {
    let f = fixture()?;
    let prime = f.0.join("prime");
    own_prime(&prime)?;
    fs::write(
        prime.join("usr/lib/libsynthetic.so.1"),
        b"library after staging",
    )?;
    fs::write(
        prime.join("usr/share/doc/synthetic-library/copyright"),
        NOTICE,
    )?;
    fs::create_dir(f.0.join("debs"))?;
    let deb = f.0.join("debs/synthetic-library_1.2.3-4_amd64.deb");
    build_deb(&f.0, "synthetic-library", &deb)?;

    let result = run_inventory(&f.0)?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let notice = "notices/synthetic-library/usr/share/doc/synthetic-library/copyright";
    let document = storage::parse_json(&fs::read(f.0.join("audit/inventory.json"))?)?;
    assert_eq!(document["schema"], 1);
    assert_eq!(
        document["packages"],
        json!([{
            "package": "synthetic-library",
            "version": "1.2.3-4",
            "architecture": "amd64",
            "source_package": "synthetic-source",
            "source_version": "1.2.3-2",
            "input": {
                "filename": "synthetic-library_1.2.3-4_amd64.deb",
                "sha256": storage::sha256(fs::File::open(&deb)?)?,
            },
            "staged_files": [
                {
                    "path": "usr/lib/libsynthetic.so.1",
                    "sha256": storage::sha256(b"library after staging".as_slice())?,
                    "original_sha256": storage::sha256(LIBRARY)?,
                    "modified": true,
                },
                {
                    "path": "usr/share/doc/synthetic-library/copyright",
                    "sha256": storage::sha256(NOTICE)?,
                    "original_sha256": storage::sha256(NOTICE)?,
                    "modified": false,
                },
            ],
            "notices": [{"path": notice, "sha256": storage::sha256(NOTICE)?, "staged": true}],
            "common_license_references": ["LGPL-3"],
        }])
    );
    assert_eq!(fs::read(f.0.join("audit").join(notice))?, NOTICE);
    Ok(())
}

/// Two package files for the same binary package make it ambiguous which one
/// was staged, and a package with no file in prime was not staged at all;
/// both are refused without an inventory.
#[test]
fn actual_cli_refuses_duplicate_and_unstaged_real_packages() -> Result<()> {
    let f = fixture()?;
    let prime = f.0.join("prime");
    own_prime(&prime)?;
    fs::write(prime.join("usr/lib/libsynthetic.so.1"), LIBRARY)?;
    fs::create_dir(f.0.join("debs"))?;
    build_deb(&f.0, "synthetic-library", &f.0.join("debs/first.deb"))?;
    build_deb(&f.0, "synthetic-library", &f.0.join("debs/second.deb"))?;
    let result = run_inventory(&f.0)?;
    assert!(!result.status.success());
    assert_eq!(
        String::from_utf8(result.stderr)?.trim_end(),
        "among-tools: duplicate binary package inputs"
    );
    assert!(!f.0.join("audit/inventory.json").exists());

    fs::remove_dir_all(f.0.join("audit"))?;
    fs::remove_file(f.0.join("debs/second.deb"))?;
    fs::remove_file(prime.join("usr/lib/libsynthetic.so.1"))?;
    build_deb(&f.0, "unstaged-library", &f.0.join("debs/unstaged.deb"))?;
    fs::remove_file(f.0.join("debs/first.deb"))?;
    let result = run_inventory(&f.0)?;
    assert!(!result.status.success());
    assert_eq!(
        String::from_utf8(result.stderr)?.trim_end(),
        "among-tools: Debian input has no observable regular files in prime"
    );
    assert!(!f.0.join("audit/inventory.json").exists());
    Ok(())
}
