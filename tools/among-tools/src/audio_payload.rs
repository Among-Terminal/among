use crate::{storage, Result};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

#[path = "../../../data/audio/runtime-files.rs"]
mod files;
pub use files::MEMBERS;

pub fn require(lib: &Path) -> Result<()> {
    let root = lib.canonicalize()?;
    for relative in MEMBERS {
        let path = root
            .join(relative)
            .canonicalize()
            .map_err(|_| format!("audio payload lacks {relative}"))?;
        if !path.starts_with(&root) || !path.is_file() || fs::metadata(&path)?.len() == 0 {
            return Err(format!("audio payload member is invalid: {relative}").into());
        }
        if relative.ends_with("gst-plugin-scanner")
            && fs::metadata(path)?.permissions().mode() & 0o111 == 0
        {
            return Err("audio plugin scanner is not executable".into());
        }
    }
    Ok(())
}

/// Preserve distribution-relative module/helper paths, so dpkg attribution and
/// GTK/GStreamer discovery agree. linuxdeploy must patch their dependencies in
/// place, not flatten basenames into anonymous libraries.
pub fn stage(source_lib: &Path, destination_lib: &Path) -> Result<Vec<PathBuf>> {
    require(source_lib)?;
    let mut paths = Vec::new();
    for relative in MEMBERS {
        let source = source_lib.join(relative);
        let destination = storage::safe_path(destination_lib, Path::new(relative))?;
        // The GTK deployment plugin may already have copied media modules;
        // replace only these known members before our dependency/RUNPATH pass.
        fs::create_dir_all(destination.parent().ok_or("audio member parent absent")?)?;
        fs::copy(source, &destination)?;
        fs::set_permissions(
            &destination,
            fs::Permissions::from_mode(if relative.ends_with("gst-plugin-scanner") {
                0o755
            } else {
                0o644
            }),
        )?;
        paths.push(destination);
    }
    Ok(paths)
}
