use among_tools::Result;
use serde_json::{json, Value};
use std::{collections::BTreeSet, fs, path::Path};

#[test]
fn main_snap_workflow_uses_native_rust_drivers_without_publication() -> Result<()> {
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
    for forbidden in ["snapcraft upload", "gh release create", "gh release upload"] {
        assert!(
            !scripts.contains(forbidden),
            "forbidden Snap build/runtime caller: {forbidden}"
        );
    }
    for required in ["package-version .","SNAP_VERSION=","Internal tooling leaked","cargo build --locked --release --manifest-path tools/among-tools/Cargo.toml --bin among-tools","cargo build --locked --release --example check-clipboard","check-snap-runtime --github-hosted","check-context-menu-snap \"$PWD/target/release/examples/check-clipboard\" light --github-hosted","check-context-menu-snap \"$PWD/target/release/examples/check-clipboard\" dark --github-hosted","check-live-focus /snap/bin/among","check-live-status \"$PWD\" /snap/bin/among \"$PWD/package-plugin-profile\"","check-sessions /snap/bin/among"] {assert!(scripts.contains(required),"required tested native caller absent: {required}");}
    // This checks caller wiring, not live execution or compiler-free acceptance.
    // Runtime drivers were separately exercised on both installed architectures.
    Ok(())
}

fn snap_workflow() -> Result<Value> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bytes = fs::read(root.join(".github/workflows/snap-build.yml"))?;
    Ok(serde_yaml_ng::from_slice(&bytes)?)
}

fn job_scripts(job: &Value) -> Result<String> {
    Ok(job["steps"]
        .as_array()
        .ok_or("steps absent")?
        .iter()
        .filter_map(|step| step["run"].as_str())
        .collect::<Vec<_>>()
        .join("\n"))
}

/// The failure-evidence upload tolerates an empty match, so a check root that
/// moved while the upload path stayed behind would silently retain nothing.
#[test]
fn failure_evidence_upload_follows_the_check_root_the_job_exports() -> Result<()> {
    let workflow = snap_workflow()?;
    let job = &workflow["jobs"]["snap"];
    assert!(job_scripts(job)?
        .contains("echo \"AMONG_CHECK_ROOT=$RUNNER_TEMP/among-checks\" >> \"$GITHUB_ENV\""));
    let upload = job["steps"]
        .as_array()
        .ok_or("steps absent")?
        .iter()
        .find(|step| step["name"] == "Retain display check evidence after a failure")
        .ok_or("failure-evidence upload absent")?;
    assert_eq!(upload["with"]["path"], "${{ env.AMONG_CHECK_ROOT }}/");
    Ok(())
}

/// A detached systemd container accepts `docker exec` before its init has
/// opened the private socket systemctl talks to; a systemctl call in that
/// window fails with "Failed to connect to bus". The wait must come first.
#[test]
fn clean_runtime_waits_for_systemd_before_the_first_systemctl() -> Result<()> {
    let workflow = snap_workflow()?;
    let scripts = job_scripts(&workflow["jobs"]["clean-runtime"])?;
    let wait = scripts
        .find("until docker exec among-clean-runtime test -S /run/systemd/private")
        .ok_or("clean runtime never waits for systemd's private socket")?;
    let first = scripts
        .find("docker exec among-clean-runtime systemctl")
        .ok_or("clean runtime no longer calls systemctl")?;
    assert!(wait < first);
    Ok(())
}

/// The clean runtime writes evidence to a bind-mounted directory: the mount
/// target, the checker environment, the image's mount point and the script
/// must all name the same container path, or evidence is lost or refused.
#[test]
fn clean_runtime_mount_environment_image_and_script_agree_on_the_check_root() -> Result<()> {
    let container = "/var/lib/among-checks";
    let workflow = snap_workflow()?;
    let scripts = job_scripts(&workflow["jobs"]["clean-runtime"])?;
    assert!(scripts.contains(&format!(
        "--mount \"type=bind,source=$PWD/runtime-evidence,target={container}\""
    )));
    assert!(scripts.contains(&format!("AMONG_CHECK_ROOT={container} \\")));
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/ci/snap-runtime");
    let dockerfile = fs::read_to_string(root.join("Dockerfile"))?;
    assert!(dockerfile.contains(&format!("mkdir -p {container}")));
    let script = fs::read_to_string(root.join("check-installed.sh"))?;
    assert!(script.contains("checks=\"${AMONG_CHECK_ROOT:?"));
    // Every evidence write goes through the variable, never a literal path.
    assert!(!script.contains(container));
    Ok(())
}
