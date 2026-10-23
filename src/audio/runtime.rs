//! helper-only bundle discovery. Overrides stay in the short-lived decoder
//! process, never in the GUI or its host shells/plugins.

use gtk::prelude::*;
use std::{
    ffi::CString,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

struct RegistryDirectory(PathBuf);
impl Drop for RegistryDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[path = "../../data/audio/runtime-files.rs"]
mod files;

fn member(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    let path = root
        .join(relative)
        .canonicalize()
        .map_err(|_| format!("Packaged audio is missing {}", relative.display()))?;
    if !path.starts_with(root)
        || !path.is_file()
        || path
            .metadata()
            .map_err(|_| "Audio metadata unavailable")?
            .len()
            == 0
    {
        return Err(format!(
            "Packaged audio member is invalid: {}",
            relative.display()
        ));
    }
    Ok(path)
}

/// Helper-only: run before its GTK initialization and native workers:
/// GTK and GStreamer cache their environment-based search paths at initialization.
///
/// # Safety
/// The caller must exclude concurrent environment access for this startup-only
/// operation and must be the dedicated helper, which exits after decoding.
/// Overrides remain installed: initialization itself may start native threads,
/// so restoring environment variables afterwards would race those readers.
pub unsafe fn initialize_packaged(root: &Path) -> Result<(), String> {
    if gtk::is_initialized() {
        return Err("Packaged audio must initialize before GTK.".into());
    }
    let root = root
        .canonicalize()
        .map_err(|_| "Packaged audio root unavailable")?;
    let triplet = match std::env::consts::ARCH {
        "x86_64" => "x86_64-linux-gnu",
        "aarch64" => "aarch64-linux-gnu",
        _ => return Err("Packaged audio architecture unsupported".into()),
    };
    let native = PathBuf::from(format!("usr/lib/{triplet}"));
    let lib = if root
        .join(native.join("gtk-4.0/4.0.0/media/libmedia-gstreamer.so"))
        .is_file()
    {
        native
    } else {
        PathBuf::from("usr/lib")
    };
    let backend = member(
        &root,
        &lib.join("gtk-4.0/4.0.0/media/libmedia-gstreamer.so"),
    )?;
    let plugins = root.join(lib.join("gstreamer-1.0"));
    for name in files::MEMBERS {
        member(&root, &lib.join(name))?;
    }
    let scanner = member(
        &root,
        &lib.join("gstreamer1.0/gstreamer-1.0/gst-plugin-scanner"),
    )?;
    if scanner
        .metadata()
        .map_err(|_| "Audio scanner metadata unavailable")?
        .permissions()
        .mode()
        & 0o111
        == 0
    {
        return Err("Packaged audio plugin scanner is not executable.".into());
    }
    let gst = member(&root, &lib.join("libgstreamer-1.0.so.0"))?;
    // A fresh private registry prevents an existing host cache from admitting
    // native plugins even when this initialization restricts the scan path.
    let registry = RegistryDirectory(
        glib::mkdtemp(std::env::temp_dir().join("among-audio-registry-XXXXXX"))
            .ok_or("Could not create a private audio registry.")?,
    );
    let overrides = [
        ("GTK_PATH", root.join(lib.join("gtk-4.0")).into_os_string()),
        ("GTK_MEDIA", "gstreamer".into()),
        ("GST_PLUGIN_SYSTEM_PATH_1_0", plugins.into_os_string()),
        ("GST_PLUGIN_PATH_1_0", "".into()),
        ("GST_PLUGIN_SCANNER_1_0", scanner.into_os_string()),
        (
            "GST_REGISTRY_1_0",
            registry.0.join("registry.bin").into_os_string(),
        ),
        ("GST_REGISTRY_UPDATE", "yes".into()),
        ("GST_REGISTRY_FORK", "yes".into()),
    ];
    for (key, value) in &overrides {
        std::env::set_var(key, value);
    }
    (|| {
        let path = CString::new(gst.as_os_str().as_encoded_bytes())
            .map_err(|_| "Audio library path contains NUL")?;
        // Keep the native module alive for process lifetime: GTK's registered
        // extension and GStreamer worker functions retain pointers into it.
        let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
        if handle.is_null() {
            return Err("Packaged GStreamer dependencies could not load.".into());
        }
        let backend = CString::new(backend.as_os_str().as_encoded_bytes())
            .map_err(|_| "Audio backend path contains NUL")?;
        if unsafe { libc::dlopen(backend.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) }.is_null() {
            return Err("Packaged GTK media dependencies could not load.".into());
        }
        let init = unsafe { libc::dlsym(handle, c"gst_init_check".as_ptr()) };
        if init.is_null() {
            return Err("Packaged GStreamer initialization is unavailable.".into());
        }
        let init: unsafe extern "C" fn(
            *mut i32,
            *mut *mut *mut std::ffi::c_char,
            *mut *mut glib::ffi::GError,
        ) -> i32 = unsafe { std::mem::transmute(init) };
        let mut error = std::ptr::null_mut();
        let initialized = unsafe { init(std::ptr::null_mut(), std::ptr::null_mut(), &mut error) };
        if !error.is_null() {
            unsafe {
                glib::ffi::g_error_free(error);
            }
        }
        if initialized == 0 {
            return Err("Packaged GStreamer initialization failed.".into());
        }
        gtk::init().map_err(|_| "GTK could not initialize packaged media.".to_string())?;
        let media = gtk::MediaFile::new();
        if media.type_().name() != "GtkGstMediaFile" {
            return Err("Packaged GTK media backend did not register.".into());
        }
        Ok(())
    })()
}
