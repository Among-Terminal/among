//! The AppImage workflow is called by the tag release, so its trigger set and
//! artifact names are a contract. It must also stay read-only: building a
//! candidate must never be able to change a GitHub Release.

use among_tools::Result;
use serde_json::{json, Value};
use std::{collections::BTreeSet, fs, path::Path};

fn workflow() -> Result<Value> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bytes = fs::read(root.join(".github/workflows/appimage-release.yml"))?;
    Ok(serde_yaml_ng::from_slice(&bytes)?)
}

#[test]
fn appimage_workflow_is_callable_read_only_and_keeps_the_artifact_contract() -> Result<()> {
    let workflow = workflow()?;
    assert_eq!(workflow["permissions"], json!({"contents":"read"}));
    // YAML 1.1 readers may turn the `on` key into a boolean; accept either.
    let triggers = workflow
        .get("on")
        .or_else(|| workflow.get("true"))
        .and_then(Value::as_object)
        .ok_or("workflow triggers absent")?
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        triggers,
        BTreeSet::from([
            "pull_request".to_string(),
            "workflow_call".into(),
            "workflow_dispatch".into()
        ])
    );
    assert!(
        workflow.get("concurrency").is_none(),
        "a called workflow must not share a concurrency group with its caller"
    );
    let jobs = workflow["jobs"].as_object().ok_or("jobs absent")?;
    let mut scripts = String::new();
    let mut artifacts = BTreeSet::new();
    for job in jobs.values() {
        for step in job["steps"].as_array().ok_or("steps absent")? {
            if let Some(action) = step["uses"].as_str() {
                let (_, sha) = action.split_once('@').ok_or("action pin absent")?;
                assert!(
                    sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()),
                    "action not pinned to a commit: {action}"
                );
                if action.starts_with("actions/upload-artifact@") {
                    artifacts.insert(
                        step["with"]["name"]
                            .as_str()
                            .ok_or("artifact name absent")?
                            .to_string(),
                    );
                }
            }
            if let Some(script) = step["run"].as_str() {
                scripts.push_str(script);
                scripts.push('\n');
            }
        }
    }
    for name in [
        "appimage-${{ matrix.architecture }}",
        "appimage-SHA256SUMS",
        "appimage-sources-${{ matrix.architecture }}",
    ] {
        assert!(artifacts.contains(name), "artifact {name} is not uploaded");
    }
    for forbidden in ["gh release", "snapcraft upload", "--clobber"] {
        assert!(
            !scripts.contains(forbidden),
            "forbidden AppImage workflow command: {forbidden}"
        );
    }
    let build = &jobs["build"];
    let rows = build["strategy"]["matrix"]["include"]
        .as_array()
        .ok_or("build matrix absent")?
        .iter()
        .map(|row| {
            Ok((
                row["architecture"].as_str().ok_or("architecture absent")?,
                row["runner"].as_str().ok_or("runner absent")?,
            ))
        })
        .collect::<Result<BTreeSet<_>>>()?;
    assert_eq!(
        rows,
        BTreeSet::from([("amd64", "ubuntu-24.04"), ("arm64", "ubuntu-24.04-arm")])
    );
    for required in [
        "stage-appimage",
        "fetch-appimage-tools",
        "--test appimage_build",
        "build-appimage",
        "collect-appimage-sources",
        "ci-appimage-install",
        "appimage-version . \"$REF_TYPE\" \"$REF_NAME\"",
    ] {
        assert!(
            scripts.contains(required),
            "workflow does not run {required}"
        );
    }
    let hosts = jobs["clean-host"]["strategy"]["matrix"]["host"]
        .as_array()
        .ok_or("clean-host matrix absent")?;
    assert_eq!(
        hosts,
        &vec![json!("debian13"), json!("ubuntu24.04"), json!("fedora43")]
    );
    Ok(())
}

#[test]
fn package_profile_producers_stamp_one_exact_bin_test_from_a_valid_repo_local_sha() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (file, job) in [
        ("appimage-release.yml", "build"),
        ("snap-build.yml", "snap"),
        ("deb-release.yml", "build"),
        ("rpm-release.yml", "build"),
    ] {
        let workflow: Value =
            serde_yaml_ng::from_slice(&fs::read(root.join(".github/workflows").join(file))?)?;
        let scripts = workflow["jobs"][job]["steps"]
            .as_array()
            .ok_or("package steps absent")?
            .iter()
            .filter_map(|step| step["run"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            scripts.matches("capture-package-plugin-profile").count(),
            1,
            "{file}"
        );
        assert!(
            scripts.contains("cargo test --bin among --locked --no-run --message-format=json"),
            "{file}"
        );
        let source = scripts
            .find("source_sha=\"$(git -c \"safe.directory=$(pwd -P)\" rev-parse HEAD)\"")
            .ok_or("repository-local source stamp absent")?;
        let length = scripts
            .find("test \"${#source_sha}\" -eq 40")
            .ok_or("source SHA length check absent")?;
        let compile = scripts
            .find("AMONG_CHECK_SOURCE_SHA=\"$source_sha\"")
            .ok_or("compiled-in source stamp absent")?;
        assert!(scripts.contains("''|*[!0-9a-f]*)"), "{file}");
        assert!(
            source < length && length < compile,
            "source must be checked before compilation: {file}"
        );
        assert!(!scripts.contains("safe.directory=*"), "{file}");
    }
    Ok(())
}
