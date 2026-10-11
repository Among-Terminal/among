use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();

fn appimage_root(root: &Path, executable: &Path) -> Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("AppImage root: {error}"))?;
    let executable = executable
        .canonicalize()
        .map_err(|error| format!("AppImage executable: {error}"))?;
    let expected = root.join("usr/bin/among");
    if executable != expected {
        return Err("AppImage payload root does not contain this executable".into());
    }
    Ok(root)
}

pub fn initialize() -> Result<(), String> {
    let appimage = std::env::var_os("AMONG_APPDIR");
    // The launcher adds this only for the application. Cache it before GTK
    // threads start, then remove it so shells/plugins cannot mistake themselves
    // for this bundle. No LD_LIBRARY_PATH or GTK/XDG paths are exported.
    std::env::remove_var("AMONG_APPDIR");
    let root = if std::env::var("SNAP_NAME").as_deref() == Ok("among") {
        Some(PathBuf::from(
            std::env::var_os("SNAP").ok_or("Snap runtime did not provide its payload path")?,
        ))
    } else if let Some(root) = appimage {
        Some(appimage_root(
            Path::new(&root),
            &std::env::current_exe().map_err(|error| error.to_string())?,
        )?)
    } else {
        None
    };
    ROOT.set(root)
        .map_err(|_| "packaged runtime was initialized twice".into())
}

pub fn root() -> Option<&'static Path> {
    ROOT.get().and_then(|root| root.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn appimage_root_cannot_be_applied_to_a_host_executable() {
        let root = std::env::temp_dir().join(format!("among-appdir-{}", std::process::id()));
        std::fs::create_dir_all(root.join("usr/bin")).expect("fixture directory");
        let executable = root.join("usr/bin/among");
        std::fs::write(&executable, b"synthetic ELF stand-in for path test only")
            .expect("fixture file");
        assert_eq!(
            appimage_root(&root, &executable).expect("owned executable"),
            root.canonicalize().expect("fixture root")
        );
        assert!(appimage_root(&root, Path::new("/bin/sh")).is_err());
        std::fs::remove_file(&executable).expect("remove fixture binary");
        std::os::unix::fs::symlink("/bin/sh", &executable).expect("fixture symlink");
        assert!(appimage_root(&root, Path::new("/bin/sh")).is_err());
        std::fs::remove_dir_all(root).expect("remove owned fixture");
    }
}
