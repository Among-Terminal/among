//! owned scheduler-pressure fixture, compiled only by retirement_tests.

use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

unsafe extern "C" {
    fn signal(signal: i32, handler: usize) -> usize;
    fn sched_setaffinity(pid: i32, size: usize, mask: *const u8) -> i32;
    fn setpriority(which: i32, who: u32, priority: i32) -> i32;
    fn syscall(number: std::ffi::c_long, ...) -> std::ffi::c_long;
}

fn prepare(cpu: usize) -> io::Result<()> {
    let mut mask = [0u8; 128];
    if cpu >= mask.len() * 8 {
        return Err(io::Error::other("invalid CPU"));
    }
    mask[cpu / 8] |= 1 << (cpu % 8);
    if unsafe { sched_setaffinity(0, mask.len(), mask.as_ptr()) } != 0
        || unsafe { setpriority(0, 0, 19) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn publish() -> io::Result<()> {
    println!(
        "{{\"type\":\"notify\",\"message\":\"{}\"}}",
        std::process::id()
    );
    io::stdout().flush()
}

fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let invalid = || io::Error::other("invalid retirement fixture arguments");
    let mode = args.get(1).ok_or_else(invalid)?;
    let root = args.get(2).ok_or_else(invalid)?;
    let cpu: usize = args
        .get(3)
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    unsafe {
        signal(15, 1);
    } // SIGTERM/SIG_IGN, only this owned fixture.
    if mode == "descendant" {
        // set affinity/priority on self, never on a numeric PID reported by a peer.
        prepare(cpu)?;
        publish()?;
    } else if mode == "descendant-thread" {
        let (send, receive) = std::sync::mpsc::sync_channel(0);
        std::thread::spawn(move || -> io::Result<()> {
            prepare(cpu)?;
            send.send(())
                .map_err(|_| io::Error::other("fixture gate closed"))?;
            publish()?;
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        });
        receive
            .recv()
            .map_err(|_| io::Error::other("fixture thread failed"))?;
        // SYS_exit ends this main thread only, leaving the owned worker alive.
        #[cfg(target_arch = "x86_64")]
        const EXIT: std::ffi::c_long = 60;
        #[cfg(target_arch = "aarch64")]
        const EXIT: std::ffi::c_long = 93;
        unsafe {
            syscall(EXIT, 0);
        }
        return Err(io::Error::last_os_error());
    } else {
        let child_mode = if mode.ends_with("-thread") {
            "descendant-thread"
        } else {
            "descendant"
        };
        let child = Command::new(std::env::current_exe()?)
            .args([child_mode, root, &cpu.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .spawn()?;
        // deliberately leave descendant retirement to Process, not this fixture.
        drop(child);
        println!("{{\"type\":\"hello\",\"major\":1,\"minor\":0}}");
        io::stdout().flush()?;
        if mode.starts_with("inherited") {
            let end = Instant::now() + Duration::from_secs(4);
            while !Path::new(root).join("release-leader").exists() {
                if Instant::now() >= end {
                    return Err(io::Error::other("fixture gate timeout"));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            return Ok(());
        }
    }
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}
