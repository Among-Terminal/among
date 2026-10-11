//! `rpm-release.yml` builds candidates. It must never publish one.
//!
//! The distinction matters: building an RPM proves the spec works, while
//! attaching it to a release makes it installable by users. A workflow that
//! drifts into uploading, or that gains write scope or a publication
//! credential, would turn a build gate into a distribution channel.

use among_tools::Result;
use std::fs;

#[test]
fn deb_rc_tags_do_not_start_stable_rpm_builds() -> Result<()> {
    let document: serde_yaml_ng::Value = serde_yaml_ng::from_str(&workflow()?)?;
    assert_eq!(
        document["jobs"]["build"]["if"].as_str(),
        Some("${{ !contains(github.ref, '-rc.') }}")
    );
    Ok(())
}

fn workflow() -> Result<String> {
    // cargo runs integration tests with the package directory as the working
    // directory, so the repository root is two levels up.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    Ok(fs::read_to_string(
        root.join(".github/workflows/rpm-release.yml"),
    )?)
}

#[test]
fn the_rpm_workflow_is_valid_yaml_with_exactly_the_four_targets() -> Result<()> {
    let text = workflow()?;
    let document: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&text).map_err(|error| format!("invalid YAML: {error}"))?;
    let jobs = document["jobs"].as_mapping().ok_or("no jobs")?;
    let build = jobs.get("build").ok_or("no build job")?;
    let include = build["strategy"]["matrix"]["include"]
        .as_sequence()
        .ok_or("no matrix")?;
    let targets = include
        .iter()
        .map(|entry| {
            (
                entry["target"].as_str().unwrap_or_default().to_string(),
                entry["architecture"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        targets,
        vec![
            ("fedora43".into(), "amd64".into()),
            ("fedora43".into(), "arm64".into()),
            ("fedora44".into(), "amd64".into()),
            ("fedora44".into(), "arm64".into()),
        ],
        "the RPM target matrix changed"
    );
    Ok(())
}

/// Only a release tag may build a candidate. A branch build would produce an
/// artifact whose version does not correspond to any release.
/// The arm64 legs must run on an arm64 host. An arm64 container on an amd64
/// host still produces the right bytes, so nothing else in the job would
/// notice the substitution.
#[test]
fn each_architecture_runs_on_a_matching_native_runner() -> Result<()> {
    let text = workflow()?;
    let document: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text)?;
    let include = document["jobs"]["build"]["strategy"]["matrix"]["include"]
        .as_sequence()
        .ok_or("no matrix")?;
    for entry in include {
        let architecture = entry["architecture"].as_str().unwrap_or_default();
        let runner = entry["runner"].as_str().unwrap_or_default();
        match architecture {
            "arm64" => assert!(
                runner.ends_with("-arm"),
                "the arm64 target would run on {runner}, not a native arm64 runner"
            ),
            "amd64" => assert!(
                !runner.ends_with("-arm"),
                "the amd64 target would run on the arm64 runner {runner}"
            ),
            other => return Err(format!("unexpected architecture {other}").into()),
        }
    }
    Ok(())
}

/// Every action must be pinned to a commit. A floating tag means the code that
/// runs during a release can change without review.
#[test]
fn every_action_is_pinned_to_a_full_commit() -> Result<()> {
    let text = workflow()?;
    let document: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text)?;
    let mut checked = 0;
    for step in document["jobs"]["build"]["steps"]
        .as_sequence()
        .ok_or("no steps")?
    {
        if let Some(uses) = step["uses"].as_str() {
            // A pin may carry a trailing comment naming the release.
            let reference = uses
                .rsplit('@')
                .next()
                .unwrap_or_default()
                .split('#')
                .next()
                .unwrap_or_default()
                .trim();
            assert!(
                reference.len() == 40 && reference.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "action {uses} is not pinned to a full commit"
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "no actions found to check");
    Ok(())
}

/// Write scope must not exist at workflow or job level. Publication is a
/// separate gate, so nothing here may be able to attach an artifact.
#[test]
fn no_write_scope_exists_at_any_level() -> Result<()> {
    let text = workflow()?;
    let document: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text)?;
    let scopes = [
        ("workflow", &document["permissions"]),
        ("job", &document["jobs"]["build"]["permissions"]),
    ];
    for (where_, scope) in scopes {
        for key in ["contents", "packages", "id-token", "actions"] {
            if let Some(value) = scope.get(key).and_then(serde_yaml_ng::Value::as_str) {
                assert_ne!(
                    value, "write",
                    "{where_}-level {key} permission is write; this workflow must not publish"
                );
            }
        }
    }
    assert!(
        !text.contains("secrets."),
        "the workflow references a secret; a candidate build must need no credential"
    );
    Ok(())
}

/// The package names must be the ones Fedora actually ships. A wrong name fails
/// the very first step of every leg, which no YAML-parsing test would notice.
#[test]
fn the_fedora_package_names_are_the_real_ones() -> Result<()> {
    let text = workflow()?;
    // Names Fedora 43 and 44 actually provide.
    for required in ["vte291-gtk4-devel", "libappstream-glib", "gtk4-devel"] {
        assert!(
            text.contains(required),
            "the install list does not include {required}"
        );
    }
    // Names that do not exist as packages and would abort `dnf install`.
    for absent in ["vte-gtk4-devel", "appstream-util"] {
        assert!(
            !text.contains(&format!(" \\\n            {absent}\n")),
            "the install list still requests the non-existent package {absent}"
        );
    }
    Ok(())
}

/// The source tarball must be written into %{_sourcedir}, outside the tree being
/// archived; otherwise rpmbuild cannot find Source0 and tar aborts with
/// "file changed as we read it".
#[test]
fn the_source_tarball_is_written_into_the_rpmbuild_sourcedir() -> Result<()> {
    let text = workflow()?;
    assert!(
        text.contains("rpmbuild/SOURCES"),
        "the tarball is not placed in the rpmbuild source directory"
    );
    assert!(
        text.contains("--exclude='./examples/plugins/target'"),
        "a nested Cargo target directory would be shipped in the source tarball"
    );
    Ok(())
}

/// The payload must be checked for bundled native libraries: a shipped `.so`
/// would shadow the system library and break the LGPL obligations this project
/// documents.
#[test]
fn the_payload_is_checked_for_bundled_libraries_and_stray_files() -> Result<()> {
    let text = workflow()?;
    assert!(
        text.contains("\\.so($|\\.)"),
        "no check rejects a bundled shared library"
    );
    assert!(
        text.contains("rpm-unexpected.txt"),
        "the payload is not checked against an allowlist, so a stray file would pass"
    );
    assert!(
        text.contains("binary-rpm-path.txt"),
        "the checks do not use the recorded artifact path, so a glob could match the wrong file"
    );
    Ok(())
}

#[test]
fn the_rpm_workflow_has_read_only_scope_and_no_publication_step() -> Result<()> {
    let text = workflow()?;
    let document: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text)?;
    assert_eq!(
        document["permissions"]["contents"].as_str(),
        Some("read"),
        "the RPM workflow must not have write scope"
    );
    // Nothing here may attach an artifact to a release or upload to a package
    // repository. Candidate retention through upload-artifact is fine: it is
    // private job output, not publication.
    for forbidden in [
        "gh release",
        "gh api --method POST",
        "copr",
        "dnf upload",
        "createrepo --upload",
        "GPG_KEY",
        "OSSRH",
        "FEDORA_COPR",
        "SNAPCRAFT_STORE_CREDENTIALS",
        "AMONG_PROTECTED_APPROVAL",
    ] {
        assert!(
            !text.contains(forbidden),
            "the RPM workflow references {forbidden}, which is publication or credential scope"
        );
    }
    Ok(())
}

/// The spec is packaged, so a change to it must not silently pass review.
#[test]
fn the_workflow_builds_the_repository_spec() -> Result<()> {
    let text = workflow()?;
    assert!(
        text.contains("packaging/rpm/among.spec"),
        "the workflow does not build packaging/rpm/among.spec"
    );
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert!(
        root.join("packaging/rpm/among.spec").is_file(),
        "the spec the workflow builds does not exist"
    );
    Ok(())
}
#[test]
fn the_rpm_workflow_runs_on_release_tags_only() -> Result<()> {
    let text = workflow()?;
    let document: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text)?;
    // YAML 1.1 resolves a bare `on` to the boolean true, so the mapping is
    // probed by key rather than indexed.
    let mapping = document.as_mapping().ok_or("workflow is not a mapping")?;
    let triggers = mapping
        .iter()
        .find(|(key, _)| {
            matches!(key, serde_yaml_ng::Value::String(text) if text == "on" || text == "true")
        })
        .map(|(_, value)| value)
        .ok_or("no triggers")?;
    // EVERY top-level trigger must be the tag push, not just `push`.
    // Any key that names an event would be a trigger. Enumerating the known
    // event names catches pull_request, workflow_dispatch and schedule.
    let events = [
        "push",
        "pull_request",
        "pull_request_target",
        "schedule",
        "workflow_dispatch",
        "workflow_run",
        "repository_dispatch",
        "release",
        "issue_comment",
    ];
    for (key, _) in mapping.iter() {
        let serde_yaml_ng::Value::String(name) = key else {
            continue;
        };
        assert!(
            !events.contains(&name.as_str()) || name == "push",
            "the workflow declares an additional {name} trigger; only a release tag may build candidates"
        );
    }
    let tags = triggers["push"]["tags"]
        .as_sequence()
        .ok_or("no push tags")?;
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].as_str(), Some("v*"));
    assert!(triggers["push"].get("branches").is_none());
    Ok(())
}
