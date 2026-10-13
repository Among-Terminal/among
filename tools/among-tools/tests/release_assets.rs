//! Release assembly must attach exactly the files the tag's checked builds
//! produced, under names GitHub keeps, with a checksum list that verifies them.
//! Each refusal here stands for a way a wrong or unchecked file could reach a
//! release draft.

use among_tools::{release_assets, storage, Result};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
};

const TAG: &str = "v0.1.0-rc.11";
const VERSION: &str = "0.1.0~rc.11";

fn commit() -> String {
    "c".repeat(40)
}

fn elf(machine: u16) -> Vec<u8> {
    let mut bytes = vec![0u8; 64];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    bytes
}

fn sha(bytes: &[u8]) -> Result<String> {
    storage::sha256(bytes)
}

fn write_sums(directory: &Path, names: &[&str]) -> Result<()> {
    let mut text = String::new();
    for name in names {
        text.push_str(&format!(
            "{}  {name}\n",
            sha(&fs::read(directory.join(name))?)?
        ));
    }
    fs::write(directory.join("SHA256SUMS"), text)?;
    Ok(())
}

/// Artifact directories shaped like the ones `deb-release.yml`,
/// `rpm-release.yml` and the AppImage workflow upload.
fn fixture(root: &Path) -> Result<PathBuf> {
    let inputs = root.join("inputs");
    fs::create_dir(&inputs)?;
    for target in ["debian13", "ubuntu24.04"] {
        for arch in ["amd64", "arm64"] {
            let directory = inputs.join(format!("deb-{target}-{arch}"));
            fs::create_dir_all(directory.join("native-checker"))?;
            let package = format!("among-terminal_{VERSION}-1~{target}_{arch}.deb");
            let archive = format!("among-0.1.0-rc.11-{target}-{arch}.tar.gz");
            fs::write(directory.join(&package), format!("deb {target} {arch}"))?;
            fs::write(directory.join(&archive), format!("tar {target} {arch}"))?;
            fs::write(directory.join("native-checker/among-tools"), "checker")?;
            write_sums(&directory, &[&package, &archive])?;
            let record = json!({"schema":1,"tag":TAG,"source_sha":commit(),"target":target,"architecture":arch,
                "filename":package,"sha256":sha(format!("deb {target} {arch}").as_bytes())?,
                "binary_archive":{"filename":archive,"sha256":sha(format!("tar {target} {arch}").as_bytes())?}});
            fs::write(
                directory.join("build-record.json"),
                serde_json::to_vec(&record)?,
            )?;
        }
    }
    for target in ["fedora43", "fedora44"] {
        let release = target.trim_start_matches("fedora");
        for (arch, machine) in [("amd64", "x86_64"), ("arm64", "aarch64")] {
            let directory = inputs.join(format!("rpm-{target}-{arch}"));
            fs::create_dir(&directory)?;
            let package = format!("among-{VERSION}-1.fc{release}.{machine}.rpm");
            fs::write(directory.join(&package), format!("rpm {target} {arch}"))?;
            fs::write(directory.join("among-tools"), "checker")?;
            write_sums(&directory, &["among-tools", &package])?;
            let record = json!({"schema":1,"tag":TAG,"source_sha":commit(),"target":target,"architecture":arch,
                "filename":package,"sha256":sha(format!("rpm {target} {arch}").as_bytes())?});
            fs::write(
                directory.join("build-record.json"),
                serde_json::to_vec(&record)?,
            )?;
        }
        let directory = inputs.join(format!("rpm-checked-{target}-amd64"));
        fs::create_dir_all(directory.join("rpmbuild/SRPMS"))?;
        fs::create_dir_all(directory.join("rpmbuild/RPMS/x86_64"))?;
        let source = format!("among-{VERSION}-1.fc{release}.src.rpm");
        let package = format!("among-{VERSION}-1.fc{release}.x86_64.rpm");
        fs::write(
            directory.join("rpmbuild/SRPMS").join(&source),
            format!("srpm {target}"),
        )?;
        fs::write(
            directory.join("rpm-digests.txt"),
            format!(
                "{}  /tmp/opencode/rpmbuild/RPMS/x86_64/{package}\n{}  /tmp/opencode/rpmbuild/SRPMS/{source}\n",
                sha(format!("rpm {target} amd64").as_bytes())?,
                sha(format!("srpm {target}").as_bytes())?
            ),
        )?;
    }
    for (arch, machine) in [("amd64", 62u16), ("arm64", 183u16)] {
        let directory = inputs.join(format!("appimage-{arch}"));
        fs::create_dir(&directory)?;
        let name = format!("Among-0.1.0-rc.11-{arch}.AppImage");
        fs::write(directory.join(&name), elf(machine))?;
        fs::write(directory.join("appimage-audit.json"), "{}")?;
        write_sums(&directory, &[&name, "appimage-audit.json"])?;
    }
    Ok(inputs)
}

fn scratch(prefix: &str) -> Result<PathBuf> {
    storage::private_directory(&std::env::temp_dir(), prefix)
}

#[test]
fn complete_inventory_is_staged_with_github_safe_names_and_verifiable_sums() -> Result<()> {
    let root = scratch("among-release-assets")?;
    let inputs = fixture(&root)?;
    let output = root.join("release-assets");
    let names = release_assets::stage(&inputs, TAG, &commit(), &output)?;
    assert_eq!(names.len(), release_assets::FILE_COUNT);
    assert!(names.contains(&"among-terminal_0.1.0.rc.11-1.debian13_arm64.deb".to_string()));
    assert!(names.contains(&"among-0.1.0.rc.11-1.fc44.src.rpm".to_string()));
    assert!(names.contains(&"among-0.1.0-rc.11-ubuntu24.04-amd64.tar.gz".to_string()));
    assert!(names.contains(&"Among-0.1.0-rc.11-arm64.AppImage".to_string()));
    assert!(names.iter().all(|name| !name.contains('~')));
    // Internal checkers, build records and audit records are never attached.
    let mut present = fs::read_dir(&output)?
        .map(|entry| Ok(entry?.file_name().into_string().map_err(|_| "name")?))
        .collect::<Result<Vec<_>>>()?;
    present.sort();
    let mut expected = names.clone();
    expected.push("SHA256SUMS".into());
    expected.sort();
    assert_eq!(present, expected);
    let check = std::process::Command::new("sha256sum")
        .args(["--check", "--strict", "SHA256SUMS"])
        .current_dir(&output)
        .output()?;
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_eq!(
        String::from_utf8(check.stdout)?.lines().count(),
        release_assets::FILE_COUNT
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

/// Every damaged input must stop the assembly before any output exists.
#[test]
fn damaged_or_foreign_inputs_are_refused_before_output() -> Result<()> {
    type Damage = fn(&Path) -> Result<()>;
    let cases: [(&str, Damage); 11] = [
        ("missing AppImage", |inputs| {
            Ok(fs::remove_file(
                inputs.join("appimage-arm64/Among-0.1.0-rc.11-arm64.AppImage"),
            )?)
        }),
        ("second DEB candidate", |inputs| {
            Ok(fs::write(
                inputs.join("deb-debian13-amd64/among-terminal_0.1.0-1~debian13_amd64.deb"),
                "stale",
            )?)
        }),
        ("record from another tag", |inputs| {
            let path = inputs.join("rpm-fedora43-arm64/build-record.json");
            let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
            record["tag"] = "v0.1.0-rc.10".into();
            Ok(fs::write(path, serde_json::to_vec(&record)?)?)
        }),
        ("record from another commit", |inputs| {
            let path = inputs.join("deb-ubuntu24.04-arm64/build-record.json");
            let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
            record["source_sha"] = "d".repeat(40).into();
            Ok(fs::write(path, serde_json::to_vec(&record)?)?)
        }),
        ("record for another architecture", |inputs| {
            let path = inputs.join("rpm-fedora44-amd64/build-record.json");
            let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
            record["architecture"] = "arm64".into();
            Ok(fs::write(path, serde_json::to_vec(&record)?)?)
        }),
        ("bytes differ from the build record", |inputs| {
            let directory = inputs.join("deb-debian13-arm64");
            let package = "among-terminal_0.1.0~rc.11-1~debian13_arm64.deb";
            fs::write(directory.join(package), "rebuilt")?;
            write_sums(
                &directory,
                &[package, "among-0.1.0-rc.11-debian13-arm64.tar.gz"],
            )
        }),
        ("artifact checksum list does not match", |inputs| {
            Ok(fs::write(
                inputs.join("rpm-fedora43-amd64/among-tools"),
                "replaced checker",
            )?)
        }),
        ("AppImage for the wrong architecture", |inputs| {
            let directory = inputs.join("appimage-arm64");
            let name = "Among-0.1.0-rc.11-arm64.AppImage";
            fs::write(directory.join(name), elf(62))?;
            write_sums(&directory, &[name, "appimage-audit.json"])
        }),
        (
            "SRPM from a build whose binary RPM was not checked",
            |inputs| {
                let path = inputs.join("rpm-checked-fedora44-amd64/rpm-digests.txt");
                let text = fs::read_to_string(&path)?;
                let (_, rest) = text.split_once("  ").ok_or("fixture digests")?;
                Ok(fs::write(path, format!("{}  {rest}", "e".repeat(64)))?)
            },
        ),
        ("SRPM bytes differ from the build digest", |inputs| {
            Ok(fs::write(
                inputs.join(
                    "rpm-checked-fedora43-amd64/rpmbuild/SRPMS/among-0.1.0~rc.11-1.fc43.src.rpm",
                ),
                "other source",
            )?)
        }),
        ("AppImage absent from its checksum list", |inputs| {
            let directory = inputs.join("appimage-amd64");
            write_sums(&directory, &["appimage-audit.json"])
        }),
    ];
    for (name, damage) in cases {
        let root = scratch("among-release-assets-refusal")?;
        let inputs = fixture(&root)?;
        damage(&inputs)?;
        let output = root.join("release-assets");
        assert!(
            release_assets::stage(&inputs, TAG, &commit(), &output).is_err(),
            "{name} was accepted"
        );
        assert!(!output.exists(), "{name} left partial output");
        fs::remove_dir_all(root)?;
    }
    Ok(())
}

#[test]
fn run_identity_and_existing_output_are_refused() -> Result<()> {
    let root = scratch("among-release-assets-identity")?;
    let inputs = fixture(&root)?;
    let output = root.join("release-assets");
    assert!(release_assets::stage(&inputs, "v0.1.0-rc.12", &commit(), &output).is_err());
    assert!(release_assets::stage(&inputs, "main", &commit(), &output).is_err());
    assert!(release_assets::stage(&inputs, TAG, "C".repeat(40).as_str(), &output).is_err());
    assert!(!output.exists());
    fs::create_dir(&output)?;
    assert!(release_assets::stage(&inputs, TAG, &commit(), &output).is_err());
    assert_eq!(fs::read_dir(&output)?.count(), 0);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn asset_names_map_tilde_and_refuse_characters_github_would_rename() -> Result<()> {
    assert_eq!(
        release_assets::asset_name("among-terminal_0.1.0~rc.11-1~debian13_amd64.deb")?,
        "among-terminal_0.1.0.rc.11-1.debian13_amd64.deb"
    );
    for unsafe_name in [
        "a+b.rpm",
        "a b.deb",
        ".hidden",
        "trailing.",
        "ü.AppImage",
        "",
    ] {
        assert!(
            release_assets::asset_name(unsafe_name).is_err(),
            "{unsafe_name:?}"
        );
    }
    Ok(())
}

#[test]
fn inputs_are_named_exactly_without_runtime_evidence_or_failure_logs() {
    let names = release_assets::input_artifacts();
    // Four DEB, four RPM and two AppImage artifacts, plus the two amd64
    // RPM build artifacts that carry the SRPMs.
    assert_eq!(names.len(), 12);
    assert!(names.contains("rpm-checked-fedora43-amd64"));
    assert!(!names.contains("rpm-checked-fedora43-arm64"));
    assert!(names
        .iter()
        .all(|name| !name.contains("runtime") && !name.contains("failure")));
}
