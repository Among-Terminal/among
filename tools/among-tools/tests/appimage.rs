use among_tools::{appimage, storage, Result};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

#[test]
fn staging_preserves_attribution_selects_launcher_and_refuses_wrong_native_architecture(
) -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let scratch = storage::private_directory(&std::env::temp_dir(), "among-appimage-stage")?;
    let binaries = scratch.join("binaries");
    fs::create_dir(&binaries)?;
    // Header-only synthetic fixtures exercise packaging, not runtime launch.
    let mut elf = [0u8; 20];
    elf[..6].copy_from_slice(b"\x7fELF\x02\x01");
    elf[18] = 62;
    for name in ["among", "clock", "system-status", "pane-pwd", "launcher"] {
        let path = binaries.join(name);
        fs::write(&path, elf)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    let destination = scratch.join("Among.AppDir");
    appimage::stage(
        &root,
        &binaries,
        &binaries.join("launcher"),
        "amd64",
        &destination,
    )?;
    assert_eq!(fs::read(destination.join("AppRun"))?, elf);
    appimage::verify_raster_icons(&destination)?;
    for size in [128u32, 256] {
        let path = destination.join(format!(
            "usr/share/icons/hicolor/{size}x{size}/apps/com.github.among-terminal.png"
        ));
        let bytes = fs::read(&path)?;
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&bytes[16..20], &size.to_be_bytes());
        assert_eq!(&bytes[20..24], &size.to_be_bytes());
        fs::remove_file(&path)?;
        assert!(appimage::verify_raster_icons(&destination).is_err());
        fs::write(&path, b"wrong raster")?;
        assert!(appimage::verify_raster_icons(&destination).is_err());
        fs::write(&path, bytes)?;
    }
    for path in [
        "com.github.among-terminal.svg",
        "usr/share/icons/hicolor/scalable/apps/com.github.among-terminal.svg",
    ] {
        assert_eq!(
            fs::read(destination.join(path))?,
            include_bytes!("../../../docs/media/logo-small.svg"),
            "the AppImage must carry the approved artwork at {path}"
        );
    }
    assert!(
        fs::read_to_string(destination.join("com.github.among-terminal.desktop"))?
            .contains("Exec=AppRun\n")
    );
    assert!(
        !fs::read_to_string(destination.join("com.github.among-terminal.desktop"))?
            .contains("Exec=/usr/bin/among")
    );
    assert_eq!(
        fs::read(destination.join("usr/share/doc/among-terminal/THIRD_PARTY/inventory.json"))?,
        fs::read(root.join("THIRD_PARTY/inventory.json"))?
    );
    assert!(destination
        .join("usr/share/among/plugins/system-status")
        .is_file());
    for file in ["among-tools", "among-uninstall"] {
        assert!(!destination.join(Path::new("usr/bin").join(file)).exists());
    }
    let refused = scratch.join("Wrong.AppDir");
    assert!(appimage::stage(
        &root,
        &binaries,
        &binaries.join("launcher"),
        "arm64",
        &refused
    )
    .is_err());
    assert!(!refused.exists());
    assert!(appimage::stage(
        &root,
        &binaries,
        &binaries.join("launcher"),
        "amd64",
        &destination
    )
    .is_err());
    fs::remove_dir_all(scratch)?;
    Ok(())
}
#[test]
fn helper_download_cli_refuses_unknown_architecture_before_creating_output(
) -> among_tools::Result<()> {
    let root = among_tools::storage::private_directory(
        &std::env::temp_dir(),
        "among-appimage-helper-cli",
    )?;
    let destination = root.join("must-not-exist");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .args([
            "fetch-appimage-tools",
            "universal",
            destination.to_str().ok_or("fixture path is not UTF-8")?,
        ])
        .output()?;
    let refused = !output.status.success()
        && String::from_utf8(output.stderr)?.contains("unsupported AppImage helper architecture")
        && !destination.exists();
    std::fs::remove_dir_all(root)?;
    assert!(refused);
    Ok(())
}
