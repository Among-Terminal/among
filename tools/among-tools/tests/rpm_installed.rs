use among_tools::{rpm_installed, storage, Result};
use serde_json::json;
use std::{fs, path::Path};

#[test]
fn candidate_binding_refuses_wrong_tag_source_target_architecture_and_bytes() -> Result<()> {
    let root = storage::private_directory(&std::env::temp_dir(), "among-rpm-receipt")?;
    let source = "a".repeat(40);
    let package = root.join("among-0.1.0~rc.3-1.fc43.x86_64.rpm");
    fs::write(&package, b"synthetic bytes; not an installable RPM")?;
    let record = json!({"schema":1,"tag":"v0.1.0-rc.3","source_sha":source,"target":"fedora43","architecture":"amd64",
        "filename":package.file_name().and_then(|name| name.to_str()).ok_or("fixture filename absent")?,"sha256":storage::sha256(fs::File::open(&package)?)?});
    rpm_installed::validate_record(
        &record,
        "v0.1.0-rc.3",
        &source,
        "fedora43",
        "amd64",
        &package,
    )?;
    for key in [
        "tag",
        "source_sha",
        "target",
        "architecture",
        "filename",
        "sha256",
    ] {
        let mut changed = record.clone();
        changed[key] = json!("wrong");
        assert!(rpm_installed::validate_record(
            &changed,
            "v0.1.0-rc.3",
            &source,
            "fedora43",
            "amd64",
            &package
        )
        .is_err());
    }
    fs::write(&package, b"changed package")?;
    assert!(rpm_installed::validate_record(
        &record,
        "v0.1.0-rc.3",
        &source,
        "fedora43",
        "amd64",
        &package
    )
    .is_err());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn local_install_checks_refuse_before_writing_evidence() -> Result<()> {
    let output = std::env::temp_dir().join(format!("among-rpm-no-install-{}", std::process::id()));
    assert!(rpm_installed::run(
        Path::new("/absent"),
        "fedora43",
        "amd64",
        Path::new("/absent"),
        &output
    )
    .is_err());
    assert!(!output.exists());
    let spec = output.with_extension("spec");
    assert!(among_tools::rpm::prepare_release(Path::new("/absent"), &spec).is_err());
    assert!(!spec.exists());
    Ok(())
}
