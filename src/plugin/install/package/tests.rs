use super::*;
use crate::plugin::install::metadata::Catalogue;
use crate::plugin::inventory;
use serde_json::{json, Value};
use std::io::Cursor;

pub(crate) struct OwnedRoot(pub std::path::PathBuf);

impl OwnedRoot {
    pub fn new() -> Self {
        let path = std::env::temp_dir().join(format!("among-package-{}", store::token()));
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}

impl Drop for OwnedRoot {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

pub(crate) fn compressed(tar: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(tar).unwrap();
    encoder.finish().unwrap()
}

fn raw(path: &str, kind: u8, mode: u32, bytes: &[u8]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_ustar();
    header.set_size(bytes.len() as u64);
    header.set_mode(mode);
    header.set_entry_type(tar::EntryType::new(kind));
    header.as_mut_bytes()[..100].fill(0);
    header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
    header.set_cksum();
    builder.append(&header, bytes).unwrap();
    builder.into_inner().unwrap()
}

pub(crate) fn fixture() -> (Value, Vec<u8>) {
    fixture_for("sample")
}

pub(crate) fn fixture_for(id: &str) -> (Value, Vec<u8>) {
    let mut metadata = crate::plugin::install::metadata::tests::single();
    metadata["manifest"]["id"] = json!(id);
    metadata["artifacts"][0]["architecture"] =
        json!(match crate::plugin::install::metadata::Platform::current()
            .unwrap()
            .architecture
        {
            crate::plugin::install::metadata::Architecture::Amd64 => "amd64",
            crate::plugin::install::metadata::Architecture::Arm64 => "arm64",
        });
    metadata["artifacts"][0]["runtime"] =
        json!({"kind":"script","executable":"/usr/bin/sh","minimum":null});
    let manifest = metadata["manifest"].to_string().into_bytes();
    let executable = b"#!/usr/bin/sh\nexit 123\n";
    let hash = inventory::checksum(&manifest).unwrap();
    metadata["manifest_sha256"] = json!(hash);
    metadata["artifacts"][0]["members"] = json!([
        {"path":"among-plugin.json","size":manifest.len(),"sha256":hash,"executable":false},
        {"path":"bin/plugin","size":executable.len(),"sha256":inventory::checksum(executable).unwrap(),"executable":true}
    ]);
    let mut archive = raw("among-plugin.json", b'0', 0o644, &manifest);
    archive.truncate(archive.len() - 1024);
    archive.extend(raw("bin/plugin", b'0', 0o755, executable));
    let bytes = compressed(&archive);
    metadata["artifacts"][0]["size"] = json!(bytes.len());
    metadata["artifacts"][0]["sha256"] = json!(inventory::checksum(&bytes).unwrap());
    (metadata, bytes)
}

pub(crate) fn extract_bytes(
    metadata: &Value,
    bytes: &[u8],
    transaction: &mut store::Transaction,
    check: impl Fn() -> Result<(), Problem>,
) -> Result<(), Problem> {
    transaction.archive.write_all(bytes).unwrap();
    let catalogue = Catalogue::parse(metadata.to_string().as_bytes()).unwrap();
    let version = &catalogue.plugins[0].versions[0];
    let path = transaction.payload_path().to_path_buf();
    extract(
        &mut transaction.archive,
        &transaction.payload,
        &path,
        ManagedIdentity::new(12, "sample".into()).unwrap(),
        version,
        &version.artifacts[0],
        check,
    )
}

#[test]
fn actual_extraction_creates_only_declared_files_without_execution() {
    let root = OwnedRoot::new();
    let (metadata, bytes) = fixture();
    let mut transaction = store::Transaction::new(&root.0, 4096, || Ok(())).unwrap();
    extract_bytes(&metadata, &bytes, &mut transaction, || Ok(())).unwrap();
    assert_eq!(
        std::fs::read(transaction.payload_path().join("bin/plugin")).unwrap(),
        b"#!/usr/bin/sh\nexit 123\n"
    );
    assert_eq!(
        std::fs::metadata(transaction.payload_path().join("bin/plugin"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o700
    );
    assert!(!root.0.join("installed.json").exists());
    let stage = transaction.payload_path().to_path_buf();
    drop(transaction);
    assert!(!stage.exists());
}

#[test]
fn hostile_tar_headers_never_escape_or_replace_unrelated_data() {
    let root = OwnedRoot::new();
    let (metadata, _) = fixture();
    let sentinel = root.0.join("sentinel");
    std::fs::write(&sentinel, b"preserve").unwrap();
    for (path, kind, mode, bytes) in [
        ("../sentinel", b'0', 0o644, &b"overwrite"[..]),
        ("/sentinel", b'0', 0o644, &b"overwrite"[..]),
        ("bin/../sentinel", b'0', 0o644, &b"overwrite"[..]),
        ("bin//plugin", b'0', 0o755, &b"overwrite"[..]),
        ("bin/./plugin", b'0', 0o755, &b"overwrite"[..]),
        ("bin/plugin", b'1', 0o644, &b""[..]),
        ("bin/plugin", b'2', 0o644, &b""[..]),
        ("bin/plugin", b'3', 0o644, &b""[..]),
        ("bin/plugin", b'4', 0o644, &b""[..]),
        ("bin/plugin", b'6', 0o644, &b""[..]),
        ("bin/plugin", b's', 0o644, &b""[..]),
        ("bin/plugin", b'x', 0o644, &b""[..]),
        ("bin/plugin", b'g', 0o644, &b""[..]),
        ("bin/plugin", b'L', 0o644, &b""[..]),
        ("bin/plugin", b'S', 0o644, &b""[..]),
        ("bin/plugin", b'0', 0o4755, &b""[..]),
        ("bin/plugin", b'0', 0o2755, &b""[..]),
        ("bin/plugin", b'0', 0o1755, &b""[..]),
        ("bin/plugin", b'0', 0o777, &b""[..]),
        ("bin", b'5', 0o700, &b"not empty"[..]),
        ("unrelated", b'5', 0o700, &b""[..]),
    ] {
        let mut transaction = store::Transaction::new(&root.0, 4096, || Ok(())).unwrap();
        let result = extract_bytes(
            &metadata,
            &compressed(&raw(path, kind, mode, bytes)),
            &mut transaction,
            || Ok(()),
        );
        assert!(
            result.is_err(),
            "accepted {path:?} kind {kind} mode {mode:o}"
        );
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"preserve");
        assert!(!root.0.join("installed.json").exists());
    }
}

#[test]
fn links_inserted_in_owned_staging_cannot_redirect_file_creation() {
    let (metadata, bytes) = fixture();
    for link_directory in [true, false] {
        let root = OwnedRoot::new();
        let outside = root.0.join("outside");
        std::fs::create_dir(&outside).unwrap();
        let mut transaction = store::Transaction::new(&root.0, 4096, || Ok(())).unwrap();
        if link_directory {
            std::os::unix::fs::symlink(&outside, transaction.payload_path().join("bin")).unwrap();
        } else {
            std::fs::create_dir(transaction.payload_path().join("bin")).unwrap();
            std::os::unix::fs::symlink(
                outside.join("plugin"),
                transaction.payload_path().join("bin/plugin"),
            )
            .unwrap();
        }
        assert!(extract_bytes(&metadata, &bytes, &mut transaction, || Ok(())).is_err());
        assert!(!outside.join("plugin").exists());
        // cleanup refuses unexpected links instead of following or deleting their target.
    }
}

#[test]
fn truncated_crc_appended_stream_duplicate_and_manifest_disagreement_fail() {
    let root = OwnedRoot::new();
    let (metadata, bytes) = fixture();
    let mut crc = bytes.clone();
    let last = crc.len() - 8;
    crc[last] ^= 1;
    let mut appended = bytes.clone();
    appended.extend(&bytes);
    let mut duplicate = Vec::new();
    flate2::read::GzDecoder::new(Cursor::new(&bytes))
        .read_to_end(&mut duplicate)
        .unwrap();
    duplicate.truncate(duplicate.len() - 1024);
    duplicate.extend(raw("bin/plugin", b'0', 0o755, b"#!/usr/bin/sh\nexit 123\n"));
    let mut hidden = bytes.clone();
    hidden.push(0);
    for archive in [
        bytes[..bytes.len() - 4].to_vec(),
        crc,
        appended,
        compressed(&duplicate),
        hidden,
        compressed(&raw("bin/plugin", b'0', 0o755, b"changed")),
    ] {
        let mut transaction = store::Transaction::new(&root.0, 4096, || Ok(())).unwrap();
        assert!(extract_bytes(&metadata, &archive, &mut transaction, || Ok(())).is_err());
    }
    let mut wrong = metadata.clone();
    wrong["manifest"]["name"] = json!("not the packaged manifest");
    let mut transaction = store::Transaction::new(&root.0, 4096, || Ok(())).unwrap();
    assert_eq!(
        extract_bytes(&wrong, &bytes, &mut transaction, || Ok(()))
            .unwrap_err()
            .0,
        "packaged shared manifest differs from the selected preview"
    );
}

#[test]
fn a_real_compressed_padding_bomb_hits_the_expansion_budget_without_selection() {
    let root = OwnedRoot::new();
    let (metadata, bytes) = fixture();
    let mut tar = Vec::new();
    flate2::read::GzDecoder::new(Cursor::new(bytes))
        .read_to_end(&mut tar)
        .unwrap();
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&tar).unwrap();
    std::io::copy(
        &mut std::io::repeat(0).take(MAX_EXPANDED_BYTES + MAX_MEMBERS as u64 * 17 * 1024),
        &mut encoder,
    )
    .unwrap();
    let bomb = encoder.finish().unwrap();
    assert!(bomb.len() < super::super::metadata::MAX_PACKAGE_BYTES as usize);
    let mut transaction = store::Transaction::new(&root.0, 4096, || Ok(())).unwrap();
    let start = Instant::now();
    let error = extract_bytes(&metadata, &bomb, &mut transaction, || Ok(())).unwrap_err();
    assert_eq!(error.0, "invalid gzip trailer or archive expansion budget");
    assert!(start.elapsed() < EXTRACTION_TIME + Duration::from_secs(2));
    assert!(!root.0.join("installed.json").exists());
}

#[test]
fn bounded_reader_checks_cancellation_and_expansion_without_allocating_the_bomb() {
    let count = std::cell::Cell::new(0);
    let check = || {
        count.set(count.get() + 1);
        Ok(())
    };
    let mut bounded = Bounded {
        input: std::io::repeat(0),
        check: &check,
        deadline: Instant::now() + EXTRACTION_TIME,
        remaining: (BLOCK * 2) as u64,
    };
    let mut buffer = vec![0; BLOCK * 4];
    assert_eq!(bounded.read(&mut buffer).unwrap(), BLOCK);
    assert_eq!(bounded.read(&mut buffer).unwrap(), BLOCK);
    assert!(bounded.read(&mut buffer).is_err());
    assert_eq!(count.get(), 3);
    let check = || Err(Problem("cancelled by fixture"));
    let mut bounded = Bounded {
        input: std::io::repeat(0),
        check: &check,
        deadline: Instant::now() + EXTRACTION_TIME,
        remaining: 1,
    };
    assert!(bounded.read(&mut buffer).is_err());
    let check = || Ok(());
    let mut bounded = Bounded {
        input: std::io::repeat(0),
        check: &check,
        deadline: Instant::now(),
        remaining: 1,
    };
    assert!(bounded.read(&mut buffer).is_err());
}
