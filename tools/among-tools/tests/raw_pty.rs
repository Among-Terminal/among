use among_tools::Result;
use nix::libc;
use std::fs::{self, File};
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

struct ChildGuard(Child, bool);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.1 {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(self.0.id() as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
            let _ = self.0.wait();
        }
    }
}

fn attributes(file: &File) -> Result<libc::termios> {
    let mut value = std::mem::MaybeUninit::uninit();
    if unsafe { libc::tcgetattr(file.as_raw_fd(), value.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { value.assume_init() })
}

#[test]
fn real_raw_pty_receives_exact_bracketed_bytes_and_restores_terminal_mode() -> Result<()> {
    let base = among_tools::storage::private_directory(&std::env::temp_dir(), "among-menu-raw")?;
    fs::create_dir(base.join("home"))?;
    let (mut master, mut slave) = (-1, -1);
    if unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    let before = attributes(&slave)?;
    let output = base.join("paste.bin");
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_among-tools"))
            .env_clear()
            .env("HOME", base.join("home"))
            .args([
                "raw-menu-pty",
                output.to_str().ok_or("output path not UTF-8")?,
                "paste",
            ])
            .stdin(slave.try_clone()?)
            .stdout(slave.try_clone()?)
            .stderr(slave.try_clone()?)
            .process_group(0)
            .spawn()?,
        false,
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !output.exists() {
        if Instant::now() >= deadline {
            return Err("raw fixture startup timeout".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let expected = b"\x1b[200~synthetic first line\rsynthetic second line\x1b[201~";
    master.write_all(expected)?;
    while fs::read(&output)? != expected {
        if Instant::now() >= deadline {
            return Err("raw fixture capture timeout".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    master.write_all(&[4])?;
    loop {
        if let Some(status) = child.0.try_wait()? {
            child.1 = true;
            assert!(status.success());
            break;
        }
        if Instant::now() >= deadline {
            return Err("raw fixture shutdown timeout".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // do not kill a reaped child's potentially reused process-group ID.
    drop(child);
    let after = attributes(&slave)?;
    assert_eq!(before.c_lflag, after.c_lflag);
    assert_eq!(before.c_iflag, after.c_iflag);
    assert_eq!(before.c_oflag, after.c_oflag);
    assert_eq!(before.c_cc, after.c_cc);
    fs::remove_file(output)?;
    fs::remove_dir(base.join("home"))?;
    fs::remove_dir(base)?;
    Ok(())
}
