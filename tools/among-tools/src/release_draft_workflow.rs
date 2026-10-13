//! Structure policy for `.github/workflows/release-draft.yml`.
//!
//! The workflow is the only automation that writes to GitHub Releases, so its
//! shape is the security boundary: which event starts it, which job holds the
//! write token, what that job is allowed to run, and that a draft is only
//! filled after every package workflow of the same run succeeded.

use crate::{release_assets, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub const PATH: &str = ".github/workflows/release-draft.yml";
pub const CALLED: [(&str, &str); 3] = [
    ("deb", "./.github/workflows/deb-release.yml"),
    ("rpm", "./.github/workflows/rpm-release.yml"),
    ("appimage", "./.github/workflows/appimage-release.yml"),
];

fn require(value: bool, message: &str) -> Result<()> {
    if value {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn needs(job: &Value) -> BTreeSet<String> {
    match job.get("needs") {
        Some(Value::String(name)) => BTreeSet::from([name.clone()]),
        Some(Value::Array(names)) => names
            .iter()
            .filter_map(|name| name.as_str().map(str::to_string))
            .collect(),
        _ => BTreeSet::new(),
    }
}

fn steps(job: &Value) -> Result<&Vec<Value>> {
    job["steps"]
        .as_array()
        .ok_or_else(|| "job steps missing".into())
}

fn pinned(action: &str) -> bool {
    action.split_once('@').is_some_and(|(name, sha)| {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_/-".contains(&byte))
            && sha.len() == 40
            && sha
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

fn downloads(job: &Value) -> Result<Vec<(String, String)>> {
    let mut found = Vec::new();
    for step in steps(job)? {
        if step["uses"]
            .as_str()
            .is_some_and(|action| action.starts_with("actions/download-artifact@"))
        {
            found.push((
                step["with"]["name"]
                    .as_str()
                    .ok_or("download step must name one artifact exactly")?
                    .to_string(),
                step["with"]["path"]
                    .as_str()
                    .ok_or("download step must name its path")?
                    .to_string(),
            ));
        }
    }
    Ok(found)
}

fn script(job: &Value) -> Result<String> {
    let mut text = String::new();
    for step in steps(job)? {
        if let Some(run) = step.get("run") {
            text.push_str(run.as_str().ok_or("workflow command is not text")?);
            text.push('\n');
        }
    }
    Ok(text)
}

/// The draft job's main shell step, which offline tests execute against a
/// substitute `gh`.
pub fn draft_script(document: &Value) -> Result<String> {
    steps(&document["jobs"]["draft"])?
        .iter()
        .find(|step| step["id"] == "draft")
        .and_then(|step| step["run"].as_str())
        .map(str::to_string)
        .ok_or_else(|| "draft step missing".into())
}

pub fn validate(document: &Value) -> Result<()> {
    require(
        document["on"] == json!({"push":{"tags":["v*"]}}),
        "release draft workflow must start only from a release tag push",
    )?;
    require(
        document["permissions"] == json!({}),
        "release draft workflow must grant no default token permissions",
    )?;
    require(
        document["concurrency"]["cancel-in-progress"] == false
            && document["concurrency"]["group"]
                .as_str()
                .is_some_and(|group| group.contains("${{ github.ref }}")),
        "runs for one tag must queue, never cancel each other",
    )?;
    let jobs = document["jobs"]
        .as_object()
        .ok_or("release draft jobs missing")?;
    require(
        jobs.keys().map(String::as_str).collect::<BTreeSet<_>>()
            == BTreeSet::from(["deb", "rpm", "appimage", "assemble", "draft"]),
        "release draft workflow must consist of the package calls, assembly and draft jobs",
    )?;
    let read = json!({"contents":"read"});
    for (name, workflow) in CALLED {
        let job = &jobs[name];
        require(
            job["uses"] == workflow && job["permissions"] == read,
            "each package workflow must be called from this repository with read-only contents",
        )?;
        require(
            job.get("with").is_none() && job.get("secrets").is_none(),
            "package workflows must build exactly what the tag builds, without inputs or secrets",
        )?;
    }
    let assemble = &jobs["assemble"];
    require(
        needs(assemble) == CALLED.iter().map(|(name, _)| name.to_string()).collect(),
        "assembly must wait for every package workflow",
    )?;
    require(
        assemble["permissions"] == read,
        "assembly compiles code and must not hold a write token",
    )?;
    let inputs = downloads(assemble)?;
    require(
        inputs.iter().map(|(name, _)| name.clone()).collect::<BTreeSet<_>>()
            == release_assets::input_artifacts()
            && inputs.len() == release_assets::input_artifacts().len()
            && inputs
                .iter()
                .all(|(name, path)| *path == format!("release-inputs/{name}")),
        "assembly must download exactly the expected package artifacts, each into its own directory",
    )?;
    let assembly = script(assemble)?;
    require(
        assembly.contains("stage-release-assets \\\n  release-inputs \"$GITHUB_REF_NAME\" \"$GITHUB_SHA\" release-assets")
            && assembly.contains("rustup toolchain install 1.98.1"),
        "assembly must stage the release files with the pinned toolchain for this tag and commit",
    )?;
    require(
        steps(assemble)?.iter().any(|step| {
            step["uses"]
                .as_str()
                .is_some_and(|action| action.starts_with("actions/upload-artifact@"))
                && step["with"]["name"] == "release-assets"
                && step["with"]["if-no-files-found"] == "error"
        }),
        "assembly must hand over the staged files as the release-assets artifact",
    )?;
    let draft = &jobs["draft"];
    require(
        needs(draft) == BTreeSet::from(["assemble".to_string()]),
        "the draft job must wait for the assembly",
    )?;
    require(
        draft["permissions"] == json!({"contents":"write"}),
        "the draft job needs contents write and nothing else",
    )?;
    require(
        downloads(draft)? == [("release-assets".to_string(), "release-assets".to_string())],
        "the draft job must take only the staged release files",
    )?;
    for step in steps(draft)? {
        if let Some(action) = step["uses"].as_str() {
            require(
                action.starts_with("actions/download-artifact@"),
                "the write-token job must not check out or run other actions",
            )?;
        }
    }
    let publish = draft_script(document)?;
    for required in [
        "set -euo pipefail",
        "-F draft=true -F prerelease=\"$prerelease\"",
        "*-rc.*) prerelease=true ;;",
        "repos/$repo/commits/refs/tags/$tag",
        "sha256sum --check --strict SHA256SUMS",
        "already has a published release",
        "must have prerelease=$prerelease",
        "differs from this build",
    ] {
        require(
            publish.contains(required),
            &format!("the draft step lacks a required safeguard: {required}"),
        )?;
    }
    for forbidden in [
        "draft=false",
        "--draft=false",
        "gh release edit",
        "gh release create",
        "gh release delete",
        "--clobber",
        "--method PATCH",
        "--method DELETE",
        "chmod",
    ] {
        require(
            !publish.contains(forbidden),
            &format!("the draft step must not publish, replace or execute anything: {forbidden}"),
        )?;
    }
    for (name, job) in jobs {
        if name != "draft" {
            require(
                !job["permissions"]
                    .as_object()
                    .is_some_and(|scopes| scopes.values().any(|scope| scope == "write")),
                "only the draft job may hold a write permission",
            )?;
        }
        if job.get("steps").is_some() {
            for step in steps(job)? {
                if let Some(action) = step["uses"].as_str() {
                    require(pinned(action), "workflow action must be SHA-pinned")?;
                }
            }
            require(
                !script(job)?.contains("python"),
                "release automation must not run Python",
            )?;
            // Hosted runners provide a per-job temporary directory; a fixed
            // path under /tmp would be shared with anything else on the host.
            require(
                !job.to_string().contains("/tmp/"),
                "release jobs must keep scratch files under the runner's temporary directory",
            )?;
        }
    }
    Ok(())
}
