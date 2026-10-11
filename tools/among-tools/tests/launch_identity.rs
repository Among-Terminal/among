use among_tools::{process::OwnedProcess, sessions::launch_path, storage, Result};
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn real_symlink_launcher_keeps_selected_argv_zero_and_rejects_nonexecutables() -> Result<()> {
    let f = Fixture(storage::private_directory(
        &std::env::temp_dir(),
        "among-launch",
    )?);
    let alias = f.0.join("selected-app");
    std::os::unix::fs::symlink("/bin/sh", &alias)?;
    let selected = launch_path(&alias)?;
    assert_eq!(selected, alias);
    assert_ne!(selected, alias.canonicalize()?);
    let log = f.0.join("argv-zero");
    let file = fs::File::create(&log)?;
    let mut child = OwnedProcess::spawn(
        &selected,
        &["-c", "printf '%s' \"$0\""],
        &BTreeMap::new(),
        &f.0,
        &file,
    )?;
    assert!(child.wait(Duration::from_secs(5))?.success());
    assert_eq!(
        fs::read_to_string(log)?,
        alias.to_str().ok_or("alias not UTF-8")?
    );
    let text = f.0.join("not-executable");
    fs::write(&text, b"fixture")?;
    fs::set_permissions(&text, fs::Permissions::from_mode(0o644))?;
    assert!(launch_path(&text).is_err());
    assert!(launch_path(&f.0).is_err());
    assert!(launch_path(&f.0.join("missing")).is_err());
    Ok(())
}
