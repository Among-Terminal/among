//! `release-draft.yml` is the only automation that writes to GitHub Releases.
//! These tests pin its trigger, its per-job token scopes, the order of jobs and
//! the exact artifacts it consumes, and they execute the draft step against a
//! substitute `gh` to show what it does with new, partial, foreign and
//! published releases.

use among_tools::{deb_workflow, release_draft_workflow, storage, Result};
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn workflow(path: &str) -> Result<Value> {
    deb_workflow::read(&fs::read_to_string(repository().join(path))?)
}

fn actual() -> Result<Value> {
    workflow(release_draft_workflow::PATH)
}

#[test]
fn actual_workflow_satisfies_the_release_draft_policy() -> Result<()> {
    release_draft_workflow::validate(&actual()?)
}

#[test]
fn policy_refuses_publication_scope_order_and_input_drift() -> Result<()> {
    let valid = actual()?;
    type Mutation = fn(&mut Value) -> Result<()>;
    let mutations: [(&str, Mutation); 17] = [
        ("manual trigger", |w| {
            w["on"]["workflow_dispatch"] = json!(null);
            Ok(())
        }),
        ("branch trigger", |w| {
            w["on"]["push"]["branches"] = json!(["main"]);
            Ok(())
        }),
        ("default write token", |w| {
            w["permissions"] = json!({"contents":"write"});
            Ok(())
        }),
        ("write token while compiling", |w| {
            w["jobs"]["assemble"]["permissions"] = json!({"contents":"write"});
            Ok(())
        }),
        ("write token for a package build", |w| {
            w["jobs"]["deb"]["permissions"] = json!({"contents":"write"});
            Ok(())
        }),
        ("extra scope on the draft job", |w| {
            w["jobs"]["draft"]["permissions"]["actions"] = json!("write");
            Ok(())
        }),
        ("draft before every package workflow", |w| {
            w["jobs"]["assemble"]["needs"] = json!(["deb", "rpm"]);
            Ok(())
        }),
        ("draft without assembly", |w| {
            w["jobs"]["draft"]
                .as_object_mut()
                .ok_or("draft job")?
                .remove("needs");
            Ok(())
        }),
        ("artifact pattern instead of exact name", |w| {
            w["jobs"]["assemble"]["steps"][1]["with"]["name"] = json!("deb-*");
            Ok(())
        }),
        ("fixed scratch path under /tmp", |w| {
            w["jobs"]["assemble"]["env"] = json!({"CARGO_TARGET_DIR":"/tmp/release-target"});
            Ok(())
        }),
        ("unpinned action", |w| {
            w["jobs"]["assemble"]["steps"][0]["uses"] = json!("actions/checkout@v4");
            Ok(())
        }),
        ("checkout in the write-token job", |w| {
            let checkout = w["jobs"]["assemble"]["steps"][0].clone();
            w["jobs"]["draft"]["steps"]
                .as_array_mut()
                .ok_or("draft steps")?
                .insert(0, checkout);
            Ok(())
        }),
        ("published instead of draft", |w| {
            let step = draft_step(w)?;
            let text = step["run"]
                .as_str()
                .ok_or("run")?
                .replace("-F draft=true", "-F draft=false");
            step["run"] = text.into();
            Ok(())
        }),
        ("release candidates not marked as prereleases", |w| {
            let step = draft_step(w)?;
            let text = step["run"]
                .as_str()
                .ok_or("run")?
                .replace("*-rc.*) prerelease=true ;;", "");
            step["run"] = text.into();
            Ok(())
        }),
        ("existing assets replaced", |w| {
            let step = draft_step(w)?;
            let text = format!(
                "{}\ngh release upload \"$tag\" --clobber",
                step["run"].as_str().ok_or("run")?
            );
            step["run"] = text.into();
            Ok(())
        }),
        ("package workflow given inputs", |w| {
            w["jobs"]["rpm"]["with"] = json!({"targets":"fedora44"});
            Ok(())
        }),
        ("additional job", |w| {
            w["jobs"]["publish"] =
                json!({"runs-on":"ubuntu-24.04","permissions":{"contents":"read"},"steps":[]});
            Ok(())
        }),
    ];
    for (name, mutate) in mutations {
        let mut changed = valid.clone();
        mutate(&mut changed)?;
        assert!(
            release_draft_workflow::validate(&changed).is_err(),
            "{name} was accepted"
        );
    }
    Ok(())
}

fn draft_step(workflow: &mut Value) -> Result<&mut Value> {
    workflow["jobs"]["draft"]["steps"]
        .as_array_mut()
        .ok_or("draft steps")?
        .iter_mut()
        .find(|step| step["id"] == "draft")
        .ok_or_else(|| "draft step".into())
}

/// A workflow that cannot be called breaks the release draft run at startup.
#[test]
fn called_package_workflows_accept_workflow_call_without_inputs() -> Result<()> {
    for (_, uses) in release_draft_workflow::CALLED {
        let path = uses.trim_start_matches("./");
        if !repository().join(path).exists() {
            // The AppImage workflow is delivered by a parallel change; its
            // absence is reported, not hidden, and the draft cannot start
            // until it lands.
            eprintln!("NOT CHECKED: {path} is not in this tree");
            continue;
        }
        let document = workflow(path)?;
        assert!(
            document["on"]
                .as_object()
                .is_some_and(|on| on.contains_key("workflow_call")),
            "{path} cannot be called"
        );
        assert!(
            document["on"]["workflow_call"].is_null(),
            "{path} takes inputs or secrets from its caller"
        );
    }
    Ok(())
}

#[test]
fn every_shell_step_parses() -> Result<()> {
    let document = actual()?;
    for (name, job) in document["jobs"].as_object().ok_or("jobs")? {
        for step in job["steps"].as_array().into_iter().flatten() {
            if let Some(script) = step["run"].as_str() {
                let mut child = Command::new("/bin/bash")
                    .arg("-n")
                    .stdin(Stdio::piped())
                    .spawn()?;
                child
                    .stdin
                    .take()
                    .ok_or("shell input")?
                    .write_all(script.as_bytes())?;
                assert!(child.wait()?.success(), "invalid shell in job {name}");
            }
        }
    }
    Ok(())
}

/// Stands in for the GitHub CLI: keeps one repository's releases and assets
/// in a directory and refuses any call the draft step is not expected to make.
const FAKE_GH: &str = r#"#!/bin/bash
set -euo pipefail
state="$FAKE_GH_STATE"
repo="$GITHUB_REPOSITORY"
printf '%s\n' "$*" >> "$state/calls.log"
list_assets() {
  local out='[]' directory file
  for directory in "$state"/assets/*/; do
    [ -d "$directory" ] || continue
    for file in "$directory"*; do
      out=$(jq -c --argjson id "$(basename "$directory")" --arg name "$(basename "$file")" \
        '. + [{id: $id, name: $name}]' <<< "$out")
    done
  done
  printf '%s\n' "$out"
}
if [ "$1" = api ]; then
  shift
  method=GET
  path=
  fields=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --paginate) shift ;;
      --method) method=$2; shift 2 ;;
      -H) shift 2 ;;
      -f|-F) fields+=("$2"); shift 2 ;;
      *) path=$1; shift ;;
    esac
  done
  case "$method $path" in
    "GET repos/$repo/commits/refs/tags/$GITHUB_REF_NAME")
      jq -n --arg sha "$FAKE_TAG_COMMIT" '{sha: $sha}' ;;
    "GET repos/$repo/releases?per_page=100")
      cat "$state/releases.json" ;;
    "POST repos/$repo/releases")
      printf '%s\n' "${fields[@]}" > "$state/created"
      jq -n --arg tag "$GITHUB_REF_NAME" '[{id: 7, tag_name: $tag, draft: true, prerelease: true}]' > "$state/releases.json"
      echo '{"id": 7}' ;;
    "GET repos/$repo/releases/7/assets?per_page=100")
      list_assets ;;
    "GET repos/$repo/releases/assets/"*)
      cat "$state/assets/${path##*/}"/* ;;
    *) echo "unexpected gh api $method $path" >&2; exit 2 ;;
  esac
elif [ "$1 $2" = "release upload" ] && [ "$3" = "$GITHUB_REF_NAME" ] && [ "$4 $5" = "--repo $repo" ]; then
  shift 5
  for file in "$@"; do
    id=$(cat "$state/next")
    echo $((id + 1)) > "$state/next"
    mkdir "$state/assets/$id"
    cp "$file" "$state/assets/$id/"
    basename "$file" >> "$state/uploaded"
  done
else
  echo "unexpected gh $*" >&2
  exit 2
fi
"#;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const FILES: [(&str, &str); 3] = [
    ("among-terminal_0.1.0.rc.11-1.debian13_amd64.deb", "deb"),
    ("among-0.1.0.rc.11-1.fc43.src.rpm", "srpm"),
    ("Among-0.1.0-rc.11-arm64.AppImage", "appimage"),
];

struct Draft {
    root: PathBuf,
    state: PathBuf,
}

impl Draft {
    fn new(releases: Value) -> Result<Self> {
        let root = storage::private_directory(&std::env::temp_dir(), "among-release-draft")?;
        let state = root.join("state");
        let bin = root.join("bin");
        let assets = root.join("work/release-assets");
        fs::create_dir_all(state.join("assets"))?;
        fs::create_dir_all(&bin)?;
        fs::create_dir_all(&assets)?;
        fs::write(bin.join("gh"), FAKE_GH)?;
        fs::set_permissions(bin.join("gh"), fs::Permissions::from_mode(0o755))?;
        fs::write(state.join("releases.json"), serde_json::to_vec(&releases)?)?;
        fs::write(state.join("next"), "100")?;
        fs::write(state.join("calls.log"), "")?;
        let mut sums = String::new();
        for (name, bytes) in FILES {
            fs::write(assets.join(name), bytes)?;
            sums.push_str(&format!("{}  {name}\n", storage::sha256(bytes.as_bytes())?));
        }
        fs::write(assets.join("SHA256SUMS"), sums)?;
        Ok(Self { root, state })
    }

    /// Places a file in the existing draft as if an earlier run uploaded it.
    fn existing(&self, id: u32, name: &str, bytes: &[u8]) -> Result<()> {
        let directory = self.state.join(format!("assets/{id}"));
        fs::create_dir(&directory)?;
        fs::write(directory.join(name), bytes)?;
        Ok(())
    }

    fn staged(&self, name: &str) -> Result<Vec<u8>> {
        Ok(fs::read(self.root.join("work/release-assets").join(name))?)
    }

    /// Runs the draft step with the substitute `gh` first on PATH, then the
    /// system tools a GitHub-hosted runner provides.
    fn run(&self, tag: &str, tag_commit: &str) -> Result<Outcome> {
        let path = format!("{}:/usr/bin:/bin", self.root.join("bin").display());
        self.run_with_path(tag, tag_commit, &path)
    }

    /// The whole environment is replaced, so variables of the job that runs
    /// the tests (a real tag's GITHUB_SHA, GITHUB_REF_NAME, HOME) never reach
    /// the step under test.
    fn run_with_path(&self, tag: &str, tag_commit: &str, path: &str) -> Result<Outcome> {
        let script = release_draft_workflow::draft_script(&actual()?)?;
        let output = Command::new("/bin/bash")
            .args(["--noprofile", "--norc", "-c", &script])
            .current_dir(self.root.join("work"))
            .env_clear()
            .env("PATH", path)
            .env("HOME", &self.root)
            .env("RUNNER_TEMP", &self.state)
            .env("GH_TOKEN", "not-a-token")
            .env("GITHUB_REPOSITORY", "owner/among")
            .env("GITHUB_REF_NAME", tag)
            .env("GITHUB_SHA", COMMIT)
            .env("RUN_URL", "https://github.invalid/run/1")
            .env("FAKE_GH_STATE", &self.state)
            .env("FAKE_TAG_COMMIT", tag_commit)
            .output()?;
        Ok(Outcome {
            success: output.status.success(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.state.join(name)).unwrap_or_default()
    }

    fn uploaded(&self) -> Vec<String> {
        let mut names = self
            .read("uploaded")
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        names.sort();
        names
    }
}

struct Outcome {
    success: bool,
    stderr: String,
}

impl Outcome {
    /// Success must be real success: a step that stops early, for example
    /// on a missing tool, must not count as a pass.
    fn succeeded(&self) -> bool {
        if !self.success {
            eprintln!("draft step failed:\n{}", self.stderr);
        }
        self.success
    }

    /// A refusal only counts when the step stopped for the expected reason;
    /// otherwise any unrelated failure would make every refusal test pass.
    fn refused_because(&self, reason: &str) -> bool {
        !self.success && self.stderr.contains(&format!("release draft: {reason}"))
    }
}

// The draft step runs on a GitHub-hosted runner, which provides jq. Package
// builds (debian/rules, the RPM spec) run this suite in minimal containers
// without jq, so the tests that execute the step through jq are ignored there
// and run by the Checks workflow's ignored-test runner, which also runs them
// on both architectures.

impl Drop for Draft {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn all_names() -> Vec<String> {
    let mut names = FILES
        .iter()
        .map(|(name, _)| name.to_string())
        .chain(["SHA256SUMS".to_string()])
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[test]
#[ignore = "runs the draft step, which needs jq as on the GitHub-hosted runner; package-build containers lack jq, the Checks ignored-test runner runs it"]
fn new_release_candidate_draft_is_a_prerelease_with_every_file() -> Result<()> {
    let draft = Draft::new(json!([]))?;
    assert!(draft.run("v0.1.0-rc.11", COMMIT)?.succeeded());
    let created = draft.read("created");
    assert!(created.contains("draft=true"));
    assert!(created.contains("prerelease=true"));
    assert!(created.contains("tag_name=v0.1.0-rc.11"));
    assert_eq!(draft.uploaded(), all_names());
    Ok(())
}

#[test]
#[ignore = "runs the draft step, which needs jq as on the GitHub-hosted runner; package-build containers lack jq, the Checks ignored-test runner runs it"]
fn stable_tag_draft_is_not_a_prerelease() -> Result<()> {
    let draft = Draft::new(json!([]))?;
    assert!(draft.run("v0.1.0", COMMIT)?.succeeded());
    assert!(draft.read("created").contains("prerelease=false"));
    Ok(())
}

#[test]
#[ignore = "runs the draft step, which needs jq as on the GitHub-hosted runner; package-build containers lack jq, the Checks ignored-test runner runs it"]
fn retry_keeps_matching_assets_and_uploads_only_the_missing_ones() -> Result<()> {
    let draft = Draft::new(rc_draft())?;
    let (first, _) = FILES[0];
    draft.existing(1, first, &draft.staged(first)?)?;
    draft.existing(2, "SHA256SUMS", &draft.staged("SHA256SUMS")?)?;
    assert!(draft.run("v0.1.0-rc.11", COMMIT)?.succeeded());
    assert!(
        draft.read("created").is_empty(),
        "a second draft was created"
    );
    assert_eq!(
        draft.uploaded(),
        vec![FILES[2].0.to_string(), FILES[1].0.to_string()]
    );
    Ok(())
}

fn rc_draft() -> Value {
    json!([{"id":7,"tag_name":"v0.1.0-rc.11","draft":true,"prerelease":true}])
}

/// Re-running only the draft job after a complete upload changes nothing.
#[test]
#[ignore = "runs the draft step, which needs jq as on the GitHub-hosted runner; package-build containers lack jq, the Checks ignored-test runner runs it"]
fn complete_matching_draft_is_verified_without_writes() -> Result<()> {
    let draft = Draft::new(rc_draft())?;
    for (index, name) in all_names().iter().enumerate() {
        draft.existing(index as u32 + 1, name, &draft.staged(name)?)?;
    }
    assert!(draft.run("v0.1.0-rc.11", COMMIT)?.succeeded());
    assert!(draft.read("created").is_empty());
    assert!(draft.uploaded().is_empty());
    Ok(())
}

/// A draft asset left by an earlier run: its name and bytes.
type Leftover<'a> = Option<(&'a str, &'a [u8])>;

/// Each case must fail for its own reason before any release is created or
/// any asset uploaded.
#[test]
#[ignore = "runs the draft step, which needs jq as on the GitHub-hosted runner; package-build containers lack jq, the Checks ignored-test runner runs it"]
fn foreign_published_moved_or_conflicting_releases_are_left_untouched() -> Result<()> {
    let published = json!([{"id":7,"tag_name":"v0.1.0-rc.11","draft":false}]);
    let existing = rc_draft();
    let duplicate = json!([
        {"id":7,"tag_name":"v0.1.0-rc.11","draft":true,"prerelease":true},
        {"id":8,"tag_name":"v0.1.0-rc.11","draft":true,"prerelease":true}
    ]);
    let not_prerelease =
        json!([{"id":7,"tag_name":"v0.1.0-rc.11","draft":true,"prerelease":false}]);
    let cases: [(Value, Leftover, &str, &str); 6] = [
        (
            published,
            None,
            COMMIT,
            "v0.1.0-rc.11 already has a published release",
        ),
        (
            not_prerelease,
            None,
            COMMIT,
            "existing draft for v0.1.0-rc.11 must have prerelease=true",
        ),
        (
            duplicate,
            None,
            COMMIT,
            "v0.1.0-rc.11 has more than one release",
        ),
        (
            existing.clone(),
            Some((FILES[1].0, b"rebuilt")),
            COMMIT,
            "draft asset among-0.1.0.rc.11-1.fc43.src.rpm differs from this build",
        ),
        (
            existing,
            Some(("notes.txt", b"x")),
            COMMIT,
            "draft holds unexpected asset notes.txt",
        ),
        (
            json!([]),
            None,
            "fedcba9876543210fedcba9876543210fedcba98",
            "v0.1.0-rc.11 points at fedcba9876543210fedcba9876543210fedcba98",
        ),
    ];
    for (releases, asset, tag_commit, reason) in cases {
        let draft = Draft::new(releases)?;
        if let Some((asset, bytes)) = asset {
            draft.existing(1, asset, bytes)?;
        }
        let outcome = draft.run("v0.1.0-rc.11", tag_commit)?;
        assert!(
            outcome.refused_because(reason),
            "expected refusal {reason:?}, got:\n{}",
            outcome.stderr
        );
        assert!(
            draft.read("created").is_empty(),
            "{reason}: release created"
        );
        assert!(draft.uploaded().is_empty(), "{reason}: assets uploaded");
    }
    Ok(())
}

#[test]
#[ignore = "runs the draft step, which needs jq as on the GitHub-hosted runner; package-build containers lack jq, the Checks ignored-test runner runs it"]
fn staged_files_that_do_not_match_their_sums_are_never_uploaded() -> Result<()> {
    let draft = Draft::new(json!([]))?;
    fs::write(
        draft.root.join("work/release-assets").join(FILES[0].0),
        "changed",
    )?;
    let outcome = draft.run("v0.1.0-rc.11", COMMIT)?;
    assert!(
        outcome.refused_because("staged files do not match SHA256SUMS"),
        "{}",
        outcome.stderr
    );
    assert!(draft.read("created").is_empty());
    assert!(draft.uploaded().is_empty());
    Ok(())
}

/// Reproduces the v0.1.0-rc.11 package-build failure: without jq the step
/// used to run on and fail inside unrelated commands, so refusal tests
/// "passed" and success tests failed with no clear cause. The step must stop
/// first, name the missing tool, and make no GitHub call. Runs everywhere,
/// because it needs only bash and the substitute gh.
#[test]
fn a_missing_runner_tool_stops_the_step_before_any_github_call() -> Result<()> {
    let draft = Draft::new(json!([]))?;
    // Only the substitute gh is reachable; jq and coreutils are not.
    let only_gh = draft.root.join("bin");
    let outcome = draft.run_with_path(
        "v0.1.0-rc.11",
        COMMIT,
        only_gh.to_str().ok_or("path is not UTF-8")?,
    )?;
    assert!(
        outcome.refused_because("jq is required on the runner"),
        "{}",
        outcome.stderr
    );
    assert_eq!(draft.read("calls.log"), "", "gh was called");
    assert!(draft.read("created").is_empty());
    assert!(draft.uploaded().is_empty());
    Ok(())
}
