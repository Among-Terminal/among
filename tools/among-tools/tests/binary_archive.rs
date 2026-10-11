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
    let record = serde_json::json!({"binary_archive":{"filename":name,"sha256":digest}});
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
