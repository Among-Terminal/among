use among_tools::{source_snapshot as snapshot, storage, Result};
use proc_macro2::{TokenStream, TokenTree};
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Result<Self> {
        let base = storage::private_directory(&std::env::temp_dir(), "among-source-fixture")?;
        let root = base.join("public");
        fs::create_dir(&root)?;
        for name in snapshot::FILES {
            let path = root.join(name);
            fs::create_dir_all(path.parent().ok_or("fixture has no parent")?)?;
            fs::write(path, format!("fixture public input: {name}"))?;
        }
        fs::create_dir_all(base.join("vendor/fixture"))?;
        fs::write(base.join("vendor/fixture/.cargo-checksum.json"), b"{}")?;
        Ok(Self(base))
    }
    fn root(&self) -> PathBuf {
        self.0.join("public")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn deterministic_archive_excludes_private_and_compiled_state_preserves_modes_and_inventory(
) -> Result<()> {
    let f = Fixture::new()?;
    let root = f.root();
    fs::write(root.join(".env"), b"synthetic secret not public")?;
    fs::create_dir_all(root.join("video/readme-promo"))?;
    fs::write(
        root.join("video/readme-promo/package.json"),
        b"private capture project",
    )?;
    fs::create_dir(root.join(".opencode"))?;
    fs::write(root.join(".opencode/private"), b"synthetic evidence")?;
    fs::create_dir_all(root.join("tools/among-tools/target"))?;
    fs::write(
        root.join("tools/among-tools/target/private"),
        b"compiled cache",
    )?;
    fs::create_dir_all(root.join("scripts/__pycache__"))?;
    fs::write(root.join("scripts/__pycache__/cache"), b"inert cache")?;
    fs::write(root.join("scripts/launcher.sh"), b"#!/bin/sh\nexit 0\n")?;
    fs::set_permissions(
        root.join("scripts/launcher.sh"),
        fs::Permissions::from_mode(0o755),
    )?;
    let one = f.0.join("one.tar");
    let two = f.0.join("two.tar");
    let receipts = [
        snapshot::snapshot(&root, &f.0.join("vendor"), &one)?,
        snapshot::snapshot(&root, &f.0.join("vendor"), &two)?,
    ];
    assert_eq!(receipts[0].archive_sha256, receipts[1].archive_sha256);
    assert_eq!(fs::read(&one)?, fs::read(&two)?);
    let bytes = fs::read(&one)?;
    let mut hashes = storage::inspect_tar(&bytes)?;
    for forbidden in [
        ".env",
        ".opencode/private",
        "tools/among-tools/target/private",
        "scripts/__pycache__/cache",
        "video/readme-promo/package.json",
    ] {
        assert!(!hashes.contains_key(forbidden));
    }
    for required in [
        "tools/among-tools/Cargo.toml",
        "tools/among-tools/Cargo.lock",
        ".cargo/config.toml",
    ] {
        assert!(hashes.contains_key(required));
    }
    assert_eq!(hashes.len(), receipts[0].files + 2);
    let mut archive = tar::Archive::new(bytes.as_slice());
    let mut inventory = None;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let name = entry.path()?.into_owned();
        assert_eq!(
            entry.header().mode()?,
            if name == std::path::Path::new("scripts/launcher.sh") {
                0o755
            } else {
                0o644
            }
        );
        assert_eq!(entry.header().uid()?, 0);
        assert_eq!(entry.header().gid()?, 0);
        assert_eq!(entry.header().mtime()?, 0);
        if name == std::path::Path::new("source-inventory.json") {
            let mut body = Vec::new();
            entry.read_to_end(&mut body)?;
            inventory = Some(storage::parse_json(&body)?);
        }
    }
    hashes.remove("source-inventory.json");
    assert_eq!(
        inventory.ok_or("source inventory absent")?["files"],
        serde_json::to_value(hashes)?
    );
    assert!(snapshot::snapshot(&root, &f.0.join("vendor"), &one).is_err());
    assert_eq!(
        storage::sha256(fs::File::open(&one)?)?,
        receipts[0].archive_sha256
    );
    Ok(())
}

#[test]
fn square_artwork_and_packaged_copy_reach_the_source_archive_unchanged() -> Result<()> {
    let f = Fixture::new()?;
    let assets: [(&str, &[u8]); 5] = [
        (
            "data/com.github.among-terminal-128.png",
            include_bytes!("../../../data/com.github.among-terminal-128.png"),
        ),
        (
            "data/com.github.among-terminal-256.png",
            include_bytes!("../../../data/com.github.among-terminal-256.png"),
        ),
        (
            "docs/media/logo-small.svg",
            include_bytes!("../../../docs/media/logo-small.svg"),
        ),
        (
            "docs/media/logo-small.png",
            include_bytes!("../../../docs/media/logo-small.png"),
        ),
        (
            "data/com.github.among-terminal.svg",
            include_bytes!("../../../data/com.github.among-terminal.svg"),
        ),
    ];
    for (name, bytes) in assets {
        let path = f.root().join(name);
        fs::create_dir_all(path.parent().ok_or("fixture path has no parent")?)?;
        fs::write(path, bytes)?;
    }
    let output = f.0.join("source.tar");
    snapshot::snapshot(&f.root(), &f.0.join("vendor"), &output)?;
    let mut archive = tar::Archive::new(fs::File::open(output)?);
    let mut found = 0;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if let Some((_, expected)) = assets
            .iter()
            .find(|(name, _)| path == std::path::Path::new(name))
        {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            assert_eq!(bytes, *expected);
            found += 1;
        }
    }
    assert_eq!(found, assets.len());
    Ok(())
}

#[test]
fn unsafe_missing_source_vendor_and_output_inputs_never_produce_a_new_archive() -> Result<()> {
    for failure in [
        "source-file",
        "directory-root",
        "nested-source",
        "vendor-root",
        "vendor-file",
        "missing",
        "no-checksum",
    ] {
        let f = Fixture::new()?;
        let root = f.root();
        let vendor = f.0.join("vendor");
        let output = f.0.join("output.tar");
        match failure {
            "source-file" => {
                fs::remove_file(root.join("LICENSE"))?;
                std::os::unix::fs::symlink("/bin/true", root.join("LICENSE"))?;
            }
            "directory-root" => std::os::unix::fs::symlink("/absent-fixture", root.join("src"))?,
            "nested-source" => {
                fs::create_dir(root.join("src"))?;
                std::os::unix::fs::symlink("/", root.join("src/foreign"))?;
            }
            "vendor-root" => {
                fs::rename(&vendor, f.0.join("saved-vendor"))?;
                std::os::unix::fs::symlink(f.0.join("saved-vendor"), &vendor)?;
            }
            "vendor-file" => {
                std::os::unix::fs::symlink("/bin/true", vendor.join("fixture/linked"))?
            }
            "missing" => fs::remove_file(root.join("LICENSE"))?,
            "no-checksum" => fs::remove_file(vendor.join("fixture/.cargo-checksum.json"))?,
            _ => return Err("unknown negative source fixture".into()),
        }
        assert!(
            snapshot::snapshot(&root, &vendor, &output).is_err(),
            "{failure}"
        );
        assert!(!output.exists());
    }
    let f = Fixture::new()?;
    let output = f.0.join("output.tar");
    let marker = f.0.join("user-file");
    fs::write(&marker, b"keep")?;
    std::os::unix::fs::symlink(&marker, &output)?;
    assert!(snapshot::snapshot(&f.root(), &f.0.join("vendor"), &output).is_err());
    assert_eq!(fs::read(marker)?, b"keep");
    Ok(())
}

#[test]
fn actual_snapshot_cli_records_real_archive_digest_without_overwriting_output() -> Result<()> {
    let f = Fixture::new()?;
    let archive = f.0.join("source.tar");
    let run = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
            .env_clear()
            .arg("source-snapshot")
            .arg(f.root())
            .arg(f.0.join("vendor"))
            .arg(&archive)
            .output()
    };
    let output = run()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt = storage::parse_json(&output.stdout)?;
    assert_eq!(
        receipt["archive_sha256"],
        storage::sha256(fs::File::open(&archive)?)?
    );
    let before = fs::read(&archive)?;
    let checked = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .arg("check-source-snapshot")
        .arg(&archive)
        .output()?;
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert!(!run()?.status.success());
    assert_eq!(fs::read(archive)?, before);
    Ok(())
}

/// Delivered sources must rebuild and retest the tools, so every allowlisted
/// directory, including the tools' sources, tests and container recipes,
/// reaches the archive.
#[test]
fn every_allowlisted_directory_including_the_tool_sources_reaches_the_archive() -> Result<()> {
    let f = Fixture::new()?;
    let root = f.root();
    for directory in snapshot::DIRECTORIES {
        fs::create_dir_all(root.join(directory))?;
        fs::write(root.join(directory).join("member"), directory.as_bytes())?;
    }
    fs::create_dir_all(root.join("tools/among-tools/examples"))?;
    let notice_fixture = include_bytes!("../examples/notice-plugin.rs");
    fs::write(
        root.join("tools/among-tools/examples/notice-plugin.rs"),
        notice_fixture,
    )?;
    let output = f.0.join("snapshot.tar");
    snapshot::snapshot(&root, &f.0.join("vendor"), &output)?;
    let hashes = storage::inspect_tar(&fs::read(&output)?)?;
    for directory in snapshot::DIRECTORIES {
        assert!(
            hashes.contains_key(&format!("{directory}/member")),
            "{directory} missing from the snapshot"
        );
    }
    for required in [
        "tools/among-tools/src/member",
        "tools/among-tools/tests/member",
        "tools/among-tools/containers/member",
        "tools/among-tools/examples/notice-plugin.rs",
        "examples/fake-plugin.rs",
    ] {
        assert!(hashes.contains_key(required), "{required}");
    }
    assert_eq!(
        hashes["tools/among-tools/examples/notice-plugin.rs"],
        storage::sha256(notice_fixture.as_slice())?
    );
    Ok(())
}

/// The snapshot check refuses an archive that lacks a mandatory input, the
/// vendored-source configuration or every vendored crate checksum.
#[test]
fn the_snapshot_check_refuses_archives_without_mandatory_inputs() -> Result<()> {
    let f = Fixture::new()?;
    let output = f.0.join("snapshot.tar");
    snapshot::snapshot(&f.root(), &f.0.join("vendor"), &output)?;
    let complete = storage::inspect_tar(&fs::read(&output)?)?;
    // The count covers every member except the inventory that lists them.
    assert_eq!(snapshot::check(&output)?, complete.len() - 1);
    let without = |prefix: &str| -> Result<Vec<u8>> {
        let mut archive = tar::Archive::new(fs::File::open(&output)?);
        let mut files = std::collections::BTreeMap::new();
        for entry in archive.entries()? {
            let mut entry = entry?;
            let name = entry.path()?.to_string_lossy().into_owned();
            if name != "source-inventory.json" && !name.starts_with(prefix) {
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes)?;
                files.insert(name, bytes);
            }
        }
        let mut inventory = std::collections::BTreeMap::new();
        for (name, bytes) in &files {
            inventory.insert(name.clone(), storage::sha256(bytes.as_slice())?);
        }
        files.insert(
            "source-inventory.json".into(),
            serde_json::to_vec(&serde_json::json!({"schema":1,"files":inventory}))?,
        );
        storage::deterministic_tar(&files)
    };
    for missing in ["Cargo.lock", ".cargo/config.toml", "vendor/"] {
        let path = f.0.join("incomplete.tar");
        fs::write(&path, without(missing)?)?;
        assert_eq!(
            snapshot::check(&path)
                .err()
                .map(|error| error.to_string())
                .as_deref(),
            Some("source snapshot is missing mandatory build/vendor inputs"),
            "{missing}"
        );
    }
    Ok(())
}

/// The part of a path the scanners below resolve, relative to the repository
/// root, with `.` and `..` folded away so `tools/among-tools/../..` names the
/// root.
fn normalized(path: &Path) -> PathBuf {
    let mut parts = Vec::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                parts.pop();
            }
            Component::CurDir => {}
            other => parts.push(other),
        }
    }
    parts.iter().collect()
}

fn in_snapshot(name: &str) -> bool {
    snapshot::FILES.contains(&name)
        || snapshot::DIRECTORIES
            .iter()
            .any(|directory| name == *directory || name.starts_with(&format!("{directory}/")))
}

/// The unquoted text of a plain string literal token. Escapes and raw strings
/// are refused so a path that cannot be read exactly never passes silently.
fn string_literal(literal: &proc_macro2::Literal) -> Option<String> {
    let text = literal.to_string();
    let inner = text.strip_prefix('"')?.strip_suffix('"')?;
    assert!(
        !inner.contains('\\'),
        "the source scan does not understand escapes in {text}"
    );
    Some(inner.to_string())
}

fn env_variable(tokens: &[TokenTree], at: usize) -> Option<String> {
    let (TokenTree::Ident(name), Some(TokenTree::Punct(bang)), Some(TokenTree::Group(group))) =
        (&tokens[at], tokens.get(at + 1), tokens.get(at + 2))
    else {
        return None;
    };
    if name != "env" || bang.as_char() != '!' {
        return None;
    }
    match group.stream().into_iter().next() {
        Some(TokenTree::Literal(literal)) => string_literal(&literal),
        _ => None,
    }
}

/// Every string literal inside a token stream, nested groups included, and
/// whether the stream names the crate directory through the environment.
fn literals(stream: TokenStream, found: &mut Vec<String>, manifest: &mut bool) {
    let tokens: Vec<TokenTree> = stream.into_iter().collect();
    let mut at = 0;
    while at < tokens.len() {
        if let Some(variable) = env_variable(&tokens, at) {
            assert_eq!(
                variable, "CARGO_MANIFEST_DIR",
                "the source scan only understands the crate directory variable"
            );
            *manifest = true;
            at += 3;
            continue;
        }
        match &tokens[at] {
            TokenTree::Literal(literal) => {
                if let Some(text) = string_literal(literal) {
                    found.push(text);
                }
            }
            TokenTree::Group(group) => literals(group.stream(), found, manifest),
            _ => {}
        }
        at += 1;
    }
}

struct Reference {
    file: PathBuf,
    target: PathBuf,
    kind: &'static str,
}

/// Finds, in one token stream, every file the code embeds at compile time
/// (`include_bytes!`, `include_str!`, `include!`, also inside other macros
/// and in several lines or `concat!` forms) and, best effort, every
/// `env!("CARGO_MANIFEST_DIR")` path extended with literal `.join(..)` calls,
/// which tests use to read repository files at run time. Returns the path an
/// unfinished manifest expression ends with, so `Path::new(env!(..)).join(..)`
/// is followed across the group boundary.
fn scan(
    stream: TokenStream,
    file: &Path,
    manifest: &Path,
    found: &mut Vec<Reference>,
) -> Option<PathBuf> {
    let tokens: Vec<TokenTree> = stream.into_iter().collect();
    let mut current: Option<PathBuf> = None;
    let mut at = 0;
    while at < tokens.len() {
        if env_variable(&tokens, at).as_deref() == Some("CARGO_MANIFEST_DIR") {
            current = Some(manifest.to_path_buf());
            at += 3;
            continue;
        }
        match &tokens[at] {
            TokenTree::Ident(name)
                if matches!(
                    name.to_string().as_str(),
                    "include_bytes" | "include_str" | "include"
                ) && matches!(tokens.get(at + 1), Some(TokenTree::Punct(p)) if p.as_char() == '!') =>
            {
                if let Some(TokenTree::Group(group)) = tokens.get(at + 2) {
                    let mut text = Vec::new();
                    let mut from_manifest = false;
                    literals(group.stream(), &mut text, &mut from_manifest);
                    let joined = text.concat();
                    let base = if from_manifest {
                        manifest.to_path_buf()
                    } else {
                        file.parent().unwrap_or(manifest).to_path_buf()
                    };
                    assert!(
                        !joined.is_empty(),
                        "{} has an embed the scan cannot resolve",
                        file.display()
                    );
                    found.push(Reference {
                        file: file.to_path_buf(),
                        target: normalized(&base.join(joined.trim_start_matches('/'))),
                        kind: "embeds",
                    });
                    at += 3;
                    continue;
                }
            }
            // A path belongs to one expression: neither the end of a statement
            // nor the next call argument may continue it.
            TokenTree::Punct(p) if matches!(p.as_char(), ';' | ',') => current = None,
            TokenTree::Punct(p) if p.as_char() == '.' => {
                // `.join("literal")` extends a manifest-based path.
                if let (Some(base), Some(TokenTree::Ident(method)), Some(TokenTree::Group(group))) =
                    (&current, tokens.get(at + 1), tokens.get(at + 2))
                {
                    if method == "join" {
                        let mut text = Vec::new();
                        let mut unused = false;
                        literals(group.stream(), &mut text, &mut unused);
                        current = match text.as_slice() {
                            [one] if !unused => {
                                let next = normalized(&base.join(one));
                                // Only the last join of a chain names the file
                                // that is read; the steps before it are directories.
                                let chained = matches!(tokens.get(at + 3), Some(TokenTree::Punct(p)) if p.as_char() == '.')
                                    && matches!(tokens.get(at + 4), Some(TokenTree::Ident(m)) if m == "join");
                                if !chained {
                                    found.push(Reference {
                                        file: file.to_path_buf(),
                                        target: next.clone(),
                                        kind: "reads",
                                    });
                                }
                                Some(next)
                            }
                            _ => None,
                        };
                        at += 3;
                        continue;
                    }
                }
            }
            TokenTree::Group(group) => {
                if let Some(inner) = scan(group.stream(), file, manifest, found) {
                    current = Some(inner);
                }
            }
            _ => {}
        }
        at += 1;
    }
    current
}

fn rust_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            rust_files(&path, files)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            files.push(path);
        }
    }
    Ok(())
}

/// Scans the whole code of both crates: sources, integration tests and
/// examples. The root crate runs under `cargo test` in the Debian and RPM
/// builds from the snapshot, the tools crate runs there too.
fn references() -> Result<(PathBuf, Vec<Reference>)> {
    let root = fs::canonicalize(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))?;
    let mut found = Vec::new();
    for (manifest, directories) in [
        (root.clone(), ["src", "tests", "examples"]),
        (root.join("tools/among-tools"), ["src", "tests", "examples"]),
    ] {
        for directory in directories {
            let mut files = Vec::new();
            rust_files(&manifest.join(directory), &mut files)?;
            for file in files {
                let stream: TokenStream = fs::read_to_string(&file)?
                    .parse()
                    .map_err(|error| format!("{} does not tokenize: {error}", file.display()))?;
                scan(stream, &file, &manifest, &mut found);
            }
        }
    }
    Ok((root, found))
}

fn outside_snapshot(root: &Path, found: &[Reference], kind: &str) -> Vec<String> {
    found
        .iter()
        .filter(|reference| reference.kind == kind)
        .filter_map(|reference| {
            let name = reference
                .target
                .strip_prefix(root)
                .ok()?
                .to_string_lossy()
                .into_owned();
            // Build output is produced by the build itself, not shipped.
            let generated = name == "target"
                || name.starts_with("target/")
                || name.starts_with("tools/among-tools/target");
            (!generated && !name.is_empty() && !in_snapshot(&name)).then(|| {
                format!(
                    "{} {kind} {name}, which the source snapshot does not carry",
                    reference.file.display()
                )
            })
        })
        .collect()
}

/// Packages are built and tested from the source snapshot, which carries only
/// an allowlist. Source that embeds a file outside that allowlist compiles in
/// a checkout and fails in every package build, so each embedded file of
/// either crate, in any `include_*` form and any layout, must be a snapshot
/// member.
#[test]
fn every_file_embedded_by_the_sources_reaches_the_snapshot() -> Result<()> {
    let (root, found) = references()?;
    let embedded: Vec<&Reference> = found.iter().filter(|r| r.kind == "embeds").collect();
    assert!(
        embedded.len() > 3,
        "too few embedded files found ({}): the scan is broken",
        embedded.len()
    );
    // Include macros also appear inside tests with the target on another line;
    // the managed manifest example is embedded that way.
    assert!(
        embedded
            .iter()
            .any(|r| r.target.ends_with("examples/managed-manifest.json")),
        "the scan no longer sees the managed manifest embed"
    );
    let missing = outside_snapshot(&root, &found, "embeds");
    assert!(missing.is_empty(), "{}", missing.join("\n"));
    Ok(())
}

/// Best effort for run-time reads: a test that joins a literal path onto the
/// crate directory (`env!("CARGO_MANIFEST_DIR")`, possibly through `..`) must
/// read a snapshot member, or a package build cannot run that test. Reads
/// through a path kept in a variable are not followed; the build itself
/// catches those.
#[test]
fn no_test_reads_a_repository_file_the_snapshot_does_not_carry() -> Result<()> {
    let (root, found) = references()?;
    assert!(
        found.iter().filter(|r| r.kind == "reads").count() > 10,
        "too few run-time reads found: the scan is broken"
    );
    let missing = outside_snapshot(&root, &found, "reads");
    assert!(missing.is_empty(), "{}", missing.join("\n"));
    Ok(())
}
