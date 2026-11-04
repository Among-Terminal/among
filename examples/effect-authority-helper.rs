//! unshipped exec fixture: all transport/mapping/preparation precedes the final
//! claim/authority scan. its one-byte effect uses an already-owned descriptor.

use among::notifications::{
    monotonic_now_ns, ProcessEffectAuthorization, MAX_EFFECT_BINDING, MAX_EFFECT_FDS,
};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicU32, Ordering};

fn invalid() -> io::Error {
    io::Error::other("invalid synthetic effect-helper input")
}
fn decode(value: &str) -> io::Result<Vec<u8>> {
    if value.len() > MAX_EFFECT_BINDING * 2 || !value.len().is_multiple_of(2) {
        return Err(invalid());
    }
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digit = |byte| match byte {
                b'0'..=b'9' => Ok(byte - b'0'),
                b'a'..=b'f' => Ok(byte - b'a' + 10),
                _ => Err(invalid()),
            };
            Ok(digit(pair[0])? * 16 + digit(pair[1])?)
        })
        .collect()
}
fn command(input: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    for _ in 0..32 {
        let mut byte = [0];
        if input.read(&mut byte)? == 0 {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(invalid())
            };
        }
        if byte[0] == b'\n' {
            return Ok(Some(bytes));
        }
        bytes.push(byte[0]);
    }
    Err(invalid())
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let first = args.next().ok_or_else(invalid)?;
    if first == "--verify-closed" {
        let expected = args.next().ok_or_else(invalid)?;
        if expected.len() > 512 || args.next().is_some() {
            return Err(invalid().into());
        }
        for record in expected.split(',').take(MAX_EFFECT_FDS + 1) {
            let fields: Vec<_> = record.split(':').collect();
            if fields.len() != 3 {
                return Err(invalid().into());
            }
            let fd: i32 = fields[0].parse()?;
            let device: u64 = fields[1].parse()?;
            let inode: u64 = fields[2].parse()?;
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } == 0 {
                let stat = unsafe { stat.assume_init() };
                if stat.st_dev == device && stat.st_ino == inode {
                    return Err(invalid().into());
                }
            }
        }
        println!("CLOSED");
        return Ok(());
    }
    let binding = decode(&first)?;
    let raw: Vec<i32> = args
        .next()
        .ok_or_else(invalid)?
        .split(',')
        .map(str::parse)
        .collect::<Result<_, _>>()?;
    let effect: i32 = args.next().ok_or_else(invalid)?.parse()?;
    if args.next().is_some()
        || raw.is_empty()
        || raw.len() > MAX_EFFECT_FDS
        || effect < 3
        || raw.contains(&effect)
    {
        return Err(invalid().into());
    }
    for (index, fd) in raw.iter().enumerate() {
        if *fd < 3 || raw[..index].contains(fd) || unsafe { libc::fcntl(*fd, libc::F_GETFD) } < 0 {
            return Err(invalid().into());
        }
    }
    if unsafe { libc::fcntl(effect, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let effect = unsafe { OwnedFd::from_raw_fd(effect) };
    let descriptors = raw
        .iter()
        .map(|fd| unsafe { OwnedFd::from_raw_fd(*fd) })
        .collect();
    let capability = ProcessEffectAuthorization::import_for_helper(&binding, descriptors)?;
    let mut scanner_binding = Vec::new();
    for fd in &raw {
        if unsafe { libc::fcntl(*fd, libc::F_GETFD) } & libc::FD_CLOEXEC == 0
            || unsafe { libc::fcntl(*fd, libc::F_GETFL) } & libc::O_ACCMODE != libc::O_RDONLY
        {
            return Err(invalid().into());
        }
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(*fd, stat.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        let stat = unsafe { stat.assume_init() };
        scanner_binding.push(format!("{fd}:{}:{}", stat.st_dev, stat.st_ino));
    }
    println!("READY {:?} read-only/cloexec", capability.job_identity());
    io::stdout().flush()?;
    let scanner_binding = scanner_binding.join(",");
    let claim = AtomicU32::new(0);
    let marker = [1u8];
    let mut input = io::stdin().lock();
    while let Some(command) = command(&mut input)? {
        match command.as_slice() {
            b"check" => println!(
                "CURRENT {}",
                capability.is_current_monotonic(monotonic_now_ns()?)
            ),
            b"claim" => {
                if claim
                    .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    println!("CLAIMED");
                } else {
                    println!("DENIED");
                }
            }
            b"play" | b"finish" => {
                let expected = if command == b"finish" { 1 } else { 0 };
                let claimed = claim
                    .compare_exchange(expected, 2, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok();
                if claimed && capability.validate_after_claim().is_ok() {
                    // this write IS the synthetic effect, not preparation for it.
                    if unsafe {
                        libc::write(effect.as_raw_fd(), marker.as_ptr().cast(), marker.len())
                    } != 1
                    {
                        return Err(io::Error::last_os_error().into());
                    }
                    println!("PLAYED");
                } else {
                    println!("DENIED");
                }
            }
            b"exec-check" => {
                let output = std::process::Command::new(std::env::current_exe()?)
                    .arg("--verify-closed")
                    .arg(&scanner_binding)
                    .output()?;
                if !output.status.success() || output.stdout != b"CLOSED\n" {
                    return Err(invalid().into());
                }
                println!("SCANNER CLOSED");
            }
            b"quit" => return Ok(()),
            _ => return Err(invalid().into()),
        }
        io::stdout().flush()?;
    }
    Ok(())
}
fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            println!("REFUSED {error}");
            std::process::ExitCode::from(2)
        }
    }
}
