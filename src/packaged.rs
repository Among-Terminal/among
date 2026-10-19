use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
static APPIMAGE_DESKTOP: OnceLock<Option<String>> = OnceLock::new();

fn integrated_desktop(desktop: &Path, image: &Path) -> Option<String> {
    let stem = desktop.file_stem()?.to_str()?;
    let hash = stem
        .strip_prefix("appimagekit_")?
        .strip_suffix("-Among_Terminal")?;
    if desktop.extension()? != "desktop"
        || hash.len() != 32
        || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    let entry = glib::KeyFile::new();
    entry
        .load_from_file(desktop, glib::KeyFileFlags::NONE)
        .ok()?;
    if entry
        .string("Desktop Entry", "X-AppImage-Identifier")
        .ok()?
        .as_str()
        != hash
        || entry
            .string("Desktop Entry", "X-AppImage-Old-Icon")
            .ok()?
            .as_str()
            != "com.github.among-terminal"
    {
        return None;
    }
    let exec = entry.string("Desktop Entry", "Exec").ok()?;
    let command = glib::shell_parse_argv(exec.as_str()).ok()?;
    if command.len() != 1
        || Path::new(&command[0]).canonicalize().ok()? != image.canonicalize().ok()?
    {
        return None;
    }
    Some(stem.to_string())
}

pub fn apply_window_identity(window: &impl gtk::prelude::IsA<gtk::Window>) {
    use gtk::prelude::*;
    let Some(Some(id)) = APPIMAGE_DESKTOP.get() else {
        return;
    };
    let id = id.clone();
    let window: &gtk::Window = window.as_ref();
    // GTK sets its default surface ID while mapping the window; apply the
    // integration hint after that, including each remap of the same window.
    window.connect_map(move |window| {
        let Some(surface) = window.surface() else {
            return;
        };
        let Some(wayland) = glib::Type::from_name("GdkWaylandToplevel") else {
            return;
        };
        if !surface.type_().is_a(wayland) {
            return;
        }
        if std::env::var_os("AMONG_SELFCHECK").is_some() {
            eprintln!(
                "among: appimage_surface_id={id:?} backend={}",
                surface.type_().name()
            );
        }
        let Ok(id) = std::ffi::CString::new(id.as_str()) else {
            return;
        };
        // GTK's generic bindings omit this backend API. Change only the
        // Wayland surface hint, not GApplication's single-instance identity.
        unsafe extern "C" {
            fn gdk_wayland_toplevel_set_application_id(
                toplevel: *mut gtk::gdk::ffi::GdkToplevel,
                application_id: *const std::ffi::c_char,
            );
        }
        // the dynamic backend type above guarantees a live Wayland toplevel;
        // GTK copies the ID and neither pointer escapes this call.
        unsafe {
            gdk_wayland_toplevel_set_application_id(surface.as_ptr().cast(), id.as_ptr());
        }
    });
}

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
        let root = appimage_root(
            Path::new(&root),
            &std::env::current_exe().map_err(|error| error.to_string())?,
        )?;
        let desktop = std::env::var_os("GIO_LAUNCHED_DESKTOP_FILE");
        let image = std::env::var_os("APPIMAGE");
        let id = desktop.zip(image).and_then(|(desktop, image)| {
            integrated_desktop(Path::new(&desktop), Path::new(&image))
        });
        if std::env::var_os("AMONG_SELFCHECK").is_some() {
            eprintln!("among: appimage_desktop_context={id:?}");
        }
        // launch hints belong to this application, not its host shells/plugins.
        for name in ["GIO_LAUNCHED_DESKTOP_FILE", "GIO_LAUNCHED_DESKTOP_FILE_PID"] {
            std::env::remove_var(name);
        }
        APPIMAGE_DESKTOP
            .set(id)
            .map_err(|_| "AppImage desktop context was initialized twice")?;
        Some(root)
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
    fn only_a_matching_appimagelauncher_entry_can_supply_a_surface_id() {
        let root =
            std::env::temp_dir().join(format!("among-desktop-context-{}", std::process::id()));
        std::fs::create_dir(&root).expect("new owned fixture");
        let image = root.join("Among candidate.AppImage");
        std::fs::write(&image, b"fixture; never executed").expect("fixture image");
        let hash = "0123456789abcdef0123456789abcdef";
        let id = format!("appimagekit_{hash}-Among_Terminal");
        let desktop = root.join(format!("{id}.desktop"));
        let valid = format!("[Desktop Entry]\nType=Application\nName=Among Terminal\nExec=\"{}\"\nX-AppImage-Identifier={hash}\nX-AppImage-Old-Icon=com.github.among-terminal\n", image.display());
        std::fs::write(&desktop, &valid).expect("fixture entry");
        assert_eq!(integrated_desktop(&desktop, &image), Some(id));
        for invalid in [
            valid.replace(hash, "wrong-identifier"),
            valid.replace(
                "X-AppImage-Old-Icon=com.github.among-terminal",
                "X-AppImage-Old-Icon=other",
            ),
            valid.replace(
                &format!("\"{}\"", image.display()),
                "/bin/sh -c touch~not-executed",
            ),
            valid.replace(&format!("\"{}\"", image.display()), "/usr/bin/among"),
            valid.replace(".AppImage\"", ".AppImage\" --argument"),
        ] {
            std::fs::write(&desktop, invalid).expect("invalid fixture entry");
            assert_eq!(integrated_desktop(&desktop, &image), None);
        }
        assert_eq!(
            integrated_desktop(&root.join("com.github.among-terminal.desktop"), &image),
            None
        );
        std::fs::remove_dir_all(root).expect("remove owned fixture");
    }

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
