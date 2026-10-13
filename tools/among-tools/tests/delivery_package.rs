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
