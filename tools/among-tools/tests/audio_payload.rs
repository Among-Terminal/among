use among_tools::{audio_payload, storage, Result};
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::Path,
};

fn fixture(root: &Path) -> Result<()> {
    for relative in audio_payload::MEMBERS {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().ok_or("parent")?)?;
        fs::write(&path, b"synthetic module fixture; not playback evidence")?;
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(if relative.ends_with("gst-plugin-scanner") {
                0o755
            } else {
                0o644
            }),
        )?;
    }
    Ok(())
}

#[test]
fn every_backend_codec_and_scanner_is_required_and_staged_with_owned_paths() -> Result<()> {
    let root = storage::private_directory(&std::env::temp_dir(), "among-audio-payload")?;
    let source = root.join("source");
    let output = root.join("output");
    fs::create_dir(&source)?;
    fs::create_dir(&output)?;
    fixture(&source)?;
    audio_payload::require(&source)?;
    for relative in audio_payload::MEMBERS {
        let path = source.join(relative);
        fs::remove_file(&path)?;
        assert!(
            audio_payload::require(&source).is_err(),
            "missing {relative}"
        );
        fs::write(&path, b"")?;
        assert!(audio_payload::require(&source).is_err(), "empty {relative}");
        fs::remove_file(&path)?;
        symlink("/bin/true", &path)?;
        assert!(
            audio_payload::require(&source).is_err(),
            "escaping {relative}"
        );
        fs::remove_file(path)?;
        fixture(&source)?;
    }
    let paths = audio_payload::stage(&source, &output)?;
    assert_eq!(paths.len(), audio_payload::MEMBERS.len());
    audio_payload::require(&output)?;
    let scanner = output.join("gstreamer1.0/gstreamer-1.0/gst-plugin-scanner");
    fs::set_permissions(&scanner, fs::Permissions::from_mode(0o644))?;
    assert!(audio_payload::require(&output).is_err());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn distribution_recipes_explicitly_supply_dynamic_audio_requirements() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let deb = fs::read_to_string(root.join("packaging/debian/control"))?;
    let depends = deb
        .lines()
        .find(|line| line.starts_with("Depends:"))
        .ok_or("Depends missing")?;
    for package in ["libgtk-4-media-gstreamer", "gstreamer1.0-plugins-good"] {
        assert!(depends.split(',').any(|value| value.trim() == package));
    }
    let snap: serde_yaml_ng::Value =
        serde_yaml_ng::from_slice(&fs::read(root.join("snap/snapcraft.yaml"))?)?;
    let stage = snap["parts"]["among"]["stage-packages"]
        .as_sequence()
        .ok_or("stage packages missing")?;
    for package in [
        "libgtk-4-media-gstreamer",
        "gstreamer1.0-plugins-base",
        "gstreamer1.0-plugins-good",
    ] {
        assert!(stage.iter().any(|value| value.as_str() == Some(package)));
    }
    let rpm = fs::read_to_string(root.join("packaging/rpm/among.spec"))?;
    for package in ["gtk4 >= 4.12", "gstreamer1-plugins-good", "mesa-libGLES"] {
        assert!(rpm.lines().any(|line| line
            .strip_prefix("Requires:")
            .is_some_and(|value| value.trim() == package)));
    }
    Ok(())
}
