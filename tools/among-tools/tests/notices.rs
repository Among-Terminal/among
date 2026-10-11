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
    let (outputs, count) = notices::inventory(&graphs)?;
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
    assert_eq!(outputs, notices::inventory(&graphs)?.0);
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
            assert_eq!(notices::inventory(&[("Cargo.toml", changed)])?.1, 0);
        } else {
            assert!(notices::inventory(&[("Cargo.toml", changed)]).is_err());
        }
    }
    for (field, value) in [
        ("license", Value::Null),
        ("name", json!("../escape")),
        ("license_file", json!("../foreign")),
    ] {
        let mut changed = valid.clone();
        changed["packages"][1][field] = value;
        assert!(notices::inventory(&[("Cargo.toml", changed)]).is_err());
    }
    let mut changed = valid.clone();
    changed["packages"][1]["license"] = "Apache-2.0".into();
    assert!(notices::inventory(&[
        ("Cargo.toml", valid.clone()),
        ("tools/among-tools/Cargo.toml", changed)
    ])
    .is_err());
    fs::remove_file(f.0.join("crate/LICENSE-MIT"))?;
    fs::remove_file(f.0.join("crate/NOTICE"))?;
    assert!(notices::inventory(&[("Cargo.toml", valid.clone())]).is_err());
    fs::write(f.0.join("foreign"), b"do not read")?;
    std::os::unix::fs::symlink(f.0.join("foreign"), f.0.join("crate/LICENSE"))?;
    assert!(notices::inventory(&[("Cargo.toml", valid)]).is_err());
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
    fs::write(&cargo,format!("#!/bin/sh\n[ \"$1 $2 $3 $4 $5\" = 'metadata --locked --format-version 1 --manifest-path' ] || exit 12\nprintf '%s\\n' \"$6\" >> '{}'\nprintf 'synthetic diagnostic\\n' >&2\nexec /bin/cat '{}'\n",f.0.join("calls").display(),graph.display()))?;
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755))?;
    let run = |mode: &str| {
        std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
            .env_clear()
            .env("PATH", f.0.join("bin"))
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
