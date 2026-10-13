use among_tools::{apt_provenance as apt, storage, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

struct Fixture {
    root: PathBuf,
    native: Value,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
impl Fixture {
    fn new() -> Result<Self> {
        let root = storage::private_directory(&std::env::temp_dir(), "among-apt-fixture")?;
        fs::create_dir_all(root.join("sources/fixture"))?;
        fs::create_dir(root.join("apt"))?;
        fs::write(
            root.join("apt/archive-keyring.gpg"),
            b"synthetic keyring; positive crypto verification is mocked only in library tests",
        )?;
        let archive = root.join("sources/fixture/fixture.tar.xz");
        fs::write(&archive, b"fixture source")?;
        let dsc = root.join("sources/fixture/fixture.dsc");
        fs::write(
            &dsc,
            format!(
                "Source: fixture\nVersion: 1.0\nChecksums-Sha256:\n {} {} fixture.tar.xz\n",
                storage::sha256(fs::File::open(&archive)?)?,
                archive.metadata()?.len()
            ),
        )?;
        let mut files = Vec::new();
        for path in [&dsc, &archive] {
            files.push(json!({"path":path.strip_prefix(root.join("sources"))?.to_str().ok_or("fixture path not UTF-8")?,"sha256":storage::sha256(fs::File::open(path)?)?,"bytes":path.metadata()?.len()}));
        }
        fs::write(
            root.join("sources/inventory.json"),
            serde_json::to_vec(
                &json!({"schema":1,"sources":[{"source_package":"fixture","source_version":"1.0","downloaded":true,"files":files}]}),
            )?,
        )?;
        let native = json!({"schema":1,"packages":[{"package":"fixture","version":"1.0","source_package":"fixture","source_version":"1.0","architecture":"amd64","input":{"filename":"fixture.deb","sha256":"a".repeat(64)}}]});
        fs::write(root.join("apt/Packages"),format!("Package: fixture\nVersion: 1.0\nArchitecture: amd64\nFilename: pool/fixture.deb\nSHA256: {}\n","a".repeat(64)))?;
        let mut sources = String::from("Package: fixture\nVersion: 1.0\nChecksums-Sha256:\n");
        for file in files {
            sources.push_str(&format!(
                " {} {} {}\n",
                file["sha256"].as_str().ok_or("fixture digest absent")?,
                file["bytes"],
                PathBuf::from(file["path"].as_str().ok_or("fixture name absent")?)
                    .file_name()
                    .ok_or("fixture basename absent")?
                    .to_string_lossy()
            ));
        }
        fs::write(root.join("apt/Sources"), sources)?;
        let mut release=String::from("Origin: Ubuntu\nCodename: noble\nDate: Thu, 08 Oct 2026 23:00:00 +0000\nValid-Until: Sat, 10 Oct 2026 00:00:00 +0000\nSHA256:\n");
        for name in ["Sources", "Packages"] {
            let path = root.join("apt").join(name);
            release.push_str(&format!(
                " {} {} main/{name}\n",
                storage::sha256(fs::File::open(&path)?)?,
                path.metadata()?.len()
            ));
        }
        fs::write(root.join("apt/InRelease"), release)?;
        fs::write(
            root.join("apt/inventory.json"),
            serde_json::to_vec(
                &json!({"schema":1,"collected_at":"2026-10-09T00:00:00+00:00","indices":[{"file":"Sources","release":"InRelease","release_member":"main/Sources","role":"source"},{"file":"Packages","release":"InRelease","release_member":"main/Packages","role":"binary"}]}),
            )?,
        )?;
        Ok(Self { root, native })
    }
    fn check(&self) -> Result<Value> {
        apt::authenticate_with(
            &self.root.join("apt"),
            &self.native,
            &self.root.join("sources"),
            2_000_000_000,
            |_, _| Ok(true),
        )
    }
}

#[test]
fn exact_chain_checks_all_indices_inputs_sources_and_caches_each_release_verification() -> Result<()>
{
    let f = Fixture::new()?;
    let mut verified = 0;
    let result = apt::authenticate_with(
        &f.root.join("apt"),
        &f.native,
        &f.root.join("sources"),
        2_000_000_000,
        |_, _| {
            verified += 1;
            Ok(true)
        },
    )?;
    assert_eq!(verified, 1);
    assert_eq!(result["binary_inputs"], 1);
    assert_eq!(result["source_versions"], 1);
    assert_eq!(result["indices"], 2);
    assert_eq!(result["signer"], apt::SIGNER);
    Ok(())
}

#[test]
fn wrong_signer_invalid_crypto_modified_index_expiry_and_unauthenticated_inputs_fail() -> Result<()>
{
    let f = Fixture::new()?;
    assert!(apt::authenticate_with(
        &f.root.join("apt"),
        &f.native,
        &f.root.join("sources"),
        2_000_000_000,
        |_, _| Ok(false)
    )
    .is_err());
    assert!(!apt::signature_status(
        true,
        "[GNUPG:] VALIDSIG WRONG fixture"
    ));
    assert!(!apt::signature_status(
        false,
        &format!("[GNUPG:] VALIDSIG {} fixture", apt::SIGNER)
    ));
    assert!(apt::signature_status(
        true,
        &format!("[GNUPG:] VALIDSIG {} fixture", apt::SIGNER)
    ));
    fs::write(f.root.join("apt/Sources"), b"changed")?;
    assert!(f
        .check()
        .err()
        .ok_or("changed index accepted")?
        .to_string()
        .contains("index digest"));
    let f = Fixture::new()?;
    let path = f.root.join("apt/InRelease");
    let release = fs::read_to_string(&path)?;
    fs::write(
        &path,
        release.replace(
            "Sat, 10 Oct 2026 00:00:00 +0000",
            "Thu, 08 Oct 2026 00:00:00 +0000",
        ),
    )?;
    assert!(f
        .check()
        .err()
        .ok_or("expired release accepted")?
        .to_string()
        .contains("expired"));
    let mut f = Fixture::new()?;
    f.native["packages"][0]["input"]["sha256"] = "b".repeat(64).into();
    assert!(f
        .check()
        .err()
        .ok_or("wrong binary accepted")?
        .to_string()
        .contains("binary input not authenticated"));
    let f = Fixture::new()?;
    let path = f.root.join("sources/inventory.json");
    let mut sources = storage::parse_json(&fs::read(&path)?)?;
    sources["sources"][0]["files"][0]["sha256"] = "b".repeat(64).into();
    fs::write(path, serde_json::to_vec(&sources)?)?;
    assert!(f
        .check()
        .err()
        .ok_or("wrong descriptor accepted")?
        .to_string()
        .contains("source input not authenticated"));
    Ok(())
}

#[test]
fn actual_cli_uses_real_gpgv_and_refuses_unsigned_fixture_without_trusting_mocked_records(
) -> Result<()> {
    let f = Fixture::new()?;
    let native = f.root.join("native.json");
    fs::write(&native, serde_json::to_vec(&f.native)?)?;
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .env("AMONG_CHECK_ROOT", f.root.join("checks"))
        .arg("check-apt-provenance")
        .arg(f.root.join("apt"))
        .arg(native)
        .arg(f.root.join("sources"))
        .output()?;
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        stderr.contains("not signed by the pinned Ubuntu archive key"),
        "unexpected crypto-verifier refusal: {stderr}"
    );
    assert!(output.stdout.is_empty());
    Ok(())
}

#[test]
fn naive_future_collection_wrong_origin_and_future_repository_dates_fail() -> Result<()> {
    for date in ["2026-10-09T00:00:00", "2099-10-09T00:00:00+00:00"] {
        let f = Fixture::new()?;
        let path = f.root.join("apt/inventory.json");
        let mut inventory = storage::parse_json(&fs::read(&path)?)?;
        inventory["collected_at"] = date.into();
        fs::write(path, serde_json::to_vec(&inventory)?)?;
        assert!(f.check().is_err());
    }
    for (before, after) in [
        ("Origin: Ubuntu", "Origin: Other"),
        ("Codename: noble", "Codename: other"),
        (
            "Thu, 08 Oct 2026 23:00:00 +0000",
            "Mon, 12 Oct 2026 00:00:00 +0000",
        ),
    ] {
        let f = Fixture::new()?;
        let path = f.root.join("apt/InRelease");
        let release = fs::read_to_string(&path)?;
        fs::write(path, release.replace(before, after))?;
        assert!(f.check().is_err());
    }
    Ok(())
}
