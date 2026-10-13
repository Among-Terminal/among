use among_tools::{binary_archive, storage, Result};
use std::{fs, io::Read, path::Path};

#[test]
fn actual_archive_retains_exact_executables_notices_and_explicit_runtime_requirements() -> Result<()>
{
    let root = storage::private_directory(&std::env::temp_dir(), "among-raw-archive")?;
    let payload = root.join("payload");
    fs::create_dir_all(payload.join("usr/bin"))?;
    fs::create_dir_all(payload.join("usr/share/among/plugins"))?;
    fs::create_dir_all(payload.join("usr/share/doc/among-terminal/THIRD_PARTY"))?;
    let mut elf = [0u8; 20];
    elf[..6].copy_from_slice(b"\x7fELF\x02\x01");
    elf[18] = 62;
    for relative in [
        "usr/bin/among",
        "usr/share/among/plugins/clock",
        "usr/share/among/plugins/system-status",
        "usr/share/among/plugins/pane-pwd",
    ] {
        fs::write(payload.join(relative), elf)?;
    }
    for name in ["LICENSE", "NOTICE", "THIRD_PARTY/inventory.json"] {
        fs::write(
            payload.join("usr/share/doc/among-terminal").join(name),
            b"synthetic attribution fixture",
        )?;
    }
    let output = root.join("output");
    fs::create_dir(&output)?;
    let (name, digest) = binary_archive::create(
        &payload,
        "v0.1.0-rc.7",
        "ubuntu24.04",
        "amd64",
        "libgtk-4-1, libvte-2.91-gtk4-0",
        &output,
    )?;
    assert_eq!(name, "among-0.1.0-rc.7-ubuntu24.04-amd64.tar.gz");
    assert_eq!(
        digest,
        storage::sha256(fs::File::open(output.join(&name))?)?
    );
    let record = serde_json::json!({"depends":"libgtk-4-1, libvte-2.91-gtk4-0","binary_archive":{"filename":name,"sha256":digest}});
    binary_archive::validate(&record, &output, "v0.1.0-rc.7", "ubuntu24.04", "amd64")?;
    let repeated = root.join("repeated");
    fs::create_dir(&repeated)?;
    assert_eq!(
        binary_archive::create(
            &payload,
            "v0.1.0-rc.7",
            "ubuntu24.04",
            "amd64",
            "libgtk-4-1, libvte-2.91-gtk4-0",
            &repeated
        )?
        .1,
        digest
    );
    assert_eq!(fs::read_dir(&output)?.count(), 1);
    let decoded = std::process::Command::new("/usr/bin/gzip")
        .arg("-dc")
        .arg(output.join(&name))
        .output()?;
    assert!(decoded.status.success());
    let mut members = std::collections::BTreeMap::new();
    for entry in tar::Archive::new(std::io::Cursor::new(decoded.stdout)).entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        members.insert(path, bytes);
    }
    assert_eq!(members.get(Path::new("bin/among")), Some(&elf.to_vec()));
    assert!(String::from_utf8(
        members
            .get(Path::new("INSTALL.txt"))
            .ok_or("instructions absent")?
            .clone()
    )?
    .contains("Libraries are NOT bundled"));
    assert!(members.contains_key(Path::new("THIRD_PARTY/inventory.json")));
    assert_eq!(members.len(), 8);
    let extracted = root.join("extracted");
    binary_archive::extract_checked(
        &record,
        &output,
        "v0.1.0-rc.7",
        "ubuntu24.04",
        "amd64",
        &payload,
        &extracted,
    )?;
    assert_eq!(fs::read(extracted.join("bin/among"))?, elf);
    assert_eq!(
        fs::read(extracted.join("THIRD_PARTY/inventory.json"))?,
        b"synthetic attribution fixture"
    );
    assert!(binary_archive::extract_checked(
        &record,
        &output,
        "v0.1.0-rc.7",
        "ubuntu24.04",
        "amd64",
        &payload,
        &extracted
    )
    .is_err());
    assert_eq!(
        fs::read_dir(&root)?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("among-archive-extraction-"))
            .count(),
        0
    );
    for scenario in [
        "changed-executable",
        "missing-license",
        "unexpected-file",
        "duplicate-executable",
        "symlink",
        "misleading-instructions",
    ] {
        let raw = root.join("malicious.tar");
        let mut builder = tar::Builder::new(fs::File::create(&raw)?);
        for (path, bytes) in &members {
            if scenario == "missing-license" && path == Path::new("LICENSE") {
                continue;
            }
            let mut bytes = bytes.clone();
            if scenario == "changed-executable" && path == Path::new("bin/among") {
                bytes.push(0);
            }
            if scenario == "misleading-instructions" && path == Path::new("INSTALL.txt") {
                bytes = b"No runtime libraries are required on any distribution".to_vec();
            }
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(if path.starts_with("bin") || path.starts_with("plugins") {
                0o755
            } else {
                0o644
            });
            header.set_cksum();
            builder.append_data(&mut header, path, bytes.as_slice())?;
        }
        if matches!(
            scenario,
            "unexpected-file" | "duplicate-executable" | "symlink"
        ) {
            let mut header = tar::Header::new_gnu();
            header.set_size(0);
            header.set_mode(0o644);
            if scenario == "symlink" {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_link_name("/usr/bin/among")?;
            }
            header.set_cksum();
            builder.append_data(
                &mut header,
                if scenario == "duplicate-executable" {
                    "bin/among"
                } else {
                    "foreign"
                },
                std::io::empty(),
            )?;
        }
        builder.finish()?;
        drop(builder);
        let compressed = std::process::Command::new("/usr/bin/gzip")
            .args(["-n", "-c"])
            .arg(&raw)
            .output()?;
        assert!(compressed.status.success());
        fs::write(output.join(&name), &compressed.stdout)?;
        let mut malicious_record = record.clone();
        malicious_record["binary_archive"]["sha256"] =
            storage::sha256(compressed.stdout.as_slice())?.into();
        let refused = root.join(format!("refused-{scenario}"));
        assert!(
            binary_archive::extract_checked(
                &malicious_record,
                &output,
                "v0.1.0-rc.7",
                "ubuntu24.04",
                "amd64",
                &payload,
                &refused
            )
            .is_err(),
            "{scenario}"
        );
        assert!(!refused.exists(), "{scenario}");
    }
    assert!(binary_archive::create(
        &payload,
        "v0.1.0-rc.7",
        "ubuntu24.04",
        "arm64",
        "libgtk-4-1",
        &output
    )
    .is_err());
    assert!(binary_archive::create(&payload, "v0.1.0", "universal", "amd64", "", &output).is_err());
    fs::write(output.join(&name), b"corrupted")?;
    assert!(
        binary_archive::validate(&record, &output, "v0.1.0-rc.7", "ubuntu24.04", "amd64").is_err()
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

/// Users verify a downloaded candidate with `sha256sum --check SHA256SUMS`.
/// The file written next to the DEB and the archive must satisfy exactly that
/// command, and the same command must reject an archive changed after the
/// checksum was written.
#[test]
fn candidate_checksums_verify_the_deb_and_the_archive_with_sha256sum() -> Result<()> {
    let root = storage::private_directory(&std::env::temp_dir(), "among-candidate-sums")?;
    let payload = root.join("payload");
    fs::create_dir_all(payload.join("usr/bin"))?;
    fs::create_dir_all(payload.join("usr/share/among/plugins"))?;
    fs::create_dir_all(payload.join("usr/share/doc/among-terminal/THIRD_PARTY"))?;
    let mut elf = [0u8; 20];
    elf[..6].copy_from_slice(b"\x7fELF\x02\x01");
    elf[18] = 62;
    for relative in [
        "usr/bin/among",
        "usr/share/among/plugins/clock",
        "usr/share/among/plugins/system-status",
        "usr/share/among/plugins/pane-pwd",
    ] {
        fs::write(payload.join(relative), elf)?;
    }
    for name in ["LICENSE", "NOTICE", "THIRD_PARTY/inventory.json"] {
        fs::write(
            payload.join("usr/share/doc/among-terminal").join(name),
            b"synthetic attribution fixture",
        )?;
    }
    let candidate = root.join("candidate");
    fs::create_dir(&candidate)?;
    let deb = "among-terminal_0.1.0_amd64.deb";
    fs::write(candidate.join(deb), b"synthetic package bytes")?;
    let deb_digest = storage::sha256(fs::File::open(candidate.join(deb))?)?;
    let (archive, archive_digest) = binary_archive::create(
        &payload,
        "v0.1.0",
        "ubuntu24.04",
        "amd64",
        "libgtk-4-1",
        &candidate,
    )?;
    fs::write(
        candidate.join("SHA256SUMS"),
        among_tools::deb_build::checksum_lines(&[(&deb_digest, deb), (&archive_digest, &archive)]),
    )?;
    let check = |directory: &Path| {
        std::process::Command::new("/usr/bin/sha256sum")
            .args(["--check", "SHA256SUMS"])
            .current_dir(directory)
            .output()
    };
    let verified = check(&candidate)?;
    assert!(verified.status.success());
    let report = String::from_utf8(verified.stdout)?;
    assert!(report.contains(&format!("{deb}: OK")), "{report}");
    assert!(report.contains(&format!("{archive}: OK")), "{report}");
    fs::write(candidate.join(&archive), b"changed after the checksum")?;
    assert!(!check(&candidate)?.status.success());
    fs::remove_dir_all(root)?;
    Ok(())
}
