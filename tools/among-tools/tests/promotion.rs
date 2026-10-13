use among_tools::{
    promotion::{self, Destinations},
    storage, Result,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// Local stand-in for the two publication destinations. It records every write
/// so ordering, refusal and retry can be observed without credentials or a
/// network, and it can be told to fail part way through.
struct Fake {
    releases: BTreeMap<String, Value>,
    published: Option<Value>,
    store: BTreeMap<(String, String), String>,
    attached: BTreeMap<String, BTreeMap<String, String>>,
    calls: Vec<String>,
    fail_arm64_once: bool,
    /// The rolling nightly success marker, or `None` when no nightly has fully
    /// succeeded. Deliberately separate from `published`: it advances only
    /// after both destinations succeeded.
    marker: Option<Value>,
}

impl Fake {
    fn new() -> Self {
        Fake {
            releases: BTreeMap::new(),
            published: None,
            store: BTreeMap::new(),
            attached: BTreeMap::new(),
            calls: Vec::new(),
            fail_arm64_once: false,
            marker: None,
        }
    }
}

impl Destinations for Fake {
    fn latest_stable(&mut self) -> Result<Option<Value>> {
        self.calls.push("latest_stable".into());
        Ok(self.published.clone())
    }

    fn marker(&mut self) -> Result<Option<Value>> {
        self.calls.push("marker".into());
        Ok(self.marker.clone())
    }

    fn roll(&mut self, record: &Value, assets: &BTreeMap<String, PathBuf>) -> Result<()> {
        self.calls.push("roll".into());
        // The marker records what was actually rolled, so a fake that "rolls"
        // something other than the record is visible to the test.
        let mut observed = BTreeMap::new();
        for (name, path) in assets {
            if !path.is_file() {
                return Err("roll received a missing artifact".into());
            }
            observed.insert(name.clone(), storage::sha256(fs::File::open(path)?)?);
        }
        if !observed.is_empty()
            && record["assets"]
                .as_object()
                .is_some_and(|declared| declared.len() != observed.len())
        {
            return Err("roll assets differ from the record".into());
        }
        self.marker = Some(record.clone());
        Ok(())
    }

    fn record(&mut self, tag: &str) -> Result<Option<Value>> {
        self.calls.push(format!("record:{tag}"));
        Ok(self.releases.get(tag).cloned())
    }

    fn prepare(&mut self, record: &Value, assets: &BTreeMap<String, PathBuf>) -> Result<()> {
        let tag = record["tag"].as_str().unwrap_or_default().to_string();
        let attached = self.attached.entry(tag.clone()).or_default();
        for (name, path) in assets {
            if !path.is_file() {
                return Err("prepare received a missing artifact".into());
            }
            let digest = storage::sha256(fs::File::open(path)?)?;
            // An asset already attached with these exact bytes is reconciled,
            // not re-uploaded. Conflicting bytes are an immutable conflict.
            match attached.get(name) {
                Some(existing) if existing != &digest => {
                    return Err("existing asset conflict".into());
                }
                Some(_) => {}
                None => {
                    self.calls.push(format!("upload:{name}"));
                    attached.insert(name.clone(), digest);
                }
            }
        }
        self.releases.insert(tag, record.clone());
        Ok(())
    }

    fn publish(&mut self, record: &Value) -> Result<()> {
        self.calls.push("publish".into());
        self.published = Some(record.clone());
        Ok(())
    }

    fn ensure(
        &mut self,
        path: &Path,
        version: &str,
        channel: &str,
        architecture: &str,
    ) -> Result<()> {
        if architecture == "arm64" && self.fail_arm64_once {
            self.fail_arm64_once = false;
            self.calls.push("ensure:arm64:failed".into());
            return Err("synthetic arm64 outage".into());
        }
        let digest = storage::sha256(fs::File::open(path)?)?;
        let key = (architecture.to_string(), channel.to_string());
        match self.store.get(&key) {
            Some(existing) if existing != &digest => {
                return Err("revision conflict".into());
            }
            Some(_) => {}
            None => {
                self.store.insert(key, digest);
                self.calls
                    .push(format!("ensure:{architecture}:{version}:{channel}"));
            }
        }
        Ok(())
    }
}

/// Build a complete, self-consistent two-architecture record with real files.
fn fixture() -> Result<(PathBuf, Value, Value, BTreeMap<String, PathBuf>)> {
    let root = storage::private_directory(&std::env::temp_dir(), "among-promotion")?;
    let mut assets = BTreeMap::new();
    let mut groups = serde_json::Map::new();
    let mut recorded = serde_json::Map::new();
    for architecture in ["amd64", "arm64"] {
        let snap = root.join(format!("among-{architecture}.snap"));
        let source = root.join(format!("sources-{architecture}.tar"));
        let association = root.join(format!("association-{architecture}.json"));
        // A real squashfs image is not required to exercise ordering, refusal
        // and retry; packaged identity is covered separately by the artifact
        // tests that do carry a real Snap.
        fs::write(&snap, format!("synthetic stable snap for {architecture}"))?;
        fs::write(
            &source,
            format!("synthetic source bundle for {architecture}"),
        )?;
        fs::write(
            &association,
            format!("synthetic association for {architecture}"),
        )?;
        for path in [&snap, &source, &association] {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("fixture filename is not UTF-8")?
                .to_string();
            recorded.insert(name.clone(), json!(storage::sha256(fs::File::open(path)?)?));
            assets.insert(name, path.clone());
        }
        let name = |path: &Path| -> Result<String> {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
                .ok_or_else(|| "fixture filename is not UTF-8".into())
        };
        groups.insert(
            architecture.to_string(),
            json!({
                "snap": name(&snap)?,
                "source": name(&source)?,
                "association": name(&association)?,
            }),
        );
    }
    let record = json!({
        "schema": 2,
        "tag": "v0.1.0",
        "version": "0.1.0",
        "source_sha": "a".repeat(40),
        "architectures": Value::Object(groups),
        "assets": Value::Object(recorded),
    });
    let mut checks = serde_json::Map::new();
    for gate in promotion::GATES {
        checks.insert(gate.to_string(), json!(true));
    }
    let gates = json!({
        "source_sha": record["source_sha"],
        "assets": record["assets"],
        "checks": Value::Object(checks.clone()),
        "architectures": {
            "amd64": Value::Object(checks.clone()),
            "arm64": Value::Object(checks),
        },
    });
    Ok((root, record, gates, assets))
}

fn run(
    root: &Path,
    record: &Value,
    gates: &Value,
    assets: &BTreeMap<String, PathBuf>,
    destinations: &mut Fake,
) -> Result<&'static str> {
    promotion::promote(
        record,
        gates,
        assets,
        destinations,
        true,
        root,
        "publication.lock",
    )
}

#[test]
fn partial_second_architecture_keeps_sources_public_and_retry_reconciles() -> Result<()> {
    let (root, record, gates, assets) = fixture()?;
    let mut fake = Fake::new();
    fake.fail_arm64_once = true;

    // The first run publishes the GitHub release and fails on arm64.
    assert!(run(&root, &record, &gates, &assets, &mut fake).is_err());
    // Sources must already be public; only amd64 reached the Store.
    assert_eq!(fake.published.as_ref(), Some(&record));
    assert!(fake.store.keys().any(|(arch, _)| arch == "amd64"));

    let before = fake.calls.clone();
    // The retry completes without re-uploading anything.
    assert_eq!(
        run(&root, &record, &gates, &assets, &mut fake)?,
        "published"
    );
    assert!(!fake
        .calls
        .iter()
        .skip(before.len())
        .any(|call| call.starts_with("upload:")));
    assert!(fake.store.keys().any(|(arch, _)| arch == "arm64"));

    // A third run is a no-op at both destinations.
    let settled = fake.calls.clone();
    assert_eq!(
        run(&root, &record, &gates, &assets, &mut fake)?,
        "published"
    );
    assert!(!fake
        .calls
        .iter()
        .skip(settled.len())
        .any(|call| call.starts_with("ensure:")));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn missing_approval_or_any_failed_gate_never_contacts_a_destination() -> Result<()> {
    let (root, record, gates, assets) = fixture()?;

    let mut fake = Fake::new();
    assert!(promotion::promote(
        &record,
        &gates,
        &assets,
        &mut fake,
        false,
        &root,
        "publication.lock",
    )
    .is_err());
    assert!(fake.calls.is_empty());

    for gate in promotion::GATES {
        let mut failed = gates.clone();
        failed["checks"][gate] = json!(false);
        for architecture in ["amd64", "arm64"] {
            failed["architectures"][architecture][gate] = json!(false);
        }
        fake.calls.clear();
        assert!(run(&root, &record, &failed, &assets, &mut fake).is_err());
        assert!(
            fake.calls.is_empty(),
            "a failed {gate} gate contacted a destination"
        );
    }
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn aliasing_missing_or_extra_architecture_artifacts_are_refused() -> Result<()> {
    let (root, record, _gates, _assets) = fixture()?;

    // One architecture borrowing the other's sources.
    let mut aliased = record.clone();
    aliased["architectures"]["arm64"]["source"] =
        record["architectures"]["amd64"]["source"].clone();
    assert!(promotion::artifact_groups(&aliased).is_err());

    // A Snap that is not in any architecture group.
    let mut extra = record.clone();
    extra["assets"]["extra.snap"] = json!("b".repeat(64));
    assert!(promotion::artifact_groups(&extra).is_err());

    // A missing architecture.
    let mut missing = record.clone();
    missing["architectures"]
        .as_object_mut()
        .ok_or("architectures absent")?
        .remove("arm64");
    assert!(promotion::artifact_groups(&missing).is_err());

    // The complete record resolves.
    let groups = promotion::artifact_groups(&record)?;
    assert_eq!(groups.len(), 2);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn immutable_conflicts_and_changed_bytes_refuse_before_any_write() -> Result<()> {
    let (root, record, gates, assets) = fixture()?;
    let mut fake = Fake::new();
    fake.published = Some(json!({
        "schema": 2, "tag": "v0.9.0", "version": "0.9.0",
        "source_sha": "a".repeat(40),
        "architectures": record["architectures"], "assets": record["assets"],
    }));
    // An older published release must not be replaced.
    assert!(run(&root, &record, &gates, &assets, &mut fake).is_err());
    assert!(!fake.calls.iter().any(|call| call.starts_with("upload:")));

    // The same tag with different content is an immutable conflict.
    let mut other = record.clone();
    other["source_sha"] = json!("b".repeat(40));
    let mut conflicting = Fake::new();
    conflicting.published = Some(other);
    assert!(run(&root, &record, &gates, &assets, &mut conflicting).is_err());
    assert!(!conflicting
        .calls
        .iter()
        .any(|call| call.starts_with("upload:")));

    // Bytes changed after the gates passed.
    let tampered = assets.clone();
    let target = tampered
        .keys()
        .find(|name| name.ends_with(".snap"))
        .cloned()
        .ok_or("fixture has no Snap")?;
    fs::write(&tampered[&target], b"changed after checks")?;
    let mut fresh = Fake::new();
    assert!(run(&root, &record, &gates, &tampered, &mut fresh).is_err());
    assert!(fresh.calls.is_empty());
    fs::remove_dir_all(root)?;
    Ok(())
}

/// Version components are numbers, so 0.10.0 follows 0.9.0. Comparing the
/// version text instead would refuse the genuine next release and accept a
/// rollback from 0.10.0 to 0.2.0.
#[test]
fn stable_versions_are_ordered_numerically_not_as_text() -> Result<()> {
    let (root, record, gates, assets) = fixture()?;
    let release = |tag: &str| {
        let mut release = record.clone();
        release["tag"] = json!(tag);
        release["version"] = json!(tag.trim_start_matches('v'));
        release
    };

    let mut forward = Fake::new();
    forward.published = Some(release("v0.9.0"));
    assert_eq!(
        run(&root, &release("v0.10.0"), &gates, &assets, &mut forward)?,
        "published"
    );

    let mut rollback = Fake::new();
    rollback.published = Some(release("v0.10.0"));
    let refused = run(&root, &release("v0.2.0"), &gates, &assets, &mut rollback);
    assert_eq!(
        refused.map_err(|error| error.to_string()),
        Err("stable version must increase".to_string()),
        "an older stable tag must not replace a newer published one"
    );
    assert!(!rollback
        .calls
        .iter()
        .any(|call| call.starts_with("upload:")));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn a_single_architecture_historical_record_cannot_authorize_publication() -> Result<()> {
    let (root, record, _gates, _assets) = fixture()?;
    let mut legacy = record.clone();
    legacy["architectures"]
        .as_object_mut()
        .ok_or("architectures absent")?
        .remove("arm64");
    legacy["schema"] = json!(1);
    legacy["architecture"] = json!("amd64");
    let mut fields = legacy.as_object().ok_or("record is not an object")?.clone();
    fields.remove("architectures");
    let legacy = Value::Object(fields);
    assert!(promotion::artifact_groups(&legacy).is_err());
    fs::remove_dir_all(root)?;
    Ok(())
}
