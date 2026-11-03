//! `release-tag.yml` creates tags with the repository token and starts the
//! draft workflow on them. These tests pin what that token can do: which
//! events start it, which scopes each job holds, that no outside action or
//! secret is involved, and that the shell never sees text from an expression.

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
        .collect::<Vec<_>>()
        .join("\n"))
}

#[test]
fn only_a_push_to_main_or_a_manual_run_starts_it() -> Result<()> {
    let document = workflow()?;
    assert_eq!(document["on"]["push"], json!({"branches":["main"]}));
    let inputs = &document["on"]["workflow_dispatch"]["inputs"];
    assert_eq!(inputs.as_object().map(|map| map.len()), Some(1));
    assert_eq!(inputs["candidate"]["type"], "string");
    assert_eq!(document["on"].as_object().map(|map| map.len()), Some(2));
    assert_eq!(document["permissions"], json!({}));
    // One group per commit: a shared group would drop a queued release run.
    assert_eq!(
        document["concurrency"]["group"],
        "release-tag-${{ github.sha }}"
    );
    assert_eq!(document["concurrency"]["cancel-in-progress"], false);
    Ok(())
}

#[test]
fn the_token_can_only_write_tags_and_start_workflows() -> Result<()> {
    let document = workflow()?;
    let jobs = document["jobs"].as_object().ok_or("jobs")?;
    assert_eq!(jobs.len(), 2);
    for (name, job) in jobs {
        assert_eq!(
            job["permissions"],
            json!({"contents":"write","actions":"write"}),
            "{name}"
        );
        for step in job["steps"].as_array().ok_or("steps")? {
            assert!(step.get("uses").is_none(), "{name} runs an outside action");
        }
        let text = job.to_string();
        assert!(!text.contains("secrets."), "{name} reads a secret");
        let shell = script(job)?;
        for forbidden in [
            "gh release",
            "draft=false",
            "--method DELETE",
            "--method PATCH",
            "--force",
        ] {
            assert!(!shell.contains(forbidden), "{name}: {forbidden}");
        }
        // Expression text reaches the shell only through the environment.
        assert!(
            !shell.contains("${{"),
            "{name} interpolates an expression into the script"
        );
    }
    Ok(())
}

#[test]
fn a_release_commit_is_recognised_by_title_and_by_the_version_it_carries() -> Result<()> {
    let document = workflow()?;
    assert_eq!(document["jobs"]["tag"]["if"], "github.event_name == 'push'");
    let shell = script(&document["jobs"]["tag"])?;
    for required in [
        "chore\\(release\\): ([0-9]+\\.[0-9]+\\.[0-9]+)( \\(#[0-9]+\\))?$",
        "contents/Cargo.toml?ref=$GITHUB_SHA",
        "the title says $version but Cargo.toml says",
        "already exists at $existing, not at $GITHUB_SHA",
        "gh workflow run release-draft.yml --repo \"$repo\" --ref \"$tag\"",
        "refs/tags/$tag",
    ] {
        assert!(shell.contains(required), "missing: {required}");
    }
    Ok(())
}

#[test]
fn a_candidate_tags_the_stable_release_commit_on_main_only() -> Result<()> {
    let document = workflow()?;
    let job = &document["jobs"]["candidate"];
    assert_eq!(job["if"], "github.event_name == 'workflow_dispatch'");
    assert_eq!(job["env"]["CANDIDATE"], "${{ inputs.candidate }}");
    let shell = script(job)?;
    for required in [
        "[ \"$GITHUB_REF\" = refs/heads/main ]",
        "^[1-9][0-9]*$",
        "commits/refs/tags/v$version",
        "ahead|identical",
        "v$version-rc.$CANDIDATE",
        "already exists at $existing, not at $sha",
    ] {
        assert!(shell.contains(required), "missing: {required}");
    }
    Ok(())
}
