use among_tools::{storage, Result};
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn actual_apprun_preserves_arguments_working_directory_and_host_loader_environment() -> Result<()> {
    let root = storage::private_directory(&std::env::temp_dir(), "among-apprun-test")?;
    fs::create_dir_all(root.join("usr/bin"))?;
    fs::copy(env!("CARGO_BIN_EXE_among-apprun"), root.join("AppRun"))?;
    fs::write(root.join("usr/bin/among"), b"#!/bin/sh\nprintf '%s\\n' \"$AMONG_APPDIR\" \"$LD_LIBRARY_PATH\" \"$GSETTINGS_SCHEMA_DIR\" \"$XDG_DATA_DIRS\" \"$PWD\" \"$@\"\n")?;
    fs::set_permissions(
        root.join("usr/bin/among"),
        fs::Permissions::from_mode(0o755),
    )?;
    let output = Command::new(root.join("AppRun"))
        .env_clear()
        .env("LD_LIBRARY_PATH", "/synthetic-host-libraries")
        .env("GSETTINGS_SCHEMA_DIR", "/synthetic-host-schemas")
        .env("XDG_DATA_DIRS", "/synthetic-host-data")
        .current_dir(&root)
        .args(["--example", "space and 中文"])
        .output()?;
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout)?
            .lines()
            .collect::<Vec<_>>(),
        vec![
            root.to_str().ok_or("fixture root is not UTF-8")?,
            "/synthetic-host-libraries",
            "/synthetic-host-schemas",
            "/synthetic-host-data",
            root.to_str().ok_or("fixture root is not UTF-8")?,
            "--example",
            "space and 中文"
        ]
    );
    fs::remove_file(root.join("usr/bin/among"))?;
    assert!(!Command::new(root.join("AppRun")).output()?.status.success());
    fs::remove_dir_all(root)?;
    Ok(())
}
