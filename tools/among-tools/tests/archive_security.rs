use among_tools::{source_delivery, storage, Result};
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt};

// Construct raw records: Builder must not normalize away conflicting sizes.
fn record(
    out: &mut Vec<u8>,
    name: &str,
    kind: tar::EntryType,
    size: u64,
    body: &[u8],
) -> Result<()> {
    let mut header = tar::Header::new_ustar();
    header.set_path(name)?;
    header.set_entry_type(kind);
    header.set_mode(0o755);
    header.set_size(size);
    if kind.is_symlink() || kind.is_hard_link() {
        header.set_link_name("/outside")?;
    }
    header.set_cksum();
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(body);
    out.resize(out.len().next_multiple_of(512), 0);
    Ok(())
}

fn finish(mut archive: Vec<u8>, body: &[u8]) -> Result<Vec<u8>> {
    let inventory = serde_json::to_vec(&serde_json::json!({
        "schema": 1, "files": {"original-source.tar": storage::sha256(body)?}
    }))?;
    record(
        &mut archive,
        "delivery-inventory.json",
        tar::EntryType::Regular,
        inventory.len() as u64,
        &inventory,
    )?;
    archive.resize(archive.len() + 1024, 0);
    Ok(archive)
}

fn check_consumers(archive: &[u8], body: &[u8]) -> Result<()> {
    let digest = storage::sha256(body)?;
    assert_eq!(
        source_delivery::inventory(archive, "delivery-inventory.json")?,
        BTreeMap::from([("original-source.tar".into(), digest.clone())])
    );
    let hashes = storage::inspect_tar(archive)?;
    assert_eq!(hashes.len(), 2);
    assert_eq!(hashes["original-source.tar"], digest);
    Ok(())
}

#[test]
fn pax_size_overrides_nonzero_header_without_exposing_embedded_link() -> Result<()> {
    let mut body = b"regular\n".to_vec();
    body.resize(512, 0);
    record(&mut body, "smuggled", tar::EntryType::Symlink, 0, &[])?;
    body.resize(2048, 0);
    let mut archive = Vec::new();
    record(
        &mut archive,
        "pax",
        tar::EntryType::XHeader,
        13,
        b"13 size=2048\n",
    )?;
    record(
        &mut archive,
        "original-source.tar",
        tar::EntryType::Regular,
        8,
        &body,
    )?;
    check_consumers(&finish(archive, &body)?, &body)
}

#[test]
fn pax_size_skips_gnu_longname_and_longlink_intermediate_headers() -> Result<()> {
    let body = vec![b'a'; 2048];
    for kind in [tar::EntryType::GNULongName, tar::EntryType::GNULongLink] {
        let mut archive = Vec::new();
        record(
            &mut archive,
            "pax",
            tar::EntryType::XHeader,
            13,
            b"13 size=2048\n",
        )?;
        let extension: &[u8] = if kind == tar::EntryType::GNULongName {
            b"original-source.tar\0"
        } else {
            b"unused-link-target\0"
        };
        record(
            &mut archive,
            "././@LongLink",
            kind,
            extension.len() as u64,
            extension,
        )?;
        record(
            &mut archive,
            "original-source.tar",
            tar::EntryType::Regular,
            8,
            &body,
        )?;
        check_consumers(&finish(archive, &body)?, &body)?;
    }
    Ok(())
}

#[test]
fn real_links_and_directories_remain_rejected_without_chmod_or_writes() -> Result<()> {
    let outside = storage::private_directory(&std::env::temp_dir(), "among-archive-outside")?;
    let result = (|| -> Result<()> {
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o700))?;
        fs::write(outside.join("sentinel"), b"unchanged")?;
        for kind in [
            tar::EntryType::Symlink,
            tar::EntryType::Link,
            tar::EntryType::Directory,
        ] {
            let mut archive = Vec::new();
            let mut header = tar::Header::new_ustar();
            header.set_path("linked")?;
            header.set_entry_type(kind);
            header.set_size(0);
            header.set_mode(0o755);
            if kind.is_symlink() || kind.is_hard_link() {
                header.set_link_name(&outside)?;
            }
            header.set_cksum();
            archive.extend_from_slice(header.as_bytes());
            // The directory following the link must not cause extraction/chmod.
            record(&mut archive, "linked/", tar::EntryType::Directory, 0, &[])?;
            archive.resize(archive.len() + 1024, 0);
            assert_eq!(
                storage::inspect_tar(&archive)
                    .err()
                    .map(|e| e.to_string())
                    .as_deref(),
                Some("archive members must be regular files")
            );
            assert_eq!(
                source_delivery::inventory(archive.as_slice(), "delivery-inventory.json")
                    .err()
                    .map(|e| e.to_string())
                    .as_deref(),
                Some("source archive members must be regular files")
            );
            assert_eq!(fs::metadata(&outside)?.permissions().mode() & 0o777, 0o700);
            assert_eq!(fs::read(outside.join("sentinel"))?, b"unchanged");
        }
        Ok(())
    })();
    fs::remove_dir_all(outside)?;
    result
}

#[test]
fn pax_gnu_chain_cannot_hide_a_real_link_or_truncated_payload() -> Result<()> {
    for kind in [tar::EntryType::Symlink, tar::EntryType::Link] {
        let mut archive = Vec::new();
        record(
            &mut archive,
            "pax",
            tar::EntryType::XHeader,
            10,
            b"10 size=0\n",
        )?;
        record(
            &mut archive,
            "././@LongLink",
            tar::EntryType::GNULongName,
            7,
            b"linked\0",
        )?;
        record(&mut archive, "short", kind, 0, &[])?;
        archive.resize(archive.len() + 1024, 0);
        assert_eq!(
            storage::inspect_tar(&archive)
                .err()
                .map(|e| e.to_string())
                .as_deref(),
            Some("archive members must be regular files")
        );
        assert_eq!(
            source_delivery::inventory(archive.as_slice(), "delivery-inventory.json")
                .err()
                .map(|e| e.to_string())
                .as_deref(),
            Some("source archive members must be regular files")
        );
    }
    let mut truncated = Vec::new();
    record(
        &mut truncated,
        "pax",
        tar::EntryType::XHeader,
        13,
        b"13 size=2048\n",
    )?;
    record(
        &mut truncated,
        "original-source.tar",
        tar::EntryType::Regular,
        8,
        b"short",
    )?;
    assert!(storage::inspect_tar(&truncated).is_err());
    assert!(source_delivery::inventory(truncated.as_slice(), "delivery-inventory.json").is_err());
    Ok(())
}

#[test]
fn dependency_unpack_refuses_directory_over_symlink_without_chmod_outside() -> Result<()> {
    // Project consumers reject links before extraction. Also exercise the
    // dependency's formerly vulnerable unpack boundary rather than infer its fix.
    let base = storage::private_directory(&std::env::temp_dir(), "among-tar-chmod")?;
    let outside = base.join("outside");
    let destination = base.join("destination");
    fs::create_dir(&outside)?;
    fs::create_dir(&destination)?;
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o700))?;
    fs::write(outside.join("sentinel"), b"unchanged")?;
    let mut link = tar::Header::new_ustar();
    link.set_path("linked")?;
    link.set_entry_type(tar::EntryType::Symlink);
    link.set_size(0);
    link.set_mode(0o777);
    link.set_link_name(&outside)?;
    link.set_cksum();
    let mut archive = link.as_bytes().to_vec();
    record(&mut archive, "linked", tar::EntryType::Directory, 0, &[])?;
    archive.resize(archive.len() + 1024, 0);
    let result = tar::Archive::new(archive.as_slice()).unpack(&destination);
    let mode = fs::metadata(&outside)?.permissions().mode() & 0o777;
    let sentinel = fs::read(outside.join("sentinel"))?;
    fs::remove_dir_all(base)?;
    assert!(result.is_err(), "directory over symlink must fail");
    assert_eq!(mode, 0o700, "external directory permissions changed");
    assert_eq!(sentinel, b"unchanged");
    Ok(())
}
