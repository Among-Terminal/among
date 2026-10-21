use among_tools::{notices, storage, Result};
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn fixture() -> Result<Fixture> {
    let path = storage::private_directory(&std::env::temp_dir(), "among-notices-fixture")?;
    fs::create_dir(path.join("crate"))?;
    fs::write(path.join("crate/Cargo.toml"), b"synthetic source")?;
    fs::write(path.join("crate/LICENSE-MIT"), b"verbatim license\n\n")?;
    fs::write(path.join("crate/NOTICE"), b"original notice")?;
    Ok(Fixture(path))
}
fn graph(f: &Fixture) -> Value {
    json!({"packages":[{"id":"path+fixture","source":null,"license":"Apache-2.0"},{"id":"registry+fixture#crate@1.0.0","name":"fixture-crate","version":"1.0.0","source":"registry+https://github.com/rust-lang/crates.io-index","manifest_path":f.0.join("crate/Cargo.toml"),"license":"MIT","license_file":null}]})
}

#[test]
fn verbatim_license_and_notice_bytes_deduplicate_across_all_three_graphs() -> Result<()> {
    let f = fixture()?;
    let graphs = notices::MANIFESTS.map(|manifest| (manifest, graph(&f)));
    let (outputs, count) = notices::inventory(&graphs, &f.0)?;
    assert_eq!(count, 1);
    assert_eq!(
        outputs["texts/fixture-crate-1.0.0/LICENSE-MIT"],
        b"verbatim license\n\n"
    );
    assert_eq!(
        outputs["texts/fixture-crate-1.0.0/NOTICE"],
        b"original notice"
    );
    let inventory = storage::parse_json(&outputs["inventory.json"])?;
    assert_eq!(inventory["schema"], 1);
    assert_eq!(
        inventory["packages"][0]["trees"],
        json!([
            "Cargo.toml",
            "examples/plugins/Cargo.toml",
            "tools/among-tools/Cargo.toml"
        ])
    );
    assert_eq!(
        inventory["packages"][0]["texts"][0]["sha256"],
        storage::sha256(b"verbatim license\n\n".as_slice())?
    );
    assert_eq!(outputs, notices::inventory(&graphs, &f.0)?.0);
    Ok(())
}

#[test]
fn missing_license_unsafe_evidence_and_conflicting_graphs_fail() -> Result<()> {
    let f = fixture()?;
    let valid = graph(&f);
    for changed in [
        json!({"packages":[]}),
        json!({"packages":[{"source":null,"license":"MIT"}]}),
    ] {
        if changed["packages"].as_array().is_some_and(Vec::is_empty) {
            assert_eq!(notices::inventory(&[("Cargo.toml", changed)], &f.0)?.1, 0);
        } else {
            assert!(notices::inventory(&[("Cargo.toml", changed)], &f.0).is_err());
        }
    }
    for (field, value) in [
        ("license", Value::Null),
        ("name", json!("../escape")),
        ("license_file", json!("../foreign")),
    ] {
        let mut changed = valid.clone();
        changed["packages"][1][field] = value;
        assert!(notices::inventory(&[("Cargo.toml", changed)], &f.0).is_err());
    }
    let mut changed = valid.clone();
    changed["packages"][1]["license"] = "Apache-2.0".into();
    assert!(notices::inventory(
        &[
            ("Cargo.toml", valid.clone()),
            ("tools/among-tools/Cargo.toml", changed)
        ],
        &f.0
    )
    .is_err());
    fs::remove_file(f.0.join("crate/LICENSE-MIT"))?;
    fs::remove_file(f.0.join("crate/NOTICE"))?;
    assert!(notices::inventory(&[("Cargo.toml", valid.clone())], &f.0).is_err());
    fs::write(f.0.join("foreign"), b"do not read")?;
    std::os::unix::fs::symlink(f.0.join("foreign"), f.0.join("crate/LICENSE"))?;
    assert!(notices::inventory(&[("Cargo.toml", valid)], &f.0).is_err());
    Ok(())
}

#[test]
fn actual_cli_uses_three_locked_metadata_calls_separates_stderr_and_detects_stale_bytes(
) -> Result<()> {
    let f = fixture()?;
    let graph = f.0.join("graph.json");
    fs::write(&graph, serde_json::to_vec(&self::graph(&f))?)?;
    fs::create_dir(f.0.join("bin"))?;
    fs::create_dir(f.0.join("output"))?;
    let cargo = f.0.join("bin/cargo");
    fs::write(&cargo,format!("#!/bin/sh\n[ \"$1 $2 $3 $4 $5 $6\" = 'metadata --locked --offline --format-version 1 --manifest-path' ] || exit 12\nprintf '%s\\n' \"$7\" >> '{}'\nprintf 'synthetic diagnostic\\n' >&2\nexec /bin/cat '{}'\n",f.0.join("calls").display(),graph.display()))?;
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755))?;
    let run = |mode: &str| {
        std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
            .env_clear()
            .env("PATH", f.0.join("bin"))
            .env("AMONG_CHECK_ROOT", f.0.join("checks"))
            .arg("third-party-notices")
            .arg(&f.0)
            .arg(f.0.join("output"))
            .arg(mode)
            .output()
    };
    let result = run("generate")?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        fs::read_to_string(f.0.join("calls"))?
            .lines()
            .collect::<Vec<_>>(),
        notices::MANIFESTS
    );
    assert!(run("check")?.status.success());
    fs::write(
        f.0.join("output/texts/fixture-crate-1.0.0/NOTICE"),
        b"modified",
    )?;
    assert!(!run("check")?.status.success());
    Ok(())
}

fn supplemental_fixture() -> Result<(Fixture, Value, Value)> {
    let f = fixture()?;
    fs::remove_file(f.0.join("crate/LICENSE-MIT"))?;
    fs::remove_file(f.0.join("crate/NOTICE"))?;
    let manifest = b"[package]\nname='fixture-crate'\nversion='1.0.0'\nlicense='MIT'\n";
    let vcs =
        br#"{"git":{"sha1":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"path_in_vcs":"impl"}"#;
    let archive_hash = storage::sha256(b"published fixture archive".as_slice())?;
    fs::write(f.0.join("crate/Cargo.toml.orig"), manifest)?;
    fs::write(f.0.join("crate/.cargo_vcs_info.json"), vcs)?;
    fs::write(
        f.0.join("crate/.cargo-checksum.json"),
        serde_json::to_vec(&json!({"package":archive_hash,"files":{}}))?,
    )?;
    let root = f.0.join("THIRD_PARTY/inputs");
    fs::create_dir_all(root.join("fixture"))?;
    fs::write(root.join("fixture/package.toml"), manifest)?;
    fs::write(
        root.join("fixture/LICENSE-MIT"),
        b"upstream grant, verbatim\n",
    )?;
    let mut graph = graph(&f);
    graph["packages"][1]["repository"] = "https://github.com/example/fixture".into();
    for path in notices::MANIFESTS {
        let lock = f.0.join(path).with_file_name("Cargo.lock");
        fs::create_dir_all(lock.parent().ok_or("lock parent absent")?)?;
        fs::write(lock,format!("version=4\n[[package]]\nname='fixture-crate'\nversion='1.0.0'\nsource='registry+https://github.com/rust-lang/crates.io-index'\nchecksum='{archive_hash}'\n"))?;
    }
    let index = json!({"schema":1,"packages":[{
        "name":"fixture-crate","version":"1.0.0",
        "source":"registry+https://github.com/rust-lang/crates.io-index",
        "license":"MIT","repository":"https://github.com/example/fixture",
        "upstream":"https://github.com/example/fixture","archive_sha256":archive_hash,
        "vcs_commit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","vcs_path":"impl",
        "vcs_sha256":storage::sha256(vcs.as_slice())?,"package_manifest":"fixture/package.toml",
        "files":[
            {"path":"fixture/package.toml","upstream_path":"impl/Cargo.toml","kind":"source","sha256":storage::sha256(manifest.as_slice())?},
            {"path":"fixture/LICENSE-MIT","upstream_path":"LICENSE-MIT","kind":"license","sha256":storage::sha256(b"upstream grant, verbatim\n".as_slice())?}
        ]
    }]});
    fs::write(root.join("index.json"), serde_json::to_vec(&index)?)?;
    Ok((f, graph, index))
}

#[test]
fn supplemental_inputs_drive_verbatim_output_and_record_immutable_provenance() -> Result<()> {
    let (f, graph, _) = supplemental_fixture()?;
    let graphs = notices::MANIFESTS.map(|manifest| (manifest, graph.clone()));
    let (outputs, count) = notices::inventory(&graphs, &f.0)?;
    assert_eq!(count, 1);
    assert_eq!(
        outputs["texts/fixture-crate-1.0.0/LICENSE-MIT"],
        b"upstream grant, verbatim\n"
    );
    assert_eq!(
        outputs["provenance/fixture-crate-1.0.0/impl/Cargo.toml"],
        fs::read(f.0.join("crate/Cargo.toml.orig"))?
    );
    let inventory = storage::parse_json(&outputs["inventory.json"])?;
    assert_eq!(inventory["packages"][0]["license"], "MIT");
    assert_eq!(inventory["packages"][0]["supplemental"]["evidence"][0]["url"], "https://github.com/example/fixture/blob/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/impl/Cargo.toml");
    assert_eq!(outputs, notices::inventory(&graphs, &f.0)?.0);
    Ok(())
}

#[test]
fn supplemental_identity_tampering_and_missing_primary_inputs_fail_closed() -> Result<()> {
    for (field, value) in [
        ("name", json!("another-crate")),
        ("version", json!("2.0.0")),
        ("source", json!("registry+https://example.test/index")),
        ("license", json!("Apache-2.0")),
        ("repository", json!("https://github.com/another/fixture")),
        ("archive_sha256", json!("0".repeat(64))),
        ("vcs_commit", json!("b".repeat(40))),
        ("vcs_path", json!("elsewhere")),
        ("vcs_sha256", json!("0".repeat(64))),
        ("package_manifest", json!("absent")),
    ] {
        let (f, graph, mut index) = supplemental_fixture()?;
        index["packages"][0][field] = value;
        fs::write(
            f.0.join("THIRD_PARTY/inputs/index.json"),
            serde_json::to_vec(&index)?,
        )?;
        assert!(
            notices::inventory(&[("Cargo.toml", graph)], &f.0).is_err(),
            "{field}"
        );
    }
    for path in [
        "crate/.cargo_vcs_info.json",
        "crate/Cargo.toml.orig",
        "crate/.cargo-checksum.json",
        "THIRD_PARTY/inputs/fixture/LICENSE-MIT",
        "THIRD_PARTY/inputs/fixture/package.toml",
        "THIRD_PARTY/inputs/index.json",
        "Cargo.lock",
    ] {
        let (f, graph, _) = supplemental_fixture()?;
        fs::write(f.0.join(path), b"tampered")?;
        assert!(
            notices::inventory(&[("Cargo.toml", graph)], &f.0).is_err(),
            "{path}"
        );
    }
    for path in [
        "crate/.cargo_vcs_info.json",
        "crate/Cargo.toml.orig",
        "THIRD_PARTY/inputs/fixture/LICENSE-MIT",
        "THIRD_PARTY/inputs/index.json",
    ] {
        let (f, graph, _) = supplemental_fixture()?;
        fs::remove_file(f.0.join(path))?;
        assert!(
            notices::inventory(&[("Cargo.toml", graph)], &f.0).is_err(),
            "{path}"
        );
    }
    Ok(())
}

#[test]
fn supplemental_unsafe_paths_symlinks_nonregular_files_and_duplicate_receipts_are_refused(
) -> Result<()> {
    for field in ["path", "upstream_path"] {
        for path in [
            "../foreign",
            "/foreign",
            "fixture/./LICENSE-MIT",
            "fixture//LICENSE-MIT",
        ] {
            let (f, graph, mut index) = supplemental_fixture()?;
            index["packages"][0]["files"][1][field] = path.into();
            fs::write(
                f.0.join("THIRD_PARTY/inputs/index.json"),
                serde_json::to_vec(&index)?,
            )?;
            assert!(
                notices::inventory(&[("Cargo.toml", graph)], &f.0).is_err(),
                "{field}: {path}"
            );
        }
    }
    for kind in ["file-symlink", "directory-symlink", "fifo", "empty"] {
        let (f, graph, _) = supplemental_fixture()?;
        let root = f.0.join("THIRD_PARTY/inputs");
        if kind == "directory-symlink" {
            fs::rename(root.join("fixture"), f.0.join("foreign"))?;
            std::os::unix::fs::symlink(f.0.join("foreign"), root.join("fixture"))?;
        } else {
            let file = root.join("fixture/LICENSE-MIT");
            fs::remove_file(&file)?;
            match kind {
                "file-symlink" => {
                    fs::write(f.0.join("foreign"), b"upstream grant, verbatim\n")?;
                    std::os::unix::fs::symlink(f.0.join("foreign"), file)?;
                }
                "fifo" => nix::unistd::mkfifo(&file, nix::sys::stat::Mode::S_IRUSR)?,
                _ => fs::write(file, b"")?,
            }
        }
        assert!(
            notices::inventory(&[("Cargo.toml", graph)], &f.0).is_err(),
            "{kind}"
        );
    }
    let (f, graph, mut index) = supplemental_fixture()?;
    let duplicate = index["packages"][0].clone();
    index["packages"] = json!([duplicate.clone(), duplicate]);
    fs::write(
        f.0.join("THIRD_PARTY/inputs/index.json"),
        serde_json::to_vec(&index)?,
    )?;
    assert!(notices::inventory(&[("Cargo.toml", graph)], &f.0).is_err());
    Ok(())
}

#[test]
fn registry_archive_identity_is_checked_without_mutating_the_registry() -> Result<()> {
    let (f, mut graph, _) = supplemental_fixture()?;
    let source = f.0.join("registry/src/test-index/fixture-crate-1.0.0");
    fs::create_dir_all(&source)?;
    for entry in fs::read_dir(f.0.join("crate"))? {
        let entry = entry?;
        if entry.file_name() != ".cargo-checksum.json" {
            fs::copy(entry.path(), source.join(entry.file_name()))?;
        }
    }
    graph["packages"][1]["manifest_path"] = json!(source.join("Cargo.toml"));
    let archive =
        f.0.join("registry/cache/test-index/fixture-crate-1.0.0.crate");
    fs::create_dir_all(archive.parent().ok_or("archive parent absent")?)?;
    fs::write(&archive, b"published fixture archive")?;
    assert_eq!(
        notices::inventory(&[("Cargo.toml", graph.clone())], &f.0)?.1,
        1
    );
    fs::write(&archive, b"tampered archive")?;
    let error = notices::inventory(&[("Cargo.toml", graph.clone())], &f.0)
        .err()
        .ok_or("changed archive accepted")?;
    assert!(error.to_string().contains("archive checksum mismatch"));
    fs::remove_file(archive)?;
    assert!(notices::inventory(&[("Cargo.toml", graph)], &f.0).is_err());
    Ok(())
}

#[test]
fn actual_cli_generates_and_checks_supplemental_bytes_before_any_publication() -> Result<()> {
    let (f, graph, _) = supplemental_fixture()?;
    let graph_path = f.0.join("graph.json");
    fs::write(&graph_path, serde_json::to_vec(&graph)?)?;
    fs::create_dir(f.0.join("bin"))?;
    fs::create_dir(f.0.join("output"))?;
    let cargo = f.0.join("bin/cargo");
    fs::write(&cargo, format!("#!/bin/sh\n[ \"$1 $2 $3\" = 'metadata --locked --offline' ] || exit 12\nexec /bin/cat '{}'\n",graph_path.display()))?;
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755))?;
    let run = |mode: &str| {
        std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
            .env_clear()
            .env("PATH", f.0.join("bin"))
            .env("AMONG_CHECK_ROOT", f.0.join("checks"))
            .args(["third-party-notices"])
            .arg(&f.0)
            .arg(f.0.join("output"))
            .arg(mode)
            .output()
    };
    let generated = run("generate")?;
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    assert!(run("check")?.status.success());
    let license = f.0.join("output/texts/fixture-crate-1.0.0/LICENSE-MIT");
    assert_eq!(fs::read(&license)?, b"upstream grant, verbatim\n");
    let inventory = fs::read(f.0.join("output/inventory.json"))?;
    fs::write(&license, b"changed output")?;
    assert!(!run("check")?.status.success());
    fs::write(
        f.0.join("THIRD_PARTY/inputs/fixture/LICENSE-MIT"),
        b"changed input",
    )?;
    assert!(!run("generate")?.status.success());
    assert_eq!(fs::read(license)?, b"changed output");
    assert_eq!(fs::read(f.0.join("output/inventory.json"))?, inventory);
    Ok(())
}

#[test]
fn lock_conflicts_and_missing_grants_cannot_be_replaced_by_source_declarations() -> Result<()> {
    let (f, graph, mut index) = supplemental_fixture()?;
    let lock = f.0.join("examples/plugins/Cargo.lock");
    let original = fs::read_to_string(&lock)?;
    fs::write(
        &lock,
        original.replace(
            index["packages"][0]["archive_sha256"]
                .as_str()
                .ok_or("hash absent")?,
            &"0".repeat(64),
        ),
    )?;
    let error = notices::inventory(&[("Cargo.toml", graph.clone())], &f.0)
        .err()
        .ok_or("conflicting lock accepted")?;
    assert!(error
        .to_string()
        .contains("conflicting locked archive checksums"));
    fs::write(lock, original)?;
    index["packages"][0]["files"][1]["kind"] = "source".into();
    fs::write(
        f.0.join("THIRD_PARTY/inputs/index.json"),
        serde_json::to_vec(&index)?,
    )?;
    let error = notices::inventory(&[("Cargo.toml", graph)], &f.0)
        .err()
        .ok_or("source declaration accepted as grant")?;
    assert!(error
        .to_string()
        .contains("exact package source and license grant"));
    Ok(())
}

#[test]
fn existing_source_snapshot_retains_every_checked_in_supplemental_input() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let inputs = among_tools::source_snapshot::inputs(&root, &[])?;
    let index = storage::parse_json(&inputs["THIRD_PARTY/inputs/index.json"].bytes)?;
    for package in index["packages"].as_array().ok_or("packages absent")? {
        for file in package["files"].as_array().ok_or("files absent")? {
            let path = format!(
                "THIRD_PARTY/inputs/{}",
                file["path"].as_str().ok_or("file path absent")?
            );
            assert_eq!(
                storage::sha256(inputs[&path].bytes.as_slice())?,
                file["sha256"].as_str().ok_or("hash absent")?
            );
        }
    }
    Ok(())
}
