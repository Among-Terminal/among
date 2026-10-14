use among_tools::{release_policy, source_snapshot, stable_build, stable_source, storage, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

struct Fixture {
    base: PathBuf,
    record: Value,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}
impl Fixture {
    fn new() -> Result<Self> {
        let base = storage::private_directory(&std::env::temp_dir(), "among-stable-source")?;
        let mut f = Self {
            base,
            record: json!({}),
        };
        fs::create_dir(f.repo())?;
        for name in source_snapshot::FILES {
            let path = f.repo().join(name);
            fs::create_dir_all(path.parent().ok_or("fixture parent absent")?)?;
            fs::write(path, b"fixture\n")?;
        }
        for name in ["Cargo.toml", "examples/plugins/Cargo.toml"] {
            fs::write(f.repo().join(name), "[package]\nversion=\"0.1.0\"\n")?;
        }
        fs::write(
            f.repo().join("LICENSE"),
            b"\xffbinary license integrity fixture\n\n",
        )?;
        fs::create_dir(f.repo().join("snap"))?;
        fs::write(
            f.repo().join("snap/snapcraft.yaml"),
            "# keep comment\nname: among\nbase: core24\nconfinement: classic\ngrade: devel\n",
        )?;
        fs::write(f.repo().join(".secret"), b"synthetic private marker")?;
        f.git(&["init", "-q", "-b", "main"])?;
        f.git(&["config", "user.name", "Synthetic Fixture"])?;
        f.git(&["config", "user.email", "fixture@example.invalid"])?;
        f.git(&["add", "."])?;
        f.git(&[
            "-c",
            "commit.gpgSign=false",
            "commit",
            "-qm",
            "synthetic source",
        ])?;
        f.git(&["-c", "tag.gpgSign=false", "tag", "v0.1.0"])?;
        f.record =
            json!({"tag":"v0.1.0","version":"0.1.0","source_sha":f.git(&["rev-parse","HEAD"])?});
        stable_build::prepare(&f.repo(), &f.tree(), "v0.1.0")?;
        fs::create_dir_all(f.base.join("vendor/crate"))?;
        fs::write(
            f.base.join("vendor/crate/.cargo-checksum.json"),
            b"{\"files\":{},\"package\":null}",
        )?;
        f.snapshot()?;
        Ok(f)
    }
    fn repo(&self) -> PathBuf {
        self.base.join("repo")
    }
    fn tree(&self) -> PathBuf {
        self.base.join("build")
    }
    fn bundle(&self) -> PathBuf {
        self.base.join("bundle.tar")
    }
    fn git(&self, args: &[&str]) -> Result<String> {
        let output = std::process::Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .arg("-C")
            .arg(self.repo())
            .args(args)
            .output()?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).to_string().into());
        }
        Ok(String::from_utf8(output.stdout)?.trim().into())
    }
    fn snapshot(&self) -> Result<()> {
        let archive = self.base.join("original-source.tar");
        if archive.exists() {
            fs::remove_file(&archive)?;
        }
        source_snapshot::snapshot(&self.tree(), &self.base.join("vendor"), &archive)?;
        let files = BTreeMap::from([("original-source.tar".into(), fs::read(archive)?)]);
        fs::write(self.bundle(), storage::deterministic_tar(&files)?)?;
        Ok(())
    }
    fn check(&self) -> Result<Value> {
        stable_source::check(&self.repo(), &self.record, &self.bundle())
    }
    /// The refusal for the current record and bundle, as its message.
    fn refusal(&self) -> Result<String> {
        refusal(self.check())
    }
    /// The refusal for another record against the same repository and bundle.
    fn refusal_for(&self, record: &Value) -> Result<String> {
        refusal(stable_source::check(&self.repo(), record, &self.bundle()))
    }
    /// Commit the staged repository change and move the tag onto it, so the
    /// tag names a different tree from the one the build overlay was made of.
    fn retag(&mut self) -> Result<()> {
        self.git(&[
            "-c",
            "commit.gpgSign=false",
            "commit",
            "-qm",
            "synthetic change",
        ])?;
        self.git(&["-c", "tag.gpgSign=false", "tag", "-f", "v0.1.0"])?;
        self.record["source_sha"] = self.git(&["rev-parse", "HEAD"])?.into();
        Ok(())
    }
    /// Rebuild the stable tree and its bundle from the current working tree.
    fn rebuild(&self) -> Result<()> {
        fs::remove_dir_all(self.tree())?;
        stable_build::prepare(&self.repo(), &self.tree(), "v0.1.0")?;
        self.snapshot()
    }
    /// Edit the overlay record inside the build tree and rebuild the bundle.
    fn overlay(&self, edit: impl FnOnce(&mut Value)) -> Result<()> {
        let path = self.tree().join("snap/stable-build-input.json");
        let mut overlay = storage::parse_json(&fs::read(&path)?)?;
        edit(&mut overlay);
        fs::write(path, serde_json::to_vec(&overlay)?)?;
        self.snapshot()
    }
    /// Tag a different original recipe and bind the build tree to it
    /// consistently: the tagged recipe, the preserved original, the effective
    /// recipe and every hash the overlay records all agree. Whatever is then
    /// refused is refused for the recipes themselves.
    fn rebind_recipe(&mut self, original: &[u8], effective: &[u8]) -> Result<()> {
        fs::write(self.repo().join("snap/snapcraft.yaml"), original)?;
        self.git(&["add", "snap/snapcraft.yaml"])?;
        self.retag()?;
        fs::write(self.tree().join("snap/snapcraft.devel.yaml"), original)?;
        fs::write(self.tree().join("snap/snapcraft.yaml"), effective)?;
        let original = storage::sha256(original)?;
        let effective = storage::sha256(effective)?;
        self.overlay(|overlay| {
            overlay["original_inputs"]["snap/snapcraft.yaml"] = original.clone().into();
            overlay["original_recipe_sha256"] = original.into();
            overlay["effective_recipe_sha256"] = effective.into();
        })
    }
    /// The members of the original source archive the fixture built.
    fn original_members(&self) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut archive = tar::Archive::new(fs::File::open(self.base.join("original-source.tar"))?);
        let mut members = Vec::new();
        for entry in archive.entries()? {
            let mut entry = entry?;
            let name = entry.path_bytes().into_owned();
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut bytes)?;
            members.push((name, bytes));
        }
        Ok(members)
    }
    /// Replace the bundle with one holding exactly these members.
    fn write_bundle(&self, members: &[Member<'_>]) -> Result<()> {
        fs::write(self.bundle(), raw_tar(members)?)?;
        Ok(())
    }
    /// Replace the bundle with one whose original source archive holds
    /// exactly these members.
    fn write_original(&self, members: &[Member<'_>]) -> Result<()> {
        let inner = raw_tar(members)?;
        self.write_bundle(&[(b"original-source.tar", tar::EntryType::Regular, &inner)])
    }
}

fn refusal(outcome: Result<Value>) -> Result<String> {
    Ok(outcome.err().ok_or("the source was accepted")?.to_string())
}

/// A tar member as raw name bytes, entry type and content.
type Member<'a> = (&'a [u8], tar::EntryType, &'a [u8]);

/// A tar archive written from raw header fields, so a test can produce the
/// unsafe, non-UTF-8, duplicate and non-regular members a hostile archive
/// could carry and the project's own archive writers refuse to create.
fn raw_tar(members: &[Member<'_>]) -> Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, kind, data) in members {
        let mut header = tar::Header::new_gnu();
        header
            .as_old_mut()
            .name
            .get_mut(..name.len())
            .ok_or("member name too long")?
            .copy_from_slice(name);
        header.set_entry_type(*kind);
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        if *kind == tar::EntryType::Symlink {
            header.set_link_name("LICENSE")?;
        }
        header.set_cksum();
        builder.append(&header, *data)?;
    }
    Ok(builder.into_inner()?)
}

/// The original members with these substitutions applied by name.
fn substituted<'a>(
    members: &'a [(Vec<u8>, Vec<u8>)],
    name: &[u8],
    bytes: &'a [u8],
) -> Vec<Member<'a>> {
    members
        .iter()
        .map(|(member, data)| {
            let data = if member.as_slice() == name {
                bytes
            } else {
                data.as_slice()
            };
            (member.as_slice(), tar::EntryType::Regular, data)
        })
        .collect()
}

fn regular(members: &[(Vec<u8>, Vec<u8>)]) -> Vec<Member<'_>> {
    members
        .iter()
        .map(|(name, data)| (name.as_slice(), tar::EntryType::Regular, data.as_slice()))
        .collect()
}

#[test]
fn actual_cli_binds_streamed_original_archive_and_binary_git_blobs_to_tag() -> Result<()> {
    let f = Fixture::new()?;
    let result = f.check()?;
    assert_eq!(result["source_sha"], f.record["source_sha"]);
    assert_eq!(result["original_inputs"], source_snapshot::FILES.len() + 1);
    assert_eq!(
        release_policy::git_bytes(&f.repo(), &["show", "v0.1.0:LICENSE"])?,
        b"\xffbinary license integrity fixture\n\n"
    );
    let record = f.base.join("record.json");
    fs::write(&record, serde_json::to_vec(&f.record)?)?;
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .arg("check-stable-source")
        .arg(f.repo())
        .arg(&record)
        .arg(f.bundle())
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(storage::parse_json(&output.stdout)?, result);
    Ok(())
}

#[test]
fn unrelated_commit_and_self_consistent_untagged_sources_are_rejected() -> Result<()> {
    let f = Fixture::new()?;
    let mut changed = f.record.clone();
    changed["source_sha"] = "a".repeat(40).into();
    assert!(stable_source::check(&f.repo(), &changed, &f.bundle()).is_err());
    fs::write(f.tree().join("LICENSE"), b"different source")?;
    let path = f.tree().join("snap/stable-build-input.json");
    let mut overlay = storage::parse_json(&fs::read(&path)?)?;
    overlay["original_inputs"]["LICENSE"] = storage::sha256(b"different source".as_slice())?.into();
    fs::write(path, serde_json::to_vec(&overlay)?)?;
    f.snapshot()?;
    let refusal = f
        .check()
        .err()
        .ok_or("untagged source accepted")?
        .to_string();
    assert!(
        refusal.contains("tagged commit"),
        "refused for another reason: {refusal}"
    );
    Ok(())
}

#[test]
fn extra_regular_source_members_and_symlink_bundles_are_rejected() -> Result<()> {
    let f = Fixture::new()?;
    fs::create_dir(f.tree().join("src"))?;
    fs::write(f.tree().join("src/extra.rs"), b"not tagged")?;
    f.snapshot()?;
    assert!(f
        .check()
        .err()
        .ok_or("extra source accepted")?
        .to_string()
        .contains("unexpected or missing"));
    let link = f.base.join("linked.tar");
    std::os::unix::fs::symlink(f.bundle(), &link)?;
    assert!(stable_source::check(&f.repo(), &f.record, &link).is_err());
    Ok(())
}

/// Only a canonical stable tag carries stable promotion authority; a missing
/// tag, a bare version, a short or zero-padded version and a release
/// candidate are all refused before Git is consulted.
#[test]
fn a_missing_or_non_stable_tag_is_refused() -> Result<()> {
    let f = Fixture::new()?;
    let mut record = f.record.clone();
    record["tag"] = Value::Null;
    assert_eq!(f.refusal_for(&record)?, "stable source tag absent");
    for tag in ["0.1.0", "v0.1", "v01.0.0", "v0.1.0-rc.1", "nightly"] {
        record["tag"] = tag.into();
        assert_eq!(
            f.refusal_for(&record)?,
            "expected stable vMAJOR.MINOR.PATCH tag",
            "{tag}"
        );
    }
    Ok(())
}

/// The recorded version must be the tag's version, or a record could carry
/// one version's provenance under another's name.
#[test]
fn a_version_that_differs_from_the_tag_is_refused() -> Result<()> {
    let f = Fixture::new()?;
    let mut record = f.record.clone();
    record["version"] = "0.1.1".into();
    assert_eq!(
        f.refusal_for(&record)?,
        "stable source version differs from tag"
    );
    Ok(())
}

/// A tag the reviewed checkout does not have cannot be bound to anything.
#[test]
fn a_tag_absent_from_the_repository_is_refused() -> Result<()> {
    let f = Fixture::new()?;
    let mut record = f.record.clone();
    record["tag"] = "v0.2.0".into();
    record["version"] = "0.2.0".into();
    assert_eq!(
        f.refusal_for(&record)?,
        "required Git reference or ancestry is unavailable"
    );
    Ok(())
}

/// The record must name the commit the tag resolves to, and the bundle must
/// be a regular file, not a link to one.
#[test]
fn another_commit_or_a_linked_bundle_is_refused() -> Result<()> {
    let f = Fixture::new()?;
    let mut record = f.record.clone();
    record["source_sha"] = "a".repeat(40).into();
    assert_eq!(
        f.refusal_for(&record)?,
        "stable source does not resolve to tag commit"
    );
    let link = f.base.join("linked.tar");
    std::os::unix::fs::symlink(f.bundle(), &link)?;
    assert_eq!(
        refusal(stable_source::check(&f.repo(), &f.record, &link))?,
        "source delivery bundle must be a regular file"
    );
    Ok(())
}

/// Delivery bundle members must be safe relative paths, regular files and
/// unique, and the original source archive must be among them.
#[test]
fn unsafe_linked_duplicate_or_missing_bundle_members_are_refused() -> Result<()> {
    let f = Fixture::new()?;
    let original = fs::read(f.base.join("original-source.tar"))?;
    let regular_file = tar::EntryType::Regular;
    f.write_bundle(&[(b"../original-source.tar", regular_file, &original)])?;
    assert_eq!(
        f.refusal()?,
        "path must contain only relative normal components"
    );
    f.write_bundle(&[(b"notes.txt", tar::EntryType::Symlink, b"")])?;
    assert_eq!(
        f.refusal()?,
        "source delivery members must be unique regular files"
    );
    f.write_bundle(&[
        (b"notes.txt", regular_file, b"one"),
        (b"notes.txt", regular_file, b"two"),
    ])?;
    assert_eq!(
        f.refusal()?,
        "source delivery members must be unique regular files"
    );
    f.write_bundle(&[(b"notes.txt", regular_file, b"no original source")])?;
    assert_eq!(f.refusal()?, "original source archive missing");
    Ok(())
}

/// The original source archive is held to the same member rules, and its
/// inventory must exist and describe exactly its members and bytes.
#[test]
fn hostile_or_inconsistent_original_archives_are_refused() -> Result<()> {
    let f = Fixture::new()?;
    let members = f.original_members()?;
    let mut linked = regular(&members);
    linked.push((b"src/link", tar::EntryType::Symlink, b""));
    f.write_original(&linked)?;
    assert_eq!(
        f.refusal()?,
        "original source archive must contain only regular members"
    );
    let mut escaping = regular(&members);
    escaping.push((b"../escape", tar::EntryType::Regular, b"x"));
    f.write_original(&escaping)?;
    assert_eq!(
        f.refusal()?,
        "path must contain only relative normal components"
    );
    let mut undecodable = regular(&members);
    undecodable.push((b"src/\xff", tar::EntryType::Regular, b"x"));
    f.write_original(&undecodable)?;
    assert_eq!(f.refusal()?, "path is not UTF-8");
    let mut duplicated = regular(&members);
    duplicated.push((b"LICENSE", tar::EntryType::Regular, b"second copy"));
    f.write_original(&duplicated)?;
    assert_eq!(f.refusal()?, "duplicate original source archive member");
    let without_inventory = regular(&members)
        .into_iter()
        .filter(|(name, _, _)| *name != b"source-inventory.json")
        .collect::<Vec<_>>();
    f.write_original(&without_inventory)?;
    assert_eq!(f.refusal()?, "source inventory missing");
    f.write_original(&substituted(&members, b"LICENSE", b"changed bytes"))?;
    assert_eq!(
        f.refusal()?,
        "original source inventory differs from archived members or digests"
    );
    Ok(())
}

/// The stable overlay record must exist and name this schema, tag and
/// version; an overlay made for another release does not bind this one.
#[test]
fn a_missing_or_foreign_stable_overlay_is_refused() -> Result<()> {
    let f = Fixture::new()?;
    for (field, value) in [
        ("schema", json!(2)),
        ("tag", json!("v0.1.1")),
        ("version", json!("0.1.1")),
    ] {
        let saved = fs::read(f.tree().join("snap/stable-build-input.json"))?;
        f.overlay(|overlay| overlay[field] = value)?;
        assert_eq!(f.refusal()?, "stable overlay identity mismatch", "{field}");
        fs::write(f.tree().join("snap/stable-build-input.json"), saved)?;
    }
    fs::remove_file(f.tree().join("snap/stable-build-input.json"))?;
    f.snapshot()?;
    assert_eq!(f.refusal()?, "stable overlay input record missing");
    Ok(())
}

/// A tagged public input must be a regular file blob. A symbolic link or a
/// submodule under a published directory has no bytes the bundle can carry.
#[test]
fn a_tagged_link_or_submodule_among_public_inputs_is_refused() -> Result<()> {
    let mut f = Fixture::new()?;
    fs::create_dir(f.repo().join("src"))?;
    std::os::unix::fs::symlink("../LICENSE", f.repo().join("src/link"))?;
    f.git(&["add", "src/link"])?;
    f.retag()?;
    assert_eq!(
        f.refusal()?,
        "public Git source inputs must be regular files"
    );
    f.git(&["rm", "-q", "src/link"])?;
    let commit = f.record["source_sha"]
        .as_str()
        .ok_or("source absent")?
        .to_string();
    f.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{commit},src/module"),
    ])?;
    f.retag()?;
    assert_eq!(
        f.refusal()?,
        "public Git source inputs must be regular files"
    );
    Ok(())
}

/// A tag without one of the mandatory public files cannot have produced a
/// complete source delivery, whatever the build tree holds.
#[test]
fn a_tag_without_a_mandatory_public_file_is_refused() -> Result<()> {
    let mut f = Fixture::new()?;
    f.git(&["rm", "-q", "--cached", "NOTICE"])?;
    f.retag()?;
    assert_eq!(
        f.refusal()?,
        "tagged source is missing mandatory public inputs"
    );
    Ok(())
}

/// Archived bytes that differ from the tagged blob are refused even when
/// the overlay still records the tagged digest.
#[test]
fn archived_bytes_that_differ_from_the_tagged_blob_are_refused() -> Result<()> {
    let f = Fixture::new()?;
    fs::write(f.tree().join("LICENSE"), b"bytes the tag never had")?;
    f.snapshot()?;
    assert_eq!(f.refusal()?, "archived input differs from tagged source");
    Ok(())
}

/// The overlay's recipe hashes must describe the archived original and
/// effective recipes.
#[test]
fn recipe_hashes_that_do_not_match_the_archived_recipes_are_refused() -> Result<()> {
    let f = Fixture::new()?;
    let saved = fs::read(f.tree().join("snap/stable-build-input.json"))?;
    f.overlay(|overlay| overlay["original_recipe_sha256"] = "0".repeat(64).into())?;
    assert_eq!(f.refusal()?, "stable recipe hash mismatch");
    fs::write(f.tree().join("snap/stable-build-input.json"), saved)?;
    let effective = f.tree().join("snap/snapcraft.yaml");
    let mut changed = fs::read(&effective)?;
    changed.extend_from_slice(b"# changed after the overlay was recorded\n");
    fs::write(effective, changed)?;
    f.snapshot()?;
    assert_eq!(f.refusal()?, "stable recipe hash mismatch");
    Ok(())
}

/// A tag without a Snap recipe leaves the bundle without an effective
/// recipe, which is refused even though every other member is consistent.
#[test]
fn a_tag_without_a_snap_recipe_has_no_effective_recipe() -> Result<()> {
    let mut f = Fixture::new()?;
    f.git(&["rm", "-q", "--cached", "snap/snapcraft.yaml"])?;
    f.retag()?;
    fs::remove_file(f.tree().join("snap/snapcraft.yaml"))?;
    f.overlay(|overlay| {
        if let Some(inputs) = overlay["original_inputs"].as_object_mut() {
            inputs.remove("snap/snapcraft.yaml");
        }
    })?;
    assert_eq!(f.refusal()?, "effective recipe absent");
    Ok(())
}

/// With every hash consistent, the recipes themselves are still judged: the
/// original must be the core24 classic devel candidate, and the effective
/// recipe may differ from it only in grade.
#[test]
fn consistently_bound_but_unacceptable_recipes_are_refused() -> Result<()> {
    let mut f = Fixture::new()?;
    // The rebinding itself must not be what is refused.
    f.rebind_recipe(
        b"name: among\nbase: core24\nconfinement: classic\ngrade: devel\n",
        b"name: among\nbase: core24\nconfinement: classic\ngrade: stable\n",
    )?;
    f.check()?;
    f.rebind_recipe(
        b"name: among\nbase: core22\nconfinement: classic\ngrade: devel\n",
        b"name: among\nbase: core22\nconfinement: classic\ngrade: stable\n",
    )?;
    assert_eq!(
        f.refusal()?,
        "expected unchanged core24/classic devel candidate recipe"
    );
    f.rebind_recipe(
        b"name: among\nbase: core24\nconfinement: classic\ngrade: devel\n",
        b"name: among\nbase: core24\nconfinement: strict\ngrade: stable\n",
    )?;
    assert_eq!(
        f.refusal()?,
        "stable overlay changes more than effective grade"
    );
    f.rebind_recipe(b"\xffnot text\n", b"\xffnot text\n")?;
    assert!(f.refusal()?.contains("invalid utf-8"));
    Ok(())
}

/// Python bytecode caches are generated files, not source: a tagged
/// `__pycache__` directory is neither demanded from the bundle nor counted.
#[test]
fn tagged_bytecode_caches_are_not_source_inputs() -> Result<()> {
    let mut f = Fixture::new()?;
    fs::create_dir_all(f.repo().join("src/__pycache__"))?;
    fs::write(f.repo().join("src/__pycache__/module.pyc"), b"\x00cache")?;
    f.git(&["add", "-f", "src/__pycache__/module.pyc"])?;
    f.retag()?;
    f.rebuild()?;
    assert!(!f.tree().join("src/__pycache__").exists());
    assert_eq!(
        f.check()?["original_inputs"],
        source_snapshot::FILES.len() + 1
    );
    Ok(())
}
