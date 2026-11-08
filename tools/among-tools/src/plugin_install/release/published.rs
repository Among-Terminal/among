//! One immutable published prerelease, shared by native and package acceptance.

use super::*;

fn read(root: &Path, name: &str) -> Result<Value> {
    storage::parse_json(&regular_bytes(
        &storage::safe_path(root, Path::new(name))?,
        1024 * 1024,
    )?)
}

pub fn pin(path: &Path) -> Result<Value> {
    let value = storage::parse_json(&regular_bytes(path, 16 * 1024)?)?;
    validate_pin(&value)?;
    Ok(value)
}

fn validate_pin(pin: &Value) -> Result<()> {
    if pin["repository_id"] != REPOSITORY
        || pin["publisher_id"] != PUBLISHER
        || number(pin, "release_id")? == 0
        || !observation::tag_name(text(pin, "tag")?)
        || !observation::hex(text(pin, "source_revision")?, 40)
        || !observation::hex(text(pin, "catalogue_sha256")?, 64)
    {
        return Err("published acceptance pin has invalid identity/tag/source/digest".into());
    }
    Ok(())
}

fn matches(pin: &Value, snapshot: &Value, binding: &Value) -> Result<()> {
    validate_pin(pin)?;
    if snapshot["prerelease"] != true
        || snapshot["id"] != pin["release_id"]
        || snapshot["tag"] != pin["tag"]
        || binding["commit"] != pin["source_revision"]
        || observation::asset(snapshot, "among-plugins.json")?["sha256"] != pin["catalogue_sha256"]
    {
        return Err(
            "published observation differs from the accepted release/tag/source/catalogue pin"
                .into(),
        );
    }
    Ok(())
}

fn unchanged(
    initial: (&Value, &Value, &Value),
    final_observation: (&Value, &Value, &Value),
) -> Result<()> {
    if initial != final_observation {
        return Err("published repository/release/assets/tag changed during preparation".into());
    }
    Ok(())
}

/// Download once for all CI consumers, retaining both complete API observations.
pub fn fetch(pin_path: &Path, destination: &Path) -> Result<PathBuf> {
    let pin = pin(pin_path)?;
    fs::DirBuilder::new().mode(0o700).create(destination)?;
    let root = destination.canonicalize()?;
    repository(&root, "repository.json")?;
    let endpoint = format!(
        "repositories/{REPOSITORY}/releases/{}",
        number(&pin, "release_id")?
    );
    let snapshot = observation::prerelease_snapshot(&metadata(&root, &endpoint, "latest.json")?)?;
    let (binding, chain) = resolve(&root, &snapshot, "initial")?;
    matches(&pin, &snapshot, &binding)?;
    let mut remaining = 128 * 1024 * 1024u64;
    for asset in snapshot["assets"]
        .as_array()
        .ok_or("release assets absent")?
    {
        remaining = remaining
            .checked_sub(number(asset, "size")?)
            .ok_or("published asset set exceeds its transfer budget")?;
        download(&root, asset, text(asset, "name")?, 32 * 1024 * 1024)?;
    }
    repository(&root, "repository-final.json")?;
    let final_snapshot =
        observation::prerelease_snapshot(&metadata(&root, &endpoint, "latest-final.json")?)?;
    let (final_binding, final_chain) = resolve(&root, &final_snapshot, "final")?;
    matches(&pin, &final_snapshot, &final_binding)?;
    unchanged(
        (&snapshot, &binding, &chain),
        (&final_snapshot, &final_binding, &final_chain),
    )?;
    // A receipt appears only after the second live observation agrees. Native
    // consumers reconstruct their paths and validate every archive themselves.
    storage::atomic_write(
        &root,
        Path::new("published-inputs.json"),
        &serde_json::to_vec_pretty(
            &json!({"pin":pin,"release_snapshot":snapshot,"release_binding":binding,"tag_chain":chain}),
        )?,
        0o600,
    )?;
    println!(
        "PASS downloaded exact immutable published prerelease assets: {}",
        root.display()
    );
    Ok(root)
}

fn observations(assets: &Path, pin: &Value) -> Result<(Value, Value, Value)> {
    observations_with(assets, pin, read)
}

fn observations_with(
    assets: &Path,
    pin: &Value,
    read: impl Fn(&Path, &str) -> Result<Value>,
) -> Result<(Value, Value, Value)> {
    for name in ["repository.json", "repository-final.json"] {
        check_repository(&read(assets, name)?)?;
    }
    let load = |root: &Path, _endpoint: &str, name: &str| read(root, name);
    let snapshot = observation::prerelease_snapshot(&read(assets, "latest.json")?)?;
    let (binding, chain) = resolve_with(assets, &snapshot, "initial", load)?;
    let final_snapshot = observation::prerelease_snapshot(&read(assets, "latest-final.json")?)?;
    let (final_binding, final_chain) = resolve_with(assets, &final_snapshot, "final", load)?;
    matches(pin, &snapshot, &binding)?;
    matches(pin, &final_snapshot, &final_binding)?;
    unchanged(
        (&snapshot, &binding, &chain),
        (&final_snapshot, &final_binding, &final_chain),
    )?;
    Ok((snapshot, binding, chain))
}

/// Reconstruct native prepared inputs from the required publisher observation.
/// No default-branch source or CI-built package can enter this path.
pub fn prepare(pin_path: &Path, assets: &Path, destination: &Path) -> Result<PathBuf> {
    let pin = pin(pin_path)?;
    let assets = assets.canonicalize()?;
    let receipt = read(&assets, "published-inputs.json")?;
    if receipt["pin"] != pin {
        return Err("downloaded published inputs use a different acceptance pin".into());
    }
    let (snapshot, binding, chain) = observations(&assets, &pin)?;
    if receipt["release_snapshot"] != snapshot
        || receipt["release_binding"] != binding
        || receipt["tag_chain"] != chain
    {
        return Err("published transfer receipt differs from its raw API observations".into());
    }
    fs::DirBuilder::new().mode(0o700).create(destination)?;
    let root = destination.canonicalize()?;
    let catalogue_asset = observation::asset(&snapshot, "among-plugins.json")?;
    let catalogue_bytes = checked_bytes(
        &assets.join("among-plugins.json"),
        number(catalogue_asset, "size")?,
        text(catalogue_asset, "sha256")?,
    )?;
    storage::atomic_write(
        &root,
        Path::new("among-plugins.json"),
        &catalogue_bytes,
        0o600,
    )?;
    let architecture = crate::platform::discover()?.architecture;
    let (mut inputs, catalogue) = assemble(
        &root,
        &architecture,
        &snapshot,
        storage::parse_json(&catalogue_bytes)?,
        (&binding, &chain),
        |asset, name, _maximum| {
            let bytes = checked_bytes(
                &assets.join(text(asset, "name")?),
                number(asset, "size")?,
                text(asset, "sha256")?,
            )?;
            storage::atomic_write(&root, Path::new(name), &bytes, 0o600)?;
            Ok(root.join(name))
        },
    )?;
    inputs["published_pin"] = pin;
    inputs["candidate"] = json!(false);
    observation::validate(&inputs, &catalogue, &architecture)?;
    for name in [
        "repository.json",
        "repository-final.json",
        "latest.json",
        "latest-final.json",
        "initial-ref.json",
        "initial-commit.json",
        "final-ref.json",
        "final-commit.json",
    ] {
        let bytes = regular_bytes(&assets.join(name), 1024 * 1024)?;
        storage::atomic_write(&root, Path::new(name), &bytes, 0o600)?;
    }
    for (depth, _) in chain
        .as_array()
        .ok_or("tag chain absent")?
        .iter()
        .take(
            chain
                .as_array()
                .ok_or("tag chain absent")?
                .len()
                .saturating_sub(1),
        )
        .enumerate()
    {
        for pass in ["initial", "final"] {
            let name = format!("{pass}-tag-{depth}.json");
            storage::atomic_write(
                &root,
                Path::new(&name),
                &regular_bytes(&assets.join(&name), 1024 * 1024)?,
                0o600,
            )?;
        }
    }
    seal(&root, &inputs)
}

/// Raw publication evidence travels with a package profile, without paths from
/// the producer or any claim of another live GitHub observation on the consumer.
pub fn publication_record(root: &Path) -> Result<Value> {
    let inputs = verify(root)?;
    let mut files = serde_json::Map::new();
    for name in [
        "repository.json",
        "repository-final.json",
        "latest.json",
        "latest-final.json",
        "initial-ref.json",
        "initial-commit.json",
        "final-ref.json",
        "final-commit.json",
    ] {
        files.insert(name.to_owned(), read(root, name)?);
    }
    for (depth, _) in inputs["tag_chain"]
        .as_array()
        .ok_or("tag chain absent")?
        .iter()
        .take(
            inputs["tag_chain"]
                .as_array()
                .ok_or("tag chain absent")?
                .len()
                .saturating_sub(1),
        )
        .enumerate()
    {
        for pass in ["initial", "final"] {
            let name = format!("{pass}-tag-{depth}.json");
            files.insert(name.clone(), read(root, &name)?);
        }
    }
    Ok(
        json!({"pin":inputs["published_pin"],"raw_api_observations":files,"scope":"producer live initial/final observations; consumer verifies downloaded bytes offline"}),
    )
}

pub fn validate_publication_record(record: &Value, inputs: &Value) -> Result<()> {
    if inputs["candidate"] != false || record["pin"] != inputs["published_pin"] {
        return Err("package publication record differs from its published selection".into());
    }
    let load = |_root: &Path, name: &str| {
        record["raw_api_observations"]
            .get(name)
            .cloned()
            .ok_or_else(|| "package publication record omits an API observation".into())
    };
    let (snapshot, binding, chain) = observations_with(Path::new("."), &record["pin"], load)?;
    if inputs["release_snapshot"] != snapshot
        || inputs["release_binding"] != binding
        || inputs["tag_chain"] != chain
    {
        return Err("package raw publication observations differ from its selected release".into());
    }
    Ok(())
}

pub fn verify(root: &Path) -> Result<Value> {
    let inputs = super::verify(root)?;
    if inputs["candidate"] != false {
        return Err("published prerelease acceptance refuses synthetic candidates".into());
    }
    matches(
        &inputs["published_pin"],
        &inputs["release_snapshot"],
        &inputs["release_binding"],
    )?;
    let (snapshot, binding, chain) = observations(root, &inputs["published_pin"])?;
    if inputs["release_snapshot"] != snapshot
        || inputs["release_binding"] != binding
        || inputs["tag_chain"] != chain
    {
        return Err("prepared published inputs differ from their original API observations".into());
    }
    Ok(inputs)
}

/// A local development catalogue contains exactly the original catalogue and
/// native package archives, with the release's original names and bytes.
pub fn export_catalogue(inputs: &Path, output: &Path) -> Result<()> {
    let prepared = verify(inputs)?;
    fs::DirBuilder::new().mode(0o700).create(output)?;
    let catalogue = catalogue(inputs, &prepared)?;
    let bytes = checked_bytes(
        &inputs.join("among-plugins.json"),
        number(&prepared, "catalogue_size")?,
        text(&prepared, "catalogue_sha256")?,
    )?;
    storage::atomic_write(output, Path::new("among-plugins.json"), &bytes, 0o600)?;
    for (version, artifact) in select(&catalogue, text(&prepared, "architecture")?)? {
        let id = text(&version["manifest"], "id")?;
        let bytes = checked_bytes(
            &inputs.join(format!("{id}.tar.gz")),
            number(artifact, "size")?,
            text(artifact, "sha256")?,
        )?;
        storage::atomic_write(output, Path::new(text(artifact, "asset")?), &bytes, 0o600)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accepted_pin() -> Value {
        json!({"repository_id":1431055733u64,"publisher_id":339567100u64,"release_id":434653882u64,"tag":"v0.2.0-rc.1","source_revision":"a".repeat(40),"catalogue_sha256":"b".repeat(64)})
    }

    fn api_release() -> Value {
        json!({"id":434653882u64,"tag_name":"v0.2.0-rc.1","draft":false,"prerelease":true,"immutable":true,"assets":[{"id":12,"name":"among-plugins.json","state":"uploaded","size":123,"digest":format!("sha256:{}", "b".repeat(64))}]})
    }

    fn save_api_observations(fixture: &fixture::Fixture) -> Result<()> {
        let snapshot = &fixture.inputs["release_snapshot"];
        let assets = snapshot["assets"].as_array().ok_or("assets absent")?.iter().map(|asset| json!({"id":asset["id"],"name":asset["name"],"size":asset["size"],"state":"uploaded","digest":format!("sha256:{}",asset["sha256"].as_str().unwrap_or(""))})).collect::<Vec<_>>();
        let api = json!({"id":snapshot["id"],"tag_name":snapshot["tag"],"assets":assets,"draft":false,"prerelease":true,"immutable":true});
        let repository = json!({"id":1431055733u64,"owner":{"id":339567100u64},"full_name":"Among-Terminal/among-plugins","private":false});
        for pass in ["initial", "final"] {
            fs::write(
                fixture.root.join(format!("{pass}-ref.json")),
                serde_json::to_vec(
                    &json!({"ref":format!("refs/tags/{}", snapshot["tag"].as_str().ok_or("tag absent")?),"object":{"type":"commit","sha":fixture.inputs["source_revision"]}}),
                )?,
            )?;
            fs::write(
                fixture.root.join(format!("{pass}-commit.json")),
                serde_json::to_vec(&json!({"sha":fixture.inputs["source_revision"]}))?,
            )?;
        }
        for name in ["latest.json", "latest-final.json"] {
            fs::write(fixture.root.join(name), serde_json::to_vec(&api)?)?;
        }
        for name in ["repository.json", "repository-final.json"] {
            fs::write(fixture.root.join(name), serde_json::to_vec(&repository)?)?;
        }
        Ok(())
    }

    fn published_fixture() -> Result<fixture::Fixture> {
        let mut fixture = fixture::Fixture::new()?;
        fixture.inputs["candidate"] = json!(false);
        fixture.inputs["release_snapshot"]["prerelease"] = json!(true);
        fixture.inputs["published_pin"] = json!({"repository_id":1431055733u64,"publisher_id":339567100u64,"release_id":900,"tag":"v0.2.0","source_revision":fixture.inputs["source_revision"],"catalogue_sha256":fixture.inputs["catalogue_sha256"]});
        fixture.save()?;
        save_api_observations(&fixture)?;
        Ok(fixture)
    }

    #[test]
    fn transferred_publication_requires_both_immutable_raw_observations_and_original_pin(
    ) -> Result<()> {
        let fixture = published_fixture()?;
        let original = publication_record(&fixture.root)?;
        validate_publication_record(&original, &fixture.inputs)?;
        for (file, field, value) in [
            ("latest.json", "prerelease", json!(false)),
            ("latest-final.json", "immutable", json!(false)),
            ("latest-final.json", "id", json!(901)),
            ("latest-final.json", "tag_name", json!("v0.2.0-rc.2")),
            ("final-commit.json", "sha", json!("c".repeat(40))),
            ("repository-final.json", "owner", json!({"id":1})),
        ] {
            let mut record = original.clone();
            record["raw_api_observations"][file][field] = value;
            assert!(
                validate_publication_record(&record, &fixture.inputs).is_err(),
                "accepted changed {file}/{field}"
            );
        }
        let mut record = original.clone();
        record["raw_api_observations"]
            .as_object_mut()
            .ok_or("raw observations absent")?
            .remove("latest-final.json");
        assert!(validate_publication_record(&record, &fixture.inputs).is_err());
        let mut record = original;
        record["pin"]["source_revision"] = json!("c".repeat(40));
        assert!(validate_publication_record(&record, &fixture.inputs).is_err());
        Ok(())
    }

    #[test]
    fn pin_checks_identity_tag_source_and_catalogue_against_real_api_fields() -> Result<()> {
        let pin = accepted_pin();
        let release = api_release();
        let snapshot = observation::prerelease_snapshot(&release)?;
        assert_eq!(
            snapshot,
            json!({"id":434653882u64,"tag":"v0.2.0-rc.1","prerelease":true,"assets":[{"id":12,"name":"among-plugins.json","size":123,"sha256":"b".repeat(64)}]})
        );
        let binding = json!({"tag":"v0.2.0-rc.1","object":"a".repeat(40),"commit":"a".repeat(40)});
        matches(&pin, &snapshot, &binding)?;
        for (key, wrong) in [
            ("release_id", json!(434653883)),
            ("tag", json!("v0.2.0-rc.2")),
            ("source_revision", json!("c".repeat(40))),
            ("catalogue_sha256", json!("d".repeat(64))),
            ("repository_id", json!(1)),
            ("publisher_id", json!(1)),
        ] {
            let mut changed = pin.clone();
            changed[key] = wrong;
            assert!(
                matches(&changed, &snapshot, &binding).is_err(),
                "accepted changed {key}"
            );
        }
        for (field, wrong) in [
            ("immutable", json!(false)),
            ("draft", json!(true)),
            ("prerelease", json!(false)),
            ("prerelease", Value::Null),
        ] {
            let mut changed = release.clone();
            changed[field] = wrong;
            assert!(
                observation::prerelease_snapshot(&changed).is_err(),
                "accepted wrong {field}"
            );
        }
        let mut changed = release;
        changed["assets"][0]["digest"] = json!("sha256:invalid");
        assert!(observation::prerelease_snapshot(&changed).is_err());
        assert!(check_repository(&json!({"id":1431055733u64,"owner":{"id":1},"private":false,"full_name":"Among-Terminal/among-plugins"})).is_err());
        Ok(())
    }

    #[test]
    fn final_observation_drift_cannot_be_sealed() -> Result<()> {
        let snapshot = observation::prerelease_snapshot(&api_release())?;
        let binding = json!({"tag":"v0.2.0-rc.1","object":"a".repeat(40),"commit":"a".repeat(40)});
        let chain = json!([["commit", "a".repeat(40)]]);
        unchanged((&snapshot, &binding, &chain), (&snapshot, &binding, &chain))?;
        let mut changed = snapshot.clone();
        changed["assets"][0]["id"] = json!(13);
        assert!(unchanged((&snapshot, &binding, &chain), (&changed, &binding, &chain)).is_err());
        let mut changed = binding.clone();
        changed["commit"] = json!("c".repeat(40));
        assert!(unchanged((&snapshot, &binding, &chain), (&snapshot, &changed, &chain)).is_err());
        assert!(unchanged(
            (&snapshot, &binding, &chain),
            (&snapshot, &binding, &json!([["commit", "c".repeat(40)]]))
        )
        .is_err());
        Ok(())
    }

    #[test]
    fn local_export_has_only_unchanged_catalogue_and_original_native_archive_names() -> Result<()> {
        let mut fixture = published_fixture()?;
        let output = fixture.root.join("local");
        export_catalogue(&fixture.root, &output)?;
        assert_eq!(fs::read_dir(&output)?.count(), 4);
        assert_eq!(
            fs::read(output.join("among-plugins.json"))?,
            fs::read(fixture.root.join("among-plugins.json"))?
        );
        for plugin in fixture.inputs["plugins"]
            .as_array()
            .ok_or("plugins absent")?
        {
            assert_eq!(
                fs::read(output.join(text(plugin, "asset")?))?,
                fs::read(text(plugin, "package")?)?
            );
        }
        assert!(!output.join("source.tar.gz").exists());
        assert!(!output.join("inputs.json").exists());
        assert!(export_catalogue(&fixture.root, &output).is_err());
        fs::remove_file(fixture.root.join("latest-final.json"))?;
        assert!(verify(&fixture.root).is_err());
        save_api_observations(&fixture)?;
        let package = fixture.root.join("clock.tar.gz");
        let replacement = fixture.root.join("linked-archive");
        fs::rename(&package, &replacement)?;
        std::os::unix::fs::symlink(&replacement, &package)?;
        assert!(export_catalogue(&fixture.root, &fixture.root.join("linked-export")).is_err());
        fs::remove_file(&package)?;
        fs::rename(replacement, package)?;
        fixture.inputs["candidate"] = json!(true);
        fixture.save()?;
        assert!(verify(&fixture.root).is_err());
        Ok(())
    }
}
