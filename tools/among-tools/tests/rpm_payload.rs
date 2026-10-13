//! What a built RPM and its `Source0` archive actually contain, checked on
//! synthetic extracted payloads and tar listings.

use among_tools::{rpm_payload, storage, Result};
use std::fs;
use std::path::{Path, PathBuf};

const REQUIRES: &str = "libgtk-4.so.1()(64bit)\nlibadwaita-1.so.0()(64bit)\nlibrsvg2\nrpmlib(CompressedFileNames) <= 3.0.4-1\n";

fn write(root: &Path, relative: &str, bytes: &str) -> Result<()> {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().ok_or("fixture parent absent")?)?;
    fs::write(path, bytes)?;
    Ok(())
}

/// A source tree and a payload whose license materials match it exactly.
fn fixture() -> Result<(PathBuf, PathBuf)> {
    let base = storage::private_directory(&std::env::temp_dir(), "among-rpm-payload")?;
    let source = base.join("source");
    let payload = base.join("payload");
    let inventory = "{\"schema\":1,\"packages\":[{\"name\":\"gtk4\",\"version\":\"0.11.5\"}]}\n";
    for root in [&source, &payload.join(rpm_payload::LICENSE_DIRECTORY)] {
        write(root, "LICENSE", "Apache License\n")?;
        write(root, "NOTICE", "Among Terminal notice\n")?;
        write(root, "THIRD_PARTY/inventory.json", inventory)?;
        write(root, "THIRD_PARTY/texts/gtk4-0.11.5/LICENSE", "MIT\n")?;
    }
    write(&payload, "usr/bin/among", "ELF")?;
    Ok((source, payload))
}

#[test]
fn matching_notices_pass_and_native_libraries_are_reported_as_system() -> Result<()> {
    let (source, payload) = fixture()?;
    let report = rpm_payload::check_notices(&source, &payload, REQUIRES)?;
    assert_eq!(report["rust_components_noticed"], 1);
    assert_eq!(
        report["system_native_libraries"],
        serde_json::json!(["libgtk-4.so.1()(64bit)", "libadwaita-1.so.0()(64bit)"])
    );
    fs::remove_dir_all(source.parent().ok_or("fixture base absent")?)?;
    Ok(())
}

#[test]
fn missing_stale_extra_or_bundled_material_is_refused() -> Result<()> {
    let licenses = |payload: &Path| payload.join(rpm_payload::LICENSE_DIRECTORY);
    type Mutation = fn(&Path) -> std::io::Result<()>;
    let cases: [(&str, Mutation); 6] = [
        // The original NOTICE is a condition of the Apache license.
        ("missing NOTICE", |licenses| {
            fs::remove_file(licenses.join("NOTICE"))
        }),
        ("changed LICENSE", |licenses| {
            fs::write(licenses.join("LICENSE"), "other\n")
        }),
        // A text that no longer matches the locked graph's crate version.
        ("stale notice text", |licenses| {
            fs::write(
                licenses.join("THIRD_PARTY/texts/gtk4-0.11.5/LICENSE"),
                "old\n",
            )
        }),
        ("extra notice", |licenses| {
            fs::write(licenses.join("THIRD_PARTY/extra.txt"), "x\n")
        }),
        ("missing notice", |licenses| {
            fs::remove_file(licenses.join("THIRD_PARTY/texts/gtk4-0.11.5/LICENSE"))
        }),
        // A shared object shipped in the payload instead of required from Fedora.
        ("bundled library", |licenses| {
            let lib = licenses.join("../../../lib64");
            fs::create_dir_all(&lib)?;
            fs::write(lib.join("libvte-2.91-gtk4.so.0"), "ELF")
        }),
    ];
    for (name, mutate) in cases {
        let (source, payload) = fixture()?;
        mutate(&licenses(&payload))?;
        assert!(
            rpm_payload::check_notices(&source, &payload, REQUIRES).is_err(),
            "{name} was accepted"
        );
        fs::remove_dir_all(source.parent().ok_or("fixture base absent")?)?;
    }
    let (source, payload) = fixture()?;
    // No soname requirement at all means the libraries were not linked from
    // the system, so the system/shipped split cannot be stated.
    assert!(rpm_payload::check_notices(&source, &payload, "librsvg2\n").is_err());
    fs::remove_dir_all(source.parent().ok_or("fixture base absent")?)?;
    Ok(())
}

fn tracked() -> Vec<String> {
    [
        ".gitignore",
        ".github/workflows/rpm-release.yml",
        "Cargo.toml",
        "packaging/rpm/among.spec",
        "src/main.rs",
    ]
    .map(String::from)
    .to_vec()
}

#[test]
fn source_archive_is_exactly_the_tagged_tree_without_ci_and_vcs_files() -> Result<()> {
    let listing = "./\namong-0.1.0/\namong-0.1.0/Cargo.toml\namong-0.1.0/packaging/\namong-0.1.0/packaging/rpm/\namong-0.1.0/packaging/rpm/among.spec\namong-0.1.0/src/\namong-0.1.0/src/main.rs\n";
    assert_eq!(
        rpm_payload::check_source_listing(&tracked(), listing, "among-0.1.0")?,
        3
    );
    Ok(())
}

#[test]
fn untracked_private_missing_or_misplaced_source_members_are_refused() {
    let good =
        "among-0.1.0/Cargo.toml\namong-0.1.0/packaging/rpm/among.spec\namong-0.1.0/src/main.rs\n";
    for (name, listing) in [
        ("untracked file", format!("{good}among-0.1.0/notes.txt\n")),
        ("private notes", format!("{good}among-0.1.0/AGENTS.md\n")),
        (
            "private tree",
            format!("{good}among-0.1.0/.private/openspec/ROADMAP.md\n"),
        ),
        (
            "build output",
            format!("{good}among-0.1.0/target/release/among\n"),
        ),
        ("git metadata", format!("{good}among-0.1.0/.git/config\n")),
        (
            "missing spec",
            good.replace("among-0.1.0/packaging/rpm/among.spec\n", ""),
        ),
        (
            "wrong top directory",
            good.replace("among-0.1.0/src", "among-0.2.0/src"),
        ),
        (
            "duplicate member",
            format!("{good}among-0.1.0/src/main.rs\n"),
        ),
    ] {
        assert!(
            rpm_payload::check_source_listing(&tracked(), &listing, "among-0.1.0").is_err(),
            "{name} was accepted"
        );
    }
}

#[test]
fn the_shared_source_snapshot_carries_the_rpm_spec_and_its_release_workflow() -> Result<()> {
    // The source delivery built for a release is the snapshot of these inputs;
    // without the spec in it, the delivered sources could not rebuild the RPM.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("project root is not resolvable")?;
    let inputs = among_tools::source_snapshot::inputs(root, &[])?;
    for required in [
        "packaging/rpm/among.spec",
        ".github/workflows/rpm-release.yml",
        "LICENSE",
        "NOTICE",
        "THIRD_PARTY/inventory.json",
        "Cargo.lock",
        "examples/plugins/Cargo.lock",
    ] {
        assert!(
            inputs.contains_key(required),
            "{required} is not a source input"
        );
    }
    Ok(())
}
