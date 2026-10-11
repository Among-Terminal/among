use among_tools::Result;
use serde_json::{json, Value};
use std::{collections::BTreeSet, fs, path::Path};

#[test]
fn main_snap_workflow_uses_native_rust_drivers_without_direct_python_or_publication() -> Result<()>
{
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bytes = fs::read(root.join(".github/workflows/snap-build.yml"))?;
    let workflow: Value = serde_yaml_ng::from_slice(&bytes)?;
    assert_eq!(workflow["permissions"], json!({"contents":"read"}));
    let job = &workflow["jobs"]["snap"];
    let rows = job["strategy"]["matrix"]["include"]
        .as_array()
        .ok_or("native Snap matrix absent")?;
    assert_eq!(rows.len(), 2);
    let actual = rows
        .iter()
        .map(|row| {
            Ok((
                row["arch"].as_str().ok_or("architecture absent")?,
                row["runner"].as_str().ok_or("runner absent")?,
            ))
        })
        .collect::<Result<BTreeSet<_>>>()?;
    assert_eq!(
        actual,
        BTreeSet::from([("amd64", "ubuntu-24.04"), ("arm64", "ubuntu-24.04-arm")])
    );
    let steps = job["steps"].as_array().ok_or("Snap steps absent")?;
    let mut scripts = String::new();
    for step in steps {
        if let Some(action) = step["uses"].as_str() {
            let (_, sha) = action.split_once('@').ok_or("action pin absent")?;
            assert_eq!(
                sha.split_whitespace()
                    .next()
                    .ok_or("action SHA absent")?
                    .len(),
                40
            );
        }
        if let Some(script) = step["run"].as_str() {
            scripts.push_str(script);
            scripts.push('\n');
        }
    }
    for forbidden in [
        "python3 ",
        "python3-gi",
        "python3-yaml",
        "scripts/ci-x11.py",
        "snapcraft upload",
        "gh release create",
        "gh release upload",
    ] {
        assert!(
            !scripts.contains(forbidden),
            "forbidden Snap build/runtime caller: {forbidden}"
        );
    }
    for required in ["package-version .","SNAP_VERSION=","Internal tooling leaked","cargo build --locked --release --manifest-path tools/among-tools/Cargo.toml --bin among-tools","cargo build --locked --release --example check-clipboard","check-snap-runtime --github-hosted","check-context-menu-snap \"$PWD/target/release/examples/check-clipboard\" light --github-hosted","check-context-menu-snap \"$PWD/target/release/examples/check-clipboard\" dark --github-hosted","check-live-focus /snap/bin/among","check-live-status \"$PWD\" /snap/bin/among /snap/among/current/usr/share/among/plugins","check-sessions /snap/bin/among"] {assert!(scripts.contains(required),"required tested native caller absent: {required}");}
    // This checks caller wiring, not live execution or compiler-free acceptance.
    // Runtime drivers were separately exercised on both installed architectures.
    Ok(())
}
