use among_tools::{plugin_capture, storage, Result};
use std::{fs, path::Path};

#[test]
fn native_preferences_extents_require_four_bounded_values() -> Result<()> {
    assert_eq!(
        plugin_capture::extents("_GTK_FRAME_EXTENTS(CARDINAL) = 10, 10, 12, 10")?,
        [10, 10, 12, 10]
    );
    for text in [
        "missing",
        "= 1,2,3",
        "= -1,2,3,4",
        "= 1001,2,3,4",
        "= 1,2,3,4,5",
    ] {
        assert!(plugin_capture::extents(text).is_err());
    }
    Ok(())
}

#[test]
fn actual_plugin_capture_refuses_unowned_display_before_creating_private_configuration(
) -> Result<()> {
    let root = storage::private_directory(&std::env::temp_dir(), "among-plugin-capture")?;
    let output = root.join("must-not-exist");
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .arg("capture-readme-plugins")
        .arg(&root)
        .arg(Path::new("/missing-app"))
        .arg(&root)
        .arg("on")
        .arg(&output)
        .output()?;
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(!output.exists());
    fs::remove_dir_all(root)?;
    Ok(())
}
