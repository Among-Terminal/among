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
    fixture_for(root, TAG, VERSION)
}

/// The same artifacts for any tag: `version` is the package version (with
/// `~` before a release-candidate suffix), `plain` the tag without its `v`.
fn fixture_for(root: &Path, tag: &str, version: &str) -> Result<PathBuf> {
    let plain = tag.trim_start_matches('v');
    let inputs = root.join("inputs");
    fs::create_dir(&inputs)?;
    for target in ["debian13", "ubuntu24.04"] {
        for arch in ["amd64", "arm64"] {
            let directory = inputs.join(format!("deb-{target}-{arch}"));
            fs::create_dir_all(directory.join("native-checker"))?;
            let package = format!("among-terminal_{version}-1~{target}_{arch}.deb");
            let archive = format!("among-{plain}-{target}-{arch}.tar.gz");
            fs::write(directory.join(&package), format!("deb {target} {arch}"))?;
            fs::write(directory.join(&archive), format!("tar {target} {arch}"))?;
            fs::write(directory.join("native-checker/among-tools"), "checker")?;
            write_sums(&directory, &[&package, &archive])?;
            let record = json!({"schema":1,"tag":tag,"source_sha":commit(),"target":target,"architecture":arch,
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
            let package = format!("among-{version}-1.fc{release}.{machine}.rpm");
            fs::write(directory.join(&package), format!("rpm {target} {arch}"))?;
            fs::write(directory.join("among-tools"), "checker")?;
            write_sums(&directory, &["among-tools", &package])?;
            let record = json!({"schema":1,"tag":tag,"source_sha":commit(),"target":target,"architecture":arch,
                "filename":package,"sha256":sha(format!("rpm {target} {arch}").as_bytes())?});
            fs::write(
                directory.join("build-record.json"),
                serde_json::to_vec(&record)?,
            )?;
        }
        let directory = inputs.join(format!("rpm-checked-{target}-amd64"));
        fs::create_dir_all(directory.join("rpmbuild/SRPMS"))?;
        fs::create_dir_all(directory.join("rpmbuild/RPMS/x86_64"))?;
        let source = format!("among-{version}-1.fc{release}.src.rpm");
        let package = format!("among-{version}-1.fc{release}.x86_64.rpm");
        fs::write(
            directory.join("rpmbuild/SRPMS").join(&source),
            format!("srpm {target}"),
        )?;
        fs::write(
            directory.join("rpm-digests.txt"),
            format!(
                "{}  /work/rpmbuild/RPMS/x86_64/{package}\n{}  /work/rpmbuild/SRPMS/{source}\n",
                sha(format!("rpm {target} amd64").as_bytes())?,
                sha(format!("srpm {target}").as_bytes())?
            ),
        )?;
    }
    for (arch, machine) in [("amd64", 62u16), ("arm64", 183u16)] {
        let directory = inputs.join(format!("appimage-{arch}"));
        fs::create_dir(&directory)?;
        let name = format!("among-{plain}-{arch}.AppImage");
        fs::write(directory.join(&name), elf(machine))?;
        let image = json!({"filename":name,"sha256":sha(&elf(machine))?,"bytes":64});
        let audit = json!({"appimage":image,"architecture":arch,
            "bundled_libraries":[{"source_package":"gtk4","source_version":"4.14.5+ds-0ubuntu0.1"}],
            "bundled_data_packages":[{"source_package":"adwaita-icon-theme","source_version":"1:46.0-1"}]});
        fs::write(
            directory.join(format!("among-{plain}-{arch}.audit.json")),
            serde_json::to_vec(&audit)?,
        )?;
        // The producer's SHA256SUMS lists only the AppImage.
        write_sums(&directory, &[&name])?;
        sources(
            &inputs.join(format!("appimage-sources-{arch}")),
            &image,
            arch,
        )?;
    }
    Ok(inputs)
}

/// Debian source packages as `collect-appimage-sources` writes them: each
/// descriptor, the files it lists, and `sources.json` naming the AppImage.
fn sources(directory: &Path, image: &serde_json::Value, arch: &str) -> Result<()> {
    fs::create_dir(directory)?;
    let mut records = Vec::new();
    for (source, version, file) in [
        ("gtk4", "4.14.5+ds-0ubuntu0.1", "gtk4_4.14.5+ds.orig.tar.xz"),
        (
            "adwaita-icon-theme",
            "1:46.0-1",
            "adwaita-icon-theme_46.0.orig.tar.xz",
        ),
    ] {
        let bytes = format!("upstream {source}");
        fs::write(directory.join(file), &bytes)?;
        let digest = sha(bytes.as_bytes())?;
        let descriptor = format!(
            "{source}_{}.dsc",
            version.split_once(':').map_or(version, |(_, rest)| rest)
        );
        let text = format!(
            "Format: 3.0 (quilt)\nSource: {source}\nVersion: {version}\nChecksums-Sha256:\n {digest} {} {file}\n",
            bytes.len()
        );
        fs::write(directory.join(&descriptor), &text)?;
        records.push(
            json!({"source_package":source,"source_version":version,"files":[
            {"name":descriptor,"sha256":sha(text.as_bytes())?},{"name":file,"sha256":digest}]}),
        );
    }
    let index =
        json!({"schema":1,"appimage":image,"architecture":arch,"scope":"test","sources":records});
    fs::write(directory.join("sources.json"), serde_json::to_vec(&index)?)?;
    Ok(())
}

fn scratch(prefix: &str) -> Result<PathBuf> {
    storage::private_directory(&std::env::temp_dir(), prefix)
}

#[test]
fn complete_inventory_is_staged_with_version_free_names_and_verifiable_sums() -> Result<()> {
    let root = scratch("among-release-assets")?;
    let inputs = fixture(&root)?;
    let output = root.join("release-assets");
    let names = release_assets::stage(&inputs, TAG, &commit(), &output)?;
    assert_eq!(names.len(), release_assets::FILE_COUNT);
    for expected in [
        "among-terminal-debian13-arm64.deb",
        "among-terminal-ubuntu24.04-amd64.tar.gz",
        "among-terminal-fedora44-arm64.rpm",
        "among-terminal-fedora43-amd64.rpm",
        "among-terminal-fedora44.src.rpm",
        "among-terminal-arm64.AppImage",
        "among-terminal-amd64-appimage-sources.tar",
    ] {
        assert!(names.contains(&expected.to_string()), "{expected}");
    }
    // The source archive carries every verified file under one directory.
    let listing = std::process::Command::new("tar")
        .args(["-tf", "among-terminal-amd64-appimage-sources.tar"])
        .current_dir(&output)
        .output()?;
    assert!(listing.status.success());
    let mut members = String::from_utf8(listing.stdout)?
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();
    members.sort();
    let prefix = "among-terminal-amd64-appimage-sources/";
    assert_eq!(
        members,
        [
            "adwaita-icon-theme_46.0-1.dsc",
            "adwaita-icon-theme_46.0.orig.tar.xz",
            "gtk4_4.14.5+ds-0ubuntu0.1.dsc",
            "gtk4_4.14.5+ds.orig.tar.xz",
            "sources.json"
        ]
        .map(|name| format!("{prefix}{name}"))
    );
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
    let cases: [(&str, Damage); 17] = [
        ("missing AppImage", |inputs| {
            Ok(fs::remove_file(
                inputs.join("appimage-arm64/among-0.1.0-rc.11-arm64.AppImage"),
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
            let name = "among-0.1.0-rc.11-arm64.AppImage";
            fs::write(directory.join(name), elf(62))?;
            write_sums(&directory, &[name])
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
            write_sums(&directory, &["among-0.1.0-rc.11-amd64.audit.json"])
        }),
        ("AppImage source file changed", |inputs| {
            Ok(fs::write(
                inputs.join("appimage-sources-arm64/gtk4_4.14.5+ds.orig.tar.xz"),
                "patched upstream",
            )?)
        }),
        ("undeclared file among the AppImage sources", |inputs| {
            Ok(fs::write(
                inputs.join("appimage-sources-amd64/notes.txt"),
                "x",
            )?)
        }),
        ("bundled library without collected source", |inputs| {
            let path = inputs.join("appimage-amd64/among-0.1.0-rc.11-amd64.audit.json");
            let mut audit: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
            audit["bundled_libraries"]
                .as_array_mut()
                .ok_or("fixture audit")?
                .push(json!({"source_package":"glib2.0","source_version":"2.80.0-6ubuntu3.4"}));
            Ok(fs::write(path, serde_json::to_vec(&audit)?)?)
        }),
        ("sources collected for another AppImage", |inputs| {
            let path = inputs.join("appimage-sources-arm64/sources.json");
            let mut index: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
            index["appimage"]["sha256"] = "f".repeat(64).into();
            Ok(fs::write(path, serde_json::to_vec(&index)?)?)
        }),
        ("audit record of another AppImage", |inputs| {
            let path = inputs.join("appimage-arm64/among-0.1.0-rc.11-arm64.audit.json");
            let mut audit: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
            audit["appimage"]["sha256"] = "f".repeat(64).into();
            Ok(fs::write(path, serde_json::to_vec(&audit)?)?)
        }),
        ("AppImage named for another version", |inputs| {
            let directory = inputs.join("appimage-amd64");
            fs::rename(
                directory.join("among-0.1.0-rc.11-amd64.AppImage"),
                directory.join("among-0.1.0-amd64.AppImage"),
            )?;
            write_sums(&directory, &["among-0.1.0-amd64.AppImage"])
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

/// A Debian package lands in a release slot named by its target and
/// architecture. A build record from another slot means the artifact
/// directories were mixed up, so the package would be published under a name
/// that promises a system it was not built for. The refusal must name the
/// mismatched field, which proves the identity check and not some later
/// filename or digest check caught it.
#[test]
fn debian_records_for_another_target_or_architecture_are_refused() -> Result<()> {
    for (directory, key, foreign) in [
        ("deb-debian13-amd64", "architecture", "arm64"),
        ("deb-ubuntu24.04-arm64", "architecture", "amd64"),
        ("deb-debian13-arm64", "target", "ubuntu24.04"),
        ("deb-ubuntu24.04-amd64", "target", "debian13"),
    ] {
        let root = scratch("among-release-assets-deb-identity")?;
        let inputs = fixture(&root)?;
        let path = inputs.join(directory).join("build-record.json");
        let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
        record[key] = foreign.into();
        fs::write(&path, serde_json::to_vec(&record)?)?;
        let output = root.join("release-assets");
        let error = release_assets::stage(&inputs, TAG, &commit(), &output)
            .err()
            .ok_or(format!("{directory} with {key} {foreign} was accepted"))?
            .to_string();
        assert!(
            error.contains(&format!("build record {key} is \"{foreign}\"")),
            "{directory}: {error}"
        );
        assert!(!output.exists(), "{directory} left partial output");
        fs::remove_dir_all(root)?;
    }
    Ok(())
}

/// The RPM workflow ships an older package in a subdirectory so the installed
/// job can test upgrades. It is lifecycle test input, never a release file.
fn add_lifecycle_fixtures(inputs: &Path) -> Result<()> {
    for target in ["fedora43", "fedora44"] {
        let release = target.trim_start_matches("fedora");
        for (arch, machine) in [("amd64", "x86_64"), ("arm64", "aarch64")] {
            let directory = inputs.join(format!("rpm-{target}-{arch}"));
            let fixture = format!("among-0.1.0~rc.3-1.fc{release}.{machine}.rpm");
            let relative = format!("ci-lifecycle-fixture-not-for-release/{fixture}");
            fs::create_dir(directory.join("ci-lifecycle-fixture-not-for-release"))?;
            fs::write(directory.join(&relative), "older package")?;
            let package = format!("among-{VERSION}-1.fc{release}.{machine}.rpm");
            write_sums(&directory, &["among-tools", &package, &relative])?;
            let path = directory.join("build-record.json");
            let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
            record["older_fixture"] = json!({"filename":fixture,
                "sha256":sha(b"older package")?,"release_asset":false});
            fs::write(path, serde_json::to_vec(&record)?)?;
        }
    }
    Ok(())
}

#[test]
fn rpm_lifecycle_fixtures_are_never_attached() -> Result<()> {
    let root = scratch("among-release-assets-fixture")?;
    let inputs = fixture(&root)?;
    add_lifecycle_fixtures(&inputs)?;
    let output = root.join("release-assets");
    let names = release_assets::stage(&inputs, TAG, &commit(), &output)?;
    assert_eq!(names.len(), release_assets::FILE_COUNT);
    assert!(names.iter().all(|name| !name.contains("rc.3")));
    assert!(!fs::read_to_string(output.join("SHA256SUMS"))?.contains("rc.3"));
    fs::remove_dir_all(&root)?;

    // A record that names the fixture as its package is refused, because the
    // package name is derived from the tag, not taken from the record.
    let root = scratch("among-release-assets-fixture-record")?;
    let inputs = fixture(&root)?;
    add_lifecycle_fixtures(&inputs)?;
    let path = inputs.join("rpm-fedora43-amd64/build-record.json");
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    record["filename"] = record["older_fixture"]["filename"].clone();
    fs::write(&path, serde_json::to_vec(&record)?)?;
    let output = root.join("release-assets");
    assert!(release_assets::stage(&inputs, TAG, &commit(), &output).is_err());
    assert!(!output.exists());
    fs::remove_dir_all(root)?;
    Ok(())
}

/// The README links `releases/latest/download/<name>`; that only works if a
/// stable tag attaches exactly the names a release candidate does.
#[test]
fn a_stable_tag_attaches_the_same_names_as_a_release_candidate() -> Result<()> {
    let root = scratch("among-release-assets-stable")?;
    let inputs = fixture_for(&root, "v0.1.0", "0.1.0")?;
    let output = root.join("release-assets");
    let stable = release_assets::stage(&inputs, "v0.1.0", &commit(), &output)?;
    fs::remove_dir_all(&root)?;

    let root = scratch("among-release-assets-candidate")?;
    let inputs = fixture(&root)?;
    let output = root.join("release-assets");
    let candidate = release_assets::stage(&inputs, TAG, &commit(), &output)?;
    fs::remove_dir_all(&root)?;

    assert_eq!(stable, candidate);
    assert_eq!(
        stable
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        release_assets::attached_names()
    );
    Ok(())
}

/// GitHub renames asset names with characters such as `~`, `+` or spaces, and
/// a renamed asset no longer matches its `SHA256SUMS` line.
#[test]
fn attached_names_carry_no_version_and_no_character_github_renames() {
    let names = release_assets::attached_names();
    assert_eq!(names.len(), release_assets::FILE_COUNT);
    for name in &names {
        assert!(name.starts_with("among-terminal-"), "{name}");
        assert!(!name.contains("0.1"), "{name}");
        assert!(
            name.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)),
            "{name}"
        );
    }
}

#[test]
fn inputs_are_named_exactly_without_runtime_evidence_or_failure_logs() {
    let names = release_assets::input_artifacts();
    // Four DEB, four RPM, two AppImage and two AppImage source artifacts,
    // plus the two amd64 RPM build artifacts that carry the SRPMs.
    assert_eq!(names.len(), 14);
    for internal in [
        "appimage-SHA256SUMS",
        "appimage-checker-amd64",
        "appimage-build-failure-arm64",
    ] {
        assert!(!names.contains(internal), "{internal}");
    }
    assert!(names.contains("rpm-checked-fedora43-amd64"));
    assert!(!names.contains("rpm-checked-fedora43-arm64"));
    assert!(names
        .iter()
        .all(|name| !name.contains("runtime") && !name.contains("failure")));
}
