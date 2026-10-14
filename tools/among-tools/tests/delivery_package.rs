use among_tools::{
    delivery_package::{self, Inputs},
    source_delivery, source_snapshot, storage, Result,
};
use serde_json::json;
use std::fs;
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn streaming_bundle_is_deterministic_integrity_bound_and_rejects_changed_builder_inputs(
) -> Result<()> {
    let f = Fixture(storage::private_directory(
        &std::env::temp_dir(),
        "among-delivery-fixture",
    )?);
    let root = f.0.join("public");
    fs::create_dir(&root)?;
    for name in source_snapshot::FILES {
        let path = root.join(name);
        fs::create_dir_all(path.parent().ok_or("fixture parent absent")?)?;
        fs::write(path, format!("fixture public input {name}"))?;
    }
    let vendor = f.0.join("vendor");
    fs::create_dir(root.join("snap"))?;
    fs::write(
        root.join("snap/snapcraft.yaml"),
        b"fixture recipe; not a releasable Snap",
    )?;
    fs::create_dir_all(vendor.join("fixture"))?;
    fs::write(vendor.join("fixture/.cargo-checksum.json"), b"{}")?;
    let snapshot = f.0.join("snapshot.tar");
    let receipt = source_snapshot::snapshot(&root, &vendor, &snapshot)?;
    let snap = f.0.join("fixture.snap");
    fs::write(&snap, b"synthetic association fixture, not a real Snap")?;
    let native = f.0.join("native");
    let sources = f.0.join("sources");
    let metadata = f.0.join("metadata");
    let builder = f.0.join("builder");
    let prime = f.0.join("prime");
    for path in [&native, &sources, &metadata, &builder] {
        fs::create_dir(path)?;
        fs::write(path.join("fixture"), b"evidence")?;
    }
    fs::create_dir_all(prime.join("usr/share/common-licenses"))?;
    fs::write(
        prime.join("usr/share/common-licenses/MIT"),
        b"authentic fixture common-license bytes",
    )?;
    for name in [
        "Dockerfile",
        "runner.sh",
        "builder-python.txt",
        "builder-packages.txt",
    ] {
        fs::write(builder.join(name), format!("fixture {name}"))?;
    }
    fs::write(
        builder.join("source-snapshot.json"),
        serde_json::to_vec(&json!({"archive_sha256":receipt.archive_sha256}))?,
    )?;
    let instructions = f.0.join("instructions.md");
    fs::write(&instructions, b"synthetic rebuild instructions")?;
    let inputs = Inputs {
        snap: &snap,
        snapshot: &snapshot,
        native: &native,
        sources: &sources,
        metadata: &metadata,
        prime: &prime,
        builder: &builder,
        instructions: &instructions,
    };
    // Exercise serialization only. This explicitly synthetic trust record is
    // not passed through the authenticated production packaging entry point.
    let trust = json!({"fixture":true,"scope":"synthetic serialization test, no signature trust"});
    let one = f.0.join("one");
    let two = f.0.join("two");
    let first = delivery_package::write_bundle(&inputs, &one, &trust)?;
    let second = delivery_package::write_bundle(&inputs, &two, &trust)?;
    assert_eq!(first, second);
    assert_eq!(
        fs::read(one.join("among-source-materials.tar"))?,
        fs::read(two.join("among-source-materials.tar"))?
    );
    source_delivery::check(
        &one.join("source-association.json"),
        &one.join("among-source-materials.tar"),
        &snap,
    )?;
    assert!(delivery_package::write_bundle(&inputs, &one, &trust).is_err());
    fs::write(
        builder.join("source-snapshot.json"),
        b"{\"archive_sha256\":\"changed\"}",
    )?;
    let refused = f.0.join("refused");
    assert!(delivery_package::write_bundle(&inputs, &refused, &trust).is_err());
    assert!(!refused.exists());
    fs::remove_file(root.join("snap/snapcraft.yaml"))?;
    let missing = f.0.join("missing-recipe.tar");
    let receipt_without_recipe = source_snapshot::snapshot(&root, &vendor, &missing)?;
    fs::write(
        builder.join("source-snapshot.json"),
        serde_json::to_vec(&json!({"archive_sha256":receipt_without_recipe.archive_sha256}))?,
    )?;
    let missing_inputs = Inputs {
        snapshot: &missing,
        ..inputs
    };
    let error = delivery_package::write_bundle(&missing_inputs, &refused, &trust)
        .err()
        .ok_or("missing build recipe was accepted")?;
    assert!(error.to_string().contains("Snap build recipe"));
    assert!(!refused.exists());
    fs::write(
        builder.join("source-snapshot.json"),
        serde_json::to_vec(&json!({"archive_sha256":receipt.archive_sha256}))?,
    )?;
    std::os::unix::fs::symlink(&snap, native.join("escaping"))?;
    assert!(delivery_package::write_bundle(&inputs, &refused, &trust).is_err());
    assert!(!refused.exists());
    Ok(())
}

#[test]
fn actual_cli_refuses_missing_authentication_before_output_creation() -> Result<()> {
    let base = Fixture(storage::private_directory(
        &std::env::temp_dir(),
        "among-delivery-cli",
    )?);
    let snap = base.0.join("fixture.snap");
    let snapshot = base.0.join("source.tar");
    let output = base.0.join("must-not-exist");
    fs::write(&snap, b"fixture")?;
    fs::write(&snapshot, b"fixture")?;
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .env("AMONG_CHECK_ROOT", base.0.join("checks"))
        .arg("package-source-delivery")
        .arg(snap)
        .arg(snapshot)
        .arg(&base.0)
        .arg(&base.0)
        .arg(&base.0)
        .arg(&base.0)
        .arg(&base.0)
        .arg(&base.0)
        .arg(&output)
        .output()?;
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(!output.exists());
    Ok(())
}

/// The Snap members whose bytes the audited prime must match.
const PAYLOAD: [&str; 5] = [
    "bin/among",
    "usr/share/among/plugins/clock",
    "usr/share/among/plugins/system-status",
    "usr/share/among/plugins/pane-pwd",
    "meta/snap.yaml",
];

fn write_payload(root: &std::path::Path) -> Result<()> {
    for name in PAYLOAD {
        let path = root.join(name);
        fs::create_dir_all(path.parent().ok_or("payload parent absent")?)?;
        fs::write(path, format!("synthetic payload member {name}"))?;
    }
    Ok(())
}

/// Pack a directory into a real SquashFS image, the format a Snap is.
fn squash(directory: &std::path::Path, image: &std::path::Path) -> Result<()> {
    let output = std::process::Command::new("/usr/bin/mksquashfs")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .arg(directory)
        .arg(image)
        .args(["-noappend", "-quiet", "-no-progress"])
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string().into());
    }
    Ok(())
}

/// The association binds sources to the binary that was audited. The
/// comparison reads the members out of an actual SquashFS image, so a Snap
/// whose executable, plugin or metadata bytes differ from the audited prime,
/// or that lacks one of them, is refused, and a matching one passes.
#[test]
#[ignore = "builds SquashFS images with mksquashfs from squashfs-tools; package-build containers lack it, the Checks ignored-test runner runs it"]
fn snap_members_are_compared_with_the_audited_prime_bytes() -> Result<()> {
    let f = Fixture(storage::private_directory(
        &std::env::temp_dir(),
        "among-delivery-squashfs",
    )?);
    let payload = f.0.join("payload");
    let prime = f.0.join("prime");
    write_payload(&payload)?;
    write_payload(&prime)?;
    let snap = f.0.join("matching.snap");
    squash(&payload, &snap)?;
    delivery_package::compare_snap_payload(&snap, &prime, &f.0)?;

    fs::write(
        prime.join("usr/share/among/plugins/clock"),
        b"rebuilt plugin",
    )?;
    let refusal = delivery_package::compare_snap_payload(&snap, &prime, &f.0)
        .err()
        .ok_or("a changed plugin was accepted")?;
    assert_eq!(
        refusal.to_string(),
        "Snap differs from audited prime executable/metadata bytes"
    );
    write_payload(&prime)?;

    fs::remove_file(payload.join("meta/snap.yaml"))?;
    let incomplete = f.0.join("incomplete.snap");
    squash(&payload, &incomplete)?;
    let refusal = delivery_package::compare_snap_payload(&incomplete, &prime, &f.0)
        .err()
        .ok_or("a Snap without its metadata was accepted")?;
    assert_eq!(
        refusal.to_string(),
        "Snap differs from audited prime executable/metadata bytes"
    );

    fs::remove_file(prime.join("bin/among"))?;
    let refusal = delivery_package::compare_snap_payload(&snap, &prime, &f.0)
        .err()
        .ok_or("a prime without the executable was accepted")?;
    assert_eq!(refusal.to_string(), "missing regular native source member");
    Ok(())
}

/// Inputs for the packaging command with every directory present but none
/// of them authenticated.
struct Unauthenticated {
    fixture: Fixture,
}

impl Unauthenticated {
    fn new() -> Result<Self> {
        let fixture = Fixture(storage::private_directory(
            &std::env::temp_dir(),
            "among-delivery-refusal",
        )?);
        let root = &fixture.0;
        fs::write(root.join("fixture.snap"), b"synthetic Snap bytes")?;
        fs::write(root.join("source.tar"), b"synthetic snapshot bytes")?;
        fs::write(root.join("instructions.md"), b"synthetic instructions")?;
        for name in ["native", "sources", "metadata", "prime", "builder"] {
            fs::create_dir(root.join(name))?;
        }
        fs::write(
            root.join("native/inventory.json"),
            serde_json::to_vec(&json!({"schema":1,"packages":[]}))?,
        )?;
        fs::write(
            root.join("metadata/archive-keyring.gpg"),
            b"synthetic keyring that signs nothing",
        )?;
        fs::write(
            root.join("metadata/InRelease"),
            b"Origin: Ubuntu\nCodename: noble\nDate: Thu, 08 Oct 2026 23:00:00 +0000\nSHA256:\n",
        )?;
        fs::write(
            root.join("metadata/inventory.json"),
            serde_json::to_vec(
                &json!({"schema":1,"collected_at":"2026-10-09T00:00:00+00:00","indices":[{"file":"Packages","release":"InRelease","release_member":"main/Packages","role":"binary"}]}),
            )?,
        )?;
        Ok(Self { fixture })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.fixture.0.join(name)
    }

    /// Run the packaging command; return its refusal and whether the output
    /// directory exists afterwards.
    fn refusal(&self, snap: &str) -> Result<(String, bool)> {
        let output = self.path("delivery");
        let result = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
            .env_clear()
            .env("AMONG_CHECK_ROOT", self.path("checks"))
            .arg("package-source-delivery")
            .arg(self.path(snap))
            .arg(self.path("source.tar"))
            .arg(self.path("native"))
            .arg(self.path("sources"))
            .arg(self.path("metadata"))
            .arg(self.path("prime"))
            .arg(self.path("builder"))
            .arg(self.path("instructions.md"))
            .arg(&output)
            .output()?;
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        Ok((
            String::from_utf8(result.stderr)?.trim_end().to_string(),
            output.exists(),
        ))
    }
}

/// Packaging authenticates before anything is written: an output that
/// already exists, a linked Snap, a missing native inventory and repository
/// metadata that the real `gpgv` cannot verify against the pinned key each
/// stop the command with its own message and no new output.
#[test]
fn actual_cli_refuses_each_unauthenticated_input_before_creating_output() -> Result<()> {
    let inputs = Unauthenticated::new()?;
    assert_eq!(
        inputs.refusal("fixture.snap")?,
        (
            "among-tools: release is not signed by the pinned Ubuntu archive key".to_string(),
            false
        )
    );

    fs::create_dir(inputs.path("delivery"))?;
    assert_eq!(
        inputs.refusal("fixture.snap")?,
        (
            "among-tools: source delivery output must be new".to_string(),
            true
        )
    );
    assert_eq!(fs::read_dir(inputs.path("delivery"))?.count(), 0);
    fs::remove_dir(inputs.path("delivery"))?;

    std::os::unix::fs::symlink(inputs.path("fixture.snap"), inputs.path("linked.snap"))?;
    assert_eq!(
        inputs.refusal("linked.snap")?,
        (
            "among-tools: delivery input must be a regular non-symlink file".to_string(),
            false
        )
    );

    fs::remove_file(inputs.path("native/inventory.json"))?;
    assert_eq!(
        inputs.refusal("fixture.snap")?,
        (
            "among-tools: missing regular native source member".to_string(),
            false
        )
    );
    Ok(())
}
