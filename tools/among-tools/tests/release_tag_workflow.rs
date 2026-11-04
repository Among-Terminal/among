//! `release-tag.yml` creates tags with the repository token and starts the
//! draft workflow on them. The decision logic is tested in `release_tag.rs`;
//! these tests pin what the workflow lets that token do: which events start it,
//! which job holds which scope, that the write job checks out and builds
//! nothing, and that no outside action is unpinned and no secret is read.

use among_tools::{deb_workflow, Result};
use serde_json::{json, Value};
use std::{fs, path::Path};

fn workflow() -> Result<Value> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/release-tag.yml");
    deb_workflow::read(&fs::read_to_string(path)?)
}

fn script(job: &Value) -> Result<String> {
    Ok(job["steps"]
        .as_array()
        .ok_or("steps")?
        .iter()
        .filter_map(|step| step["run"].as_str())
        .flat_map(str::lines)
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n"))
}

#[test]
fn only_a_push_to_main_or_a_manual_run_starts_it() -> Result<()> {
    let document = workflow()?;
    assert_eq!(document["on"]["push"], json!({"branches":["main"]}));
    let inputs = &document["on"]["workflow_dispatch"]["inputs"];
    assert_eq!(inputs.as_object().map(|map| map.len()), Some(3));
    assert_eq!(inputs["candidate"]["type"], "string");
    assert_eq!(inputs["candidate"]["required"], false);
    assert_eq!(inputs["stable"]["type"], "boolean");
    assert_eq!(inputs["stable"]["required"], false);
    // A stable tag must never be made by a default-valued run.
    assert_eq!(inputs["stable"]["default"], false);
    assert_eq!(inputs["release_commit"]["type"], "string");
    assert_eq!(inputs["release_commit"]["required"], false);
    assert_eq!(document["on"].as_object().map(|map| map.len()), Some(2));
    assert_eq!(document["permissions"], json!({}));
    // One group per commit and request: a shared group would drop a queued
    // release run.
    assert_eq!(
        document["concurrency"]["group"],
        "release-tag-${{ github.sha }}-${{ inputs.candidate }}-${{ inputs.stable }}-${{ inputs.release_commit }}"
    );
    assert_eq!(document["concurrency"]["cancel-in-progress"], false);
    Ok(())
}

#[test]
fn the_decision_job_can_only_read_and_the_write_job_runs_no_checked_out_code() -> Result<()> {
    let document = workflow()?;
    let jobs = document["jobs"].as_object().ok_or("jobs")?;
    assert_eq!(jobs.len(), 2);

    let decide = &jobs["decide"];
    assert_eq!(decide["permissions"], json!({"contents":"read"}));
    let steps = decide["steps"].as_array().ok_or("steps")?;
    let checkout = &steps[0];
    assert!(checkout["uses"]
        .as_str()
        .is_some_and(|uses| uses.starts_with("actions/checkout@")));
    assert_eq!(checkout["with"]["fetch-depth"], 0);
    assert_eq!(checkout["with"]["persist-credentials"], false);
    let shell = script(decide)?;
    assert!(shell.contains("release-tag . push \"$GITHUB_SHA\""));
    for required in [
        "release-tag . candidate \"$CANDIDATE\" \"$RELEASE_COMMIT\"",
        "release-tag . stable \"$RELEASE_COMMIT\"",
        "[ \"$GITHUB_REF\" = refs/heads/main ]",
        "give either candidate or stable, not both",
        "give candidate=N or stable=true",
    ] {
        assert!(shell.contains(required), "missing: {required}");
    }
    // The inputs reach the shell only through the environment.
    assert!(!shell.contains("${{"), "an expression is interpolated");
    let env = &steps[2]["env"];
    assert_eq!(env["CANDIDATE"], "${{ inputs.candidate }}");
    assert_eq!(env["STABLE"], "${{ inputs.stable }}");
    assert_eq!(env["RELEASE_COMMIT"], "${{ inputs.release_commit }}");

    let tag = &jobs["tag"];
    assert_eq!(
        tag["permissions"],
        json!({"contents":"write","actions":"write"})
    );
    assert_eq!(tag["needs"], "decide");
    assert_eq!(
        tag["if"],
        "needs.decide.outputs.action == 'create' || needs.decide.outputs.action == 'exists'"
    );
    for step in tag["steps"].as_array().ok_or("steps")? {
        assert!(step.get("uses").is_none(), "the write job runs an action");
    }
    for (name, job) in jobs {
        for step in job["steps"].as_array().ok_or("steps")? {
            if let Some(uses) = step["uses"].as_str() {
                let (_, revision) = uses.split_once('@').ok_or("unpinned action")?;
                assert_eq!(revision.len(), 40, "{name}: {uses}");
            }
        }
        let text = job.to_string();
        assert!(!text.contains("secrets."), "{name} reads a secret");
    }
    Ok(())
}

#[test]
fn the_write_job_only_creates_the_decided_tag_and_starts_the_draft() -> Result<()> {
    let document = workflow()?;
    let tag = &document["jobs"]["tag"];
    assert_eq!(tag["env"]["ACTION"], "${{ needs.decide.outputs.action }}");
    assert_eq!(tag["env"]["TAG"], "${{ needs.decide.outputs.tag }}");
    assert_eq!(tag["env"]["SHA"], "${{ needs.decide.outputs.sha }}");
    let shell = script(tag)?;
    // Outputs reach the shell only through the environment, and are validated.
    assert!(
        !shell.contains("${{"),
        "an expression is interpolated into the script"
    );
    for required in [
        "^v[0-9]+\\.[0-9]+\\.[0-9]+(-rc\\.[0-9]+)?$",
        "^[0-9a-f]{40}$",
        "if [ \"$ACTION\" = create ]; then",
        "git/refs\" -f \"ref=refs/tags/$TAG\" -f \"sha=$SHA\"",
        "gh workflow run release-draft.yml --repo \"$repo\" --ref \"$TAG\"",
    ] {
        assert!(shell.contains(required), "missing: {required}");
    }
    for forbidden in [
        "gh release",
        "draft=false",
        "--method DELETE",
        "--method PATCH",
        "--force",
    ] {
        assert!(!shell.contains(forbidden), "{forbidden}");
    }
    Ok(())
}
