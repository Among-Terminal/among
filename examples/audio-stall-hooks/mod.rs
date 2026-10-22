//! trusted call-site faults in an external probe only; never linked into Among.
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn guard() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if !Path::new("/.dockerenv").is_file()
        || ["/dev/snd", "/dev/dsp", "/dev/audio"]
            .iter()
            .any(|p| Path::new(p).exists())
    {
        return Err("stall probe requires an owned device-free container".into());
    }
    let runtime =
        PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").ok_or("private runtime missing")?);
    let socket = format!("unix:{}/pulse.sock", runtime.display());
    if std::env::var("PULSE_SERVER").as_deref() != Ok(socket.as_str()) {
        return Err("private audio socket missing".into());
    }
    let mut values = Vec::new();
    for args in [
        vec!["info"],
        vec!["list", "short", "sinks"],
        vec!["list", "short", "modules"],
    ] {
        let result = std::process::Command::new("/usr/bin/pactl")
            .args(args)
            .output()?;
        if !result.status.success() {
            return Err("private audio inspection failed".into());
        }
        values.push(String::from_utf8(result.stdout)?);
    }
    if !values[0]
        .lines()
        .any(|line| line == format!("Server String: {socket}"))
        || !values[0].contains("Default Sink: among_audit")
        || values[1].lines().count() != 1
        || !values[1].contains("among_audit\tmodule-null-sink.c")
        || values[2].lines().count() != 2
        || !values[2].contains("module-native-protocol-unix")
        || !values[2].contains("module-null-sink")
    {
        return Err("sole private null sink was not confirmed".into());
    }
    Ok(runtime)
}

fn inject(kind: &str) {
    let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) else {
        return;
    };
    let control = runtime.join("audio-native-stall");
    if fs::read_to_string(&control).ok().as_deref() != Some(kind) {
        return;
    }
    if fs::rename(&control, runtime.join("audio-native-stall-claimed")).is_err() {
        return;
    }
    if kind == "crash" {
        std::process::exit(42);
    }
    // exercise grouped descendant cleanup too. No PID read from this evidence
    // file is ever used to signal; the supervisor owns kernel process handles.
    let descendant = if kind == "late" {
        None
    } else {
        std::process::Command::new("/bin/sleep")
            .arg("60")
            .spawn()
            .ok()
    };
    let record = format!(
        "helper={} descendant={:?} call={kind}\n",
        std::process::id(),
        descendant.as_ref().map(std::process::Child::id)
    );
    let _ = fs::write(runtime.join("audio-native-stall-entered"), record);
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
    }
    loop {
        if kind == "late" && runtime.join("audio-native-stall-release").is_file() {
            return;
        }
        if kind == "late" {
            std::thread::sleep(std::time::Duration::from_millis(5));
            continue;
        }
        unsafe {
            libc::pause();
        }
    }
}

#[unsafe(no_mangle)]
/// # Safety
/// GTK supplies a live NUL-terminated filename on its owning thread.
pub unsafe extern "C" fn gtk_media_file_new_for_filename(
    filename: *const std::ffi::c_char,
) -> *mut gtk::ffi::GtkMediaFile {
    inject("construct");
    inject("crash");
    inject("late");
    let symbol =
        unsafe { libc::dlsym(libc::RTLD_NEXT, c"gtk_media_file_new_for_filename".as_ptr()) };
    if symbol.is_null() {
        std::process::exit(124);
    }
    let native: unsafe extern "C" fn(*const std::ffi::c_char) -> *mut gtk::ffi::GtkMediaFile =
        unsafe { std::mem::transmute(symbol) };
    unsafe { native(filename) }
}

#[unsafe(no_mangle)]
/// # Safety
/// GTK supplies a live MediaFile on its owning thread.
pub unsafe extern "C" fn gtk_media_file_clear(file: *mut gtk::ffi::GtkMediaFile) {
    if unsafe { gtk::ffi::gtk_media_stream_get_ended(file.cast()) } != 0 {
        inject("retire");
    }
    let symbol = unsafe { libc::dlsym(libc::RTLD_NEXT, c"gtk_media_file_clear".as_ptr()) };
    if symbol.is_null() {
        std::process::exit(124);
    }
    let native: unsafe extern "C" fn(*mut gtk::ffi::GtkMediaFile) =
        unsafe { std::mem::transmute(symbol) };
    unsafe { native(file) }
}

/// # Safety
/// forwards GTK's live stream on its owning thread; the marker observes whether
/// a late constructor reaches play, independently of supervisor notices.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gtk_media_stream_play(stream: *mut gtk::ffi::GtkMediaStream) {
    use std::io::Write;
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(runtime.join("audio-play-invocations"))
        {
            let _ = writeln!(file, "{}", std::process::id());
        }
    }
    let symbol = unsafe { libc::dlsym(libc::RTLD_NEXT, c"gtk_media_stream_play".as_ptr()) };
    if symbol.is_null() {
        std::process::exit(124);
    }
    let native: unsafe extern "C" fn(*mut gtk::ffi::GtkMediaStream) =
        unsafe { std::mem::transmute(symbol) };
    unsafe { native(stream) }
}

/// # Safety
/// forwards the platform kill ABI. Denial is confined to this trusted probe's
/// private fixture; this interposer is never linked into Among.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kill(pid: libc::pid_t, signal: libc::c_int) -> libc::c_int {
    if pid < 0
        && signal == libc::SIGKILL
        && std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .is_some_and(|root| root.join("audio-deny-kill").is_file())
    {
        unsafe {
            *libc::__errno_location() = libc::EPERM;
        }
        return -1;
    }
    let symbol = unsafe { libc::dlsym(libc::RTLD_NEXT, c"kill".as_ptr()) };
    if symbol.is_null() {
        std::process::exit(124);
    }
    let native: unsafe extern "C" fn(libc::pid_t, libc::c_int) -> libc::c_int =
        unsafe { std::mem::transmute(symbol) };
    unsafe { native(pid, signal) }
}
