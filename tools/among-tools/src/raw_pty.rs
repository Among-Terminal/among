use crate::{storage, Result};
use nix::libc;
use std::fs::OpenOptions;
use std::io::{IsTerminal, Read, Write};
use std::mem::MaybeUninit;
use std::path::Path;
use std::time::{Duration, Instant};

struct RawMode(libc::termios);
impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = std::io::stdout().write_all(b"\x1b[?1000l\x1b[?1006l\x1b[?2004l");
        let _ = std::io::stdout().flush();
        // retain the exact pre-fixture PTY mode even on read/write failures.
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.0);
        }
    }
}

pub fn run(output: &Path, mode: &str) -> Result<()> {
    if !matches!(mode, "mouse" | "source" | "paste") {
        return Err("unknown raw PTY fixture mode".into());
    }
    let base = output.parent().ok_or("raw PTY output has no parent")?;
    let home = std::env::var_os("HOME").ok_or("raw PTY HOME missing")?;
    if Path::new(&home) != base.join("home")
        || !base
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("among-menu-"))
    {
        return Err("raw PTY fixture requires private menu HOME".into());
    }
    let name = output.file_name().ok_or("raw PTY output name missing")?;
    if !matches!(
        name.to_str(),
        Some("source.bin" | "paste.bin" | "mouse.bin")
    ) {
        return Err("raw PTY output is not an approved fixture file".into());
    }
    storage::safe_path(base, Path::new(name))?;
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err("raw PTY fixture requires actual terminal descriptors".into());
    }
    let mut old = MaybeUninit::<libc::termios>::uninit();
    // tcgetattr initializes all fields on success; never inspect failed output.
    if unsafe { libc::tcgetattr(0, old.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let old = unsafe { old.assume_init() };
    let mut raw = old;
    unsafe {
        libc::cfmakeraw(&mut raw);
    }
    if unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let _restore = RawMode(old);
    let mut stdout = std::io::stdout();
    stdout.write_all(if mode == "mouse" {
        b"\x1b[?1000h\x1b[?1006h"
    } else {
        b"\x1b[?2004h"
    })?;
    if mode == "source" {
        stdout.write_all(
            b"\x1b[3J\x1b[2J\x1b[Howned source first line\r\nowned source second line\r\n",
        )?;
    }
    stdout.flush()?;
    storage::atomic_write(base, Path::new(name), b"", 0o600)?;
    let mut file = OpenOptions::new().append(true).open(output)?;
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut buffer = [0u8; 4096];
    while Instant::now() < deadline {
        let mut descriptor = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut descriptor, 1, 200) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if ready == 0 {
            continue;
        }
        if descriptor.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
            return Err("raw PTY descriptor failed".into());
        }
        let count = std::io::stdin().read(&mut buffer)?;
        if count == 0 || buffer[..count].contains(&4) {
            break;
        }
        file.write_all(&buffer[..count])?;
    }
    file.sync_all()?;
    Ok(())
}
