use super::*;
use std::os::unix::fs::symlink;

const SOURCE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Result<Self> {
        Ok(Self(storage::private_directory(
            &std::env::temp_dir(),
            "among-profile-transfer-test",
        )?))
    }

    fn envelope(&self) -> Result<Value> {
        // synthetic transfer-layer controls only; these bytes must never pass
        // the independent catalogue/receipt/access checks in profile::replay.
        fs::create_dir(self.0.join("managed"))?;
        fs::write(
            self.0.join("profile.json"),
            serde_json::to_vec(&json!({"host_source_sha":SOURCE}))?,
        )?;
        for path in [
            "managed/installed.json",
            "managed-plugins.json",
            "among-plugins.json",
        ] {
            fs::write(self.0.join(path), b"{}")?;
        }
        let hashes=inventory(&self.0)?.iter().map(|(path,bytes)|Ok(json!({"path":path,"size":bytes.len(),"sha256":storage::sha256(bytes.as_slice())?}))).collect::<Result<Vec<_>>>()?;
        Ok(
            json!({"schema":1,"host_source_sha":SOURCE,"host_test_sha256":"b".repeat(64),"cargo_records_sha256":"d".repeat(64),"architecture":crate::platform::discover()?.architecture,"files":hashes,
            "lifecycle":{"architecture":crate::platform::discover()?.architecture,"source_revision":super::super::release::SOURCE,"catalogue_revision":super::super::release::CATALOGUE,"actual_public_install":true,"explicit_review_cancel_accept":true,"three_mapped_outputs":true,"disabled_then_uninstalled":true,"same_pty_real_input":true}}),
        )
    }

    fn save(&self, record: &Value) -> Result<()> {
        fs::write(self.0.join("transfer.json"), serde_json::to_vec(record)?)?;
        Ok(())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn transfer_refuses_wrong_host_source_architecture_and_missing_lifecycle_boundaries() -> Result<()>
{
    for field in [
        "host_source_sha",
        "host_test_sha256",
        "architecture",
        "lifecycle",
    ] {
        let fixture = Fixture::new()?;
        let mut record = fixture.envelope()?;
        fixture.save(&record)?;
        validate(&fixture.0, SOURCE)?;
        assert!(profile::replay(&fixture.0, &fixture.0.join("not-an-installation")).is_err());
        record[field] = match field {
            "host_source_sha" => json!("c".repeat(40)),
            "host_test_sha256" => json!("not-a-digest"),
            "architecture" => json!(if crate::platform::discover()?.architecture == "amd64" {
                "arm64"
            } else {
                "amd64"
            }),
            _ => json!({}),
        };
        fixture.save(&record)?;
        assert!(validate(&fixture.0, SOURCE).is_err(), "{field}");
    }
    Ok(())
}

#[test]
fn transfer_refuses_modified_missing_duplicate_unrecorded_and_link_inputs() -> Result<()> {
    for mutation in [
        "modified",
        "missing",
        "duplicate",
        "unrecorded",
        "link",
        "source-marker",
    ] {
        let fixture = Fixture::new()?;
        let mut record = fixture.envelope()?;
        fixture.save(&record)?;
        let settings = fixture.0.join("managed-plugins.json");
        match mutation {
            "modified" => fs::write(&settings, b"changed grants")?,
            "missing" => fs::remove_file(&settings)?,
            "duplicate" => {
                let duplicate = record["files"][0].clone();
                record["files"]
                    .as_array_mut()
                    .ok_or("fixture transfer files absent")?
                    .push(duplicate);
                fixture.save(&record)?;
            }
            "unrecorded" => fs::write(fixture.0.join("unexpected-input"), b"extra")?,
            "link" => {
                fs::remove_file(&settings)?;
                symlink("among-plugins.json", settings)?;
            }
            _ => fs::write(
                fixture.0.join("profile.json"),
                serde_json::to_vec(&json!({"host_source_sha":"c".repeat(40)}))?,
            )?,
        }
        assert!(validate(&fixture.0, SOURCE).is_err(), "{mutation}");
    }
    Ok(())
}

#[test]
fn cargo_host_binding_refuses_foreign_manifest_source_target_and_multiple_executables() -> Result<()>
{
    let fixture = Fixture::new()?;
    let record = json!({"reason":"compiler-artifact","manifest_path":fixture.0.join("Cargo.toml"),"target":{"src_path":fixture.0.join("src/main.rs"),"name":"among"},"profile":{"test":true},"executable":"/nonexistent/among-host-test"});
    assert_eq!(
        host_test(&fixture.0, &record.to_string())?,
        Path::new("/nonexistent/among-host-test")
    );
    for field in ["manifest_path", "src_path", "name", "executable"] {
        let mut changed = record.clone();
        match field {
            "manifest_path" => changed[field] = json!("/foreign/Cargo.toml"),
            "src_path" => changed["target"][field] = json!("/foreign/src/main.rs"),
            "name" => changed["target"][field] = json!("foreign-host"),
            _ => changed[field] = json!("/nonexistent/another-test"),
        }
        let records = if field == "executable" {
            format!("{record}\n{changed}")
        } else {
            changed.to_string()
        };
        assert!(host_test(&fixture.0, &records).is_err(), "{field}");
    }
    assert!(host_test(&fixture.0, &format!("{record}\n{record}")).is_err());
    Ok(())
}

#[test]
fn source_receipt_refuses_dirty_tracked_and_untracked_work_with_unchanged_head() -> Result<()> {
    let fixture = Fixture::new()?;
    let git = |args: &[&str]| -> Result<()> {
        let status = std::process::Command::new("/usr/bin/git")
            .current_dir(&fixture.0)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args([
                "-c",
                "user.name=Among profile test",
                "-c",
                "user.email=profile-test@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()?;
        if !status.status.success() {
            return Err(String::from_utf8_lossy(&status.stderr).into_owned().into());
        }
        Ok(())
    };
    git(&["init", "--quiet"])?;
    fs::write(fixture.0.join("tracked"), b"original")?;
    git(&["add", "tracked"])?;
    git(&["commit", "--quiet", "-m", "fixture"])?;
    let source = clean_source(&fixture.0)?;
    fs::write(fixture.0.join("tracked"), b"dirty same HEAD")?;
    assert_eq!(
        release_policy::git(&fixture.0, &["rev-parse", "HEAD"])?,
        source
    );
    assert!(clean_source(&fixture.0).is_err());
    fs::write(fixture.0.join("tracked"), b"original")?;
    assert_eq!(clean_source(&fixture.0)?, source);
    fs::write(fixture.0.join("untracked"), b"unknown source")?;
    assert!(clean_source(&fixture.0).is_err());
    Ok(())
}
