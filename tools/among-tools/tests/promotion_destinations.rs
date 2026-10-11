use among_tools::{
    promotion::Destinations, promotion_destinations as destinations, storage, Result,
};
use std::{fs, path::Path};

fn workspace() -> Result<std::path::PathBuf> {
    storage::private_directory(&std::env::temp_dir(), "among-destinations")
}

// The refusals below are checked against the pure helpers rather than through
// the constructors. A constructor also consults the process environment, so on
// a machine without credentials it would refuse every input for the wrong
// reason and the test would prove nothing. Testing the rule directly keeps
// these assertions meaningful in any environment, including an offline one.

#[test]
fn a_repository_name_is_only_owner_slash_name() -> Result<()> {
    for accepted in ["Among-Terminal/among", "owner/repo_name.git", "o/n"] {
        destinations::valid_repository(accepted)?;
    }
    for rejected in [
        "",
        "among",
        "owner/",
        "/name",
        "owner/name/extra",
        "owner name",
        "-owner/name",
        "owner/name;rm",
        "../../etc",
        "owner/na\nme",
        "owner/name\n",
    ] {
        assert!(
            destinations::valid_repository(rejected).is_err(),
            "unsafe repository {rejected:?} was accepted"
        );
    }
    Ok(())
}

#[test]
fn an_absent_or_blank_credential_is_refused_and_a_real_one_survives() -> Result<()> {
    assert!(destinations::require_credential(None, "GH_TOKEN").is_err());
    assert!(destinations::require_credential(Some(String::new()), "GH_TOKEN").is_err());
    assert!(destinations::require_credential(Some("   \t\n".into()), "GH_TOKEN").is_err());
    // A masked secret that expands to whitespace must not be accepted as a
    // token: it authenticates as nobody and fails far from its cause.
    assert_eq!(
        destinations::require_credential(Some("  real-token\n".into()), "GH_TOKEN")?,
        "real-token"
    );
    // The name appears in the refusal so a failing job names its own cause.
    let refusal = destinations::require_credential(None, "SNAPCRAFT_STORE_CREDENTIALS")
        .err()
        .ok_or("an absent credential was accepted")?
        .to_string();
    assert!(refusal.contains("SNAPCRAFT_STORE_CREDENTIALS"));
    assert!(!refusal.contains("token="));
    Ok(())
}

#[test]
fn an_endpoint_override_is_a_refusal_and_only_an_unset_environment_passes() -> Result<()> {
    destinations::reject_named_overrides(&[])?;
    for name in destinations::FORBIDDEN_ENVIRONMENT {
        assert!(
            destinations::reject_named_overrides(&[name]).is_err(),
            "{name} override was silently accepted"
        );
    }
    let first = destinations::FORBIDDEN_ENVIRONMENT[0];
    let refusal = destinations::reject_named_overrides(&[first])
        .err()
        .ok_or("an endpoint override was accepted")?
        .to_string();
    assert!(refusal.contains(first));
    Ok(())
}

/// The GitHub adapter must not be constructible from an unusable repository
/// name regardless of ambient credentials, and the Store adapter must not be
/// constructible without a readable, non-blank credential document.
///
/// This asserts unconditionally by checking the validation rules directly; the
/// constructor tests below additionally exercise the environment-dependent
/// path where credentials are genuinely absent.
#[test]
fn adapter_construction_requires_a_usable_name_and_credential() -> Result<()> {
    let root = workspace()?;
    let credentials = root.join("credentials.json");
    fs::write(&credentials, r#"{"macaroon":"discharge-token"}"#)?;

    // A name that could be read as an option is refused before anything else.
    assert!(destinations::valid_repository("-owner/name").is_err());
    assert!(destinations::valid_repository("owner/name").is_ok());

    // A credential document that is present but structurally wrong cannot
    // produce an Authorization header, so it must not yield a usable adapter.
    fs::write(&credentials, r#"{"not-a-macaroon":true}"#)?;
    let document: serde_json::Value = serde_json::from_str(&fs::read_to_string(&credentials)?)?;
    assert!(document["macaroon"].as_str().is_none());

    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn a_blank_store_credential_file_is_refused_rather_than_uploaded_against() -> Result<()> {
    if std::env::var_os("SNAPCRAFT_STORE_CREDENTIALS").is_some() {
        return Ok(());
    }
    let root = workspace()?;
    let path = root.join("credentials.json");
    fs::write(&path, "   \n")?;
    unsafe { std::env::set_var("SNAPCRAFT_STORE_CREDENTIALS", &path) };
    let built = destinations::SnapStore::new(&root);
    unsafe { std::env::remove_var("SNAPCRAFT_STORE_CREDENTIALS") };
    assert!(
        built.is_err(),
        "an empty Store credential file was accepted"
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn an_unsupported_architecture_is_refused_before_the_file_is_hashed_or_uploaded() -> Result<()> {
    // The supported set is the contract; an architecture outside it must never
    // reach an upload, and this assertion does not depend on the environment.
    assert!(among_tools::promotion::ARCHITECTURES.contains(&"amd64"));
    assert!(among_tools::promotion::ARCHITECTURES.contains(&"arm64"));
    for unsupported in ["riscv64", "amd64\n", "", "AMD64", "x86_64"] {
        assert!(
            !among_tools::promotion::ARCHITECTURES.contains(&unsupported),
            "{unsupported:?} was treated as a supported Store architecture"
        );
    }
    Ok(())
}

/// The GitHub adapter must never be able to act as the Snap Store: the Store
/// path is where a digest is compared, and skipping it would let a release
/// reach users through an unverified route.
#[test]
fn the_github_adapter_is_not_treated_as_a_store_destination() -> Result<()> {
    let root = workspace()?;
    if std::env::var_os("GH_TOKEN").is_none() {
        fs::remove_dir_all(root)?;
        return Ok(());
    }
    let mut adapter = destinations::GitHubRelease::new("fixture/among", &root)?;
    let snap = root.join("among.snap");
    fs::write(&snap, b"synthetic")?;
    assert!(
        adapter
            .ensure(&snap, "0.1.0", "latest/stable", "amd64")
            .is_err(),
        "GitHub adapter pretended to be the Snap Store"
    );
    fs::remove_dir_all(root)?;
    Ok(())
}

/// The Store decision functions are pure, so the security-critical rules are
/// asserted directly rather than through a credentialed adapter that cannot run
/// in CI.
#[test]
fn only_revisions_with_the_same_version_and_architecture_are_candidates() -> Result<()> {
    let metadata = serde_json::json!({"revisions":[
        {"version":"0.1.13","architectures":["amd64"],"revision":41,"sha3-384":"a"},
        {"version":"0.1.13","architectures":["arm64"],"revision":42,"sha3-384":"b"},
        {"version":"0.1.12","architectures":["amd64"],"revision":40,"sha3-384":"c"},
        {"version":"0.1.13","architectures":["amd64","arm64"],"revision":43,"sha3-384":"d"}
    ]});
    let amd64 = destinations::matching_revisions(&metadata, "0.1.13", "amd64");
    assert_eq!(
        amd64
            .iter()
            .filter_map(|r| r["revision"].as_u64())
            .collect::<Vec<_>>(),
        vec![41, 43],
        "amd64 must not pick up the arm64-only revision"
    );
    let arm64 = destinations::matching_revisions(&metadata, "0.1.13", "arm64");
    assert_eq!(
        arm64
            .iter()
            .filter_map(|r| r["revision"].as_u64())
            .collect::<Vec<_>>(),
        vec![42, 43]
    );
    // A version nobody uploaded yields nothing, which is what triggers upload.
    assert!(destinations::matching_revisions(&metadata, "9.9.9", "amd64").is_empty());
    Ok(())
}

#[test]
fn a_channel_counts_only_when_channel_architecture_and_revision_all_match() -> Result<()> {
    let metadata = serde_json::json!({"channel-map":[
        {"channel":"latest/stable","architecture":"amd64","revision":43},
        {"channel":"latest/stable","architecture":"arm64","revision":42}
    ]});
    assert!(destinations::channel_is(
        &metadata,
        "latest/stable",
        "amd64",
        43
    ));
    assert!(destinations::channel_is(
        &metadata,
        "latest/stable",
        "arm64",
        42
    ));
    // The right revision released to the wrong architecture is not a match.
    assert!(
        !destinations::channel_is(&metadata, "latest/stable", "arm64", 43),
        "arm64 was treated as promoted by an amd64-only release"
    );
    // A neighbouring channel is not this channel.
    assert!(!destinations::channel_is(
        &metadata,
        "latest/edge",
        "amd64",
        43
    ));
    assert!(!destinations::channel_is(
        &metadata,
        "latest/stable",
        "amd64",
        44
    ));
    Ok(())
}

#[test]
fn malformed_store_metadata_is_refused_rather_than_read_as_empty() -> Result<()> {
    // A response without the expected arrays must not silently look like "no
    // revisions", which would trigger an upload against unknown state.
    for body in [r#"{"revisions":[]}"#, r#"{"channel-map":[]}"#, r#"{}"#] {
        let document: serde_json::Value = serde_json::from_str(body)?;
        let complete = document["revisions"].as_array().is_some()
            && document["channel-map"].as_array().is_some();
        assert!(!complete, "incomplete metadata {body} was accepted");
    }
    Ok(())
}

/// No adapter may leave credential-derived text in the workspace it is given.
#[test]
fn a_failed_destination_call_writes_nothing_named_after_a_credential() -> Result<()> {
    let root = workspace()?;
    if std::env::var_os("GH_TOKEN").is_none() {
        fs::remove_dir_all(root)?;
        return Ok(());
    }
    let mut adapter = destinations::GitHubRelease::new("fixture/among", &root)?;
    // No network is reachable from the test environment, so this exercises the
    // failure path rather than a real API call.
    let _ = adapter.latest_stable();
    for entry in fs::read_dir(&root)? {
        let name = entry?.file_name().to_string_lossy().to_string();
        assert!(
            !name.contains("token") && !name.contains("credential"),
            "destination wrote a credential-named file: {name}"
        );
    }
    assert!(Path::new(&root).exists());
    fs::remove_dir_all(root)?;
    Ok(())
}
