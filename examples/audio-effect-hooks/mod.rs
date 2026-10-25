//! external probe barrier only; never linked into the application.
use std::{fs, path::PathBuf, time::Duration};

/// # Safety
/// forwards waitid's platform ABI and the caller-owned siginfo pointer. Only the
/// known audio supervisor is parked; no received PID is used as signal authority.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn waitid(
    kind: libc::idtype_t,
    id: libc::id_t,
    info: *mut libc::siginfo_t,
    options: libc::c_int,
) -> libc::c_int {
    if std::thread::current().name() == Some("among-audio-supervisor") {
        if let Some(root) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
            if fs::rename(
                root.join("audio-supervisor-park"),
                root.join("audio-supervisor-claimed"),
            )
            .is_ok()
            {
                if let Err(error) = fs::write(
                    root.join("audio-supervisor-parked"),
                    format!("supervisor={} owned_child={id}\n", std::process::id()),
                ) {
                    eprintln!("could not record probe barrier: {error}");
                }
                while !root.join("audio-supervisor-release").exists() {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }
    let symbol = unsafe { libc::dlsym(libc::RTLD_NEXT, c"waitid".as_ptr()) };
    if symbol.is_null() {
        std::process::exit(124);
    }
    let native: unsafe extern "C" fn(
        libc::idtype_t,
        libc::id_t,
        *mut libc::siginfo_t,
        libc::c_int,
    ) -> libc::c_int = unsafe { std::mem::transmute(symbol) };
    unsafe { native(kind, id, info, options) }
}
