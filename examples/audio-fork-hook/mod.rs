//! trusted delays around the real fork, external probe only.
use std::{fs, path::PathBuf, time::Duration};

/// # Safety
/// forwards the platform fork ABI. Only the supervisor's parent return is parked;
/// the real child is never replaced by a simulated success or mock decoder.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fork() -> libc::pid_t {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let mode = if std::thread::current().name() == Some("among-audio-supervisor") {
        runtime
            .as_ref()
            .and_then(|root| fs::read_to_string(root.join("audio-fork-stall")).ok())
    } else {
        None
    };
    let symbol = unsafe { libc::dlsym(libc::RTLD_NEXT, c"fork".as_ptr()) };
    if symbol.is_null() {
        std::process::exit(124);
    }
    let native: unsafe extern "C" fn() -> libc::pid_t = unsafe { std::mem::transmute(symbol) };
    if mode.as_deref() == Some("before") {
        if let Some(root) = &runtime {
            let _ = fs::write(root.join("audio-fork-entered"), "before real fork");
            while !root.join("audio-fork-release").exists() {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    let pid = unsafe { native() };
    if pid > 0 && mode.is_some() {
        if let Some(root) = &runtime {
            let _ = fs::write(root.join("audio-fork-real"), pid.to_string());
            if mode.as_deref() == Some("after") {
                let _ = fs::write(
                    root.join("audio-fork-entered"),
                    format!("after real fork: child={pid}"),
                );
                while !root.join("audio-fork-release").exists() {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }
    pid
}
