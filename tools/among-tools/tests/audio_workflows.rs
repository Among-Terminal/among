use among_tools::Result;
use serde_yaml_ng::Value;
use std::{fs, path::Path};

fn workflow(name: &str) -> Result<Value> {
    Ok(serde_yaml_ng::from_slice(&fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(name),
    )?)?)
}
fn runs(job: &Value) -> String {
    job["steps"]
        .as_sequence()
        .into_iter()
        .flatten()
        .filter_map(|step| step["run"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn both_native_architectures_transfer_artifact_bound_audio_probes() -> Result<()> {
    let snap = workflow(".github/workflows/snap-build.yml")?;
    let appimage = workflow(".github/workflows/appimage-release.yml")?;
    for (workflow, job) in [(&snap, "snap"), (&appimage, "build")] {
        assert_eq!(workflow["permissions"]["contents"].as_str(), Some("read"));
        let run = runs(&workflow["jobs"][job]);
        assert!(run.contains("--example audio-probe"));
        assert!(run.contains("bind-audio-probe"));
        assert!(run.contains("audio/audio-receipt.json"));
        assert!(run.contains("git rev-parse HEAD"));
        let checkout = workflow["jobs"][job]["steps"]
            .as_sequence()
            .and_then(|steps| {
                steps.iter().find(|step| {
                    step["uses"]
                        .as_str()
                        .is_some_and(|s| s.starts_with("actions/checkout@"))
                })
            })
            .ok_or("checkout missing")?;
        assert_eq!(
            checkout["with"]["ref"].as_str(),
            Some("${{ github.event.pull_request.head.sha || github.sha }}")
        );
    }
    let host = &appimage["jobs"]["clean-host"];
    assert!(runs(host).contains("check-audio-package appimage"));
    assert!(host["steps"]
        .as_sequence()
        .ok_or("steps missing")?
        .iter()
        .any(|step| step["with"]["name"].as_str()
            == Some("appimage-audio-${{ matrix.host }}-${{ matrix.architecture }}")
            && step["if"].as_str() == Some("${{ always() }}")));
    let runtime = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../packaging/ci/snap-runtime/check-installed.sh"),
    )?;
    assert!(runtime.contains("check-audio-package snap-installed"));
    assert_eq!(
        snap["jobs"]["clean-runtime"]["if"].as_str(),
        Some("${{ !cancelled() }}")
    );
    assert!(runtime.contains("|| status=1"));
    assert!(runs(&snap["jobs"]["snap"]).contains("CARGO_BUILD_JOBS=1"));
    assert_eq!(
        appimage["jobs"]["build"]["env"]["CARGO_BUILD_JOBS"].as_str(),
        Some("1")
    );
    assert!(runs(&appimage["jobs"]["build"]).contains("--example audio-retirement-probe"));
    assert!(runs(&snap["jobs"]["snap"]).contains("--example audio-retirement-probe"));
    assert!(runs(host).contains("check-audio-retirement"));
    assert!(runtime.contains("check-audio-retirement"));
    Ok(())
}
