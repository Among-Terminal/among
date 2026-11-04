// A hostile managed-plugin fixture. It attempts, on request, everything a
// confined plugin must not be able to do, and reports what each attempt did.
//
// Two ways to drive it, both through the same standard input and output a
// real plugin has:
//
// * line mode: every line is `<action> [argument...]`; the answer is one line
//   `<action> <outcome>`. Outcomes are `ok[:detail]`, `err:<ERRNO>` or, for an
//   action that kills the process, nothing at all (the caller sees the exit).
// * managed mode: a first line holding a `managed_hello` message switches to
//   the managed protocol. The hello is echoed; every `status` request is
//   answered with one fragment holding the outcomes of the actions listed in
//   the file `actions` in the working directory, joined by `;`.
//
// An action run without confinement is the positive control for the same
// action run under it: a denial only counts as evidence if the control
// succeeded in the same environment.
use serde_json::{json, Value};
use std::ffi::CString;
use std::io::{self, BufRead, Write};

fn errno_name(errno: i32) -> String {
    match errno {
        libc::EPERM => "EPERM".into(),
        libc::EACCES => "EACCES".into(),
        libc::ENOENT => "ENOENT".into(),
        libc::ENOSYS => "ENOSYS".into(),
        libc::EBADF => "EBADF".into(),
        libc::EINVAL => "EINVAL".into(),
        libc::ESRCH => "ESRCH".into(),
        libc::ECONNREFUSED => "ECONNREFUSED".into(),
        libc::EAFNOSUPPORT => "EAFNOSUPPORT".into(),
        other => format!("E{other}"),
    }
}

fn last_error() -> String {
    format!(
        "err:{}",
        errno_name(io::Error::last_os_error().raw_os_error().unwrap_or(0))
    )
}

fn status(result: i64) -> String {
    if result < 0 {
        last_error()
    } else {
        "ok".into()
    }
}

fn c(text: &str) -> CString {
    CString::new(text).expect("no NUL in arguments")
}

fn number(arguments: &[&str], index: usize) -> i64 {
    arguments
        .get(index)
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

fn address(port: u16) -> libc::sockaddr_in {
    let mut address: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    address.sin_family = libc::AF_INET as u16;
    address.sin_port = port.to_be();
    address.sin_addr.s_addr = u32::from_ne_bytes([127, 0, 0, 1]);
    address
}

fn tcp(port: u16) -> String {
    let socket = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    if socket < 0 {
        return last_error();
    }
    let address = address(port);
    let result = unsafe {
        libc::connect(
            socket,
            (&address as *const libc::sockaddr_in).cast(),
            std::mem::size_of::<libc::sockaddr_in>() as u32,
        )
    };
    let outcome = status(i64::from(result));
    unsafe { libc::close(socket) };
    outcome
}

fn udp(port: u16) -> String {
    let socket = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if socket < 0 {
        return last_error();
    }
    let address = address(port);
    let result = unsafe {
        libc::sendto(
            socket,
            b"x".as_ptr().cast(),
            1,
            0,
            (&address as *const libc::sockaddr_in).cast(),
            std::mem::size_of::<libc::sockaddr_in>() as u32,
        )
    };
    let outcome = status(result as i64);
    unsafe { libc::close(socket) };
    outcome
}

fn unix(path: &str) -> String {
    let socket = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if socket < 0 {
        return last_error();
    }
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as u16;
    for (slot, byte) in address.sun_path.iter_mut().zip(path.bytes()) {
        *slot = byte as libc::c_char;
    }
    let result = unsafe {
        libc::connect(
            socket,
            (&address as *const libc::sockaddr_un).cast(),
            std::mem::size_of::<libc::sockaddr_un>() as u32,
        )
    };
    let outcome = status(i64::from(result));
    unsafe { libc::close(socket) };
    outcome
}

fn socketpair() -> String {
    let mut fds = [0i32; 2];
    let result = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
    status(i64::from(result))
}

fn io_uring() -> String {
    let mut parameters = [0u8; 128];
    status(unsafe { libc::syscall(libc::SYS_io_uring_setup, 4u32, parameters.as_mut_ptr()) })
}

fn read_file(path: &str) -> String {
    match std::fs::read(path) {
        Ok(bytes) => format!("ok:{}", bytes.len()),
        Err(error) => format!("err:{}", errno_name(error.raw_os_error().unwrap_or(0))),
    }
}

fn write_file(path: &str) -> String {
    match std::fs::write(path, b"hostile") {
        Ok(()) => "ok".into(),
        Err(error) => format!("err:{}", errno_name(error.raw_os_error().unwrap_or(0))),
    }
}

/// A child of this process that only waits: a target that Yama's ptrace scope
/// also permits without confinement, so a denial is the filter's and not Yama's.
fn sleeping_child() -> libc::pid_t {
    match unsafe { libc::fork() } {
        // the child ends by itself: under confinement the parent cannot
        // signal it, and it must still be reapable.
        0 => {
            std::thread::sleep(std::time::Duration::from_millis(500));
            unsafe { libc::_exit(0) }
        }
        pid => pid,
    }
}

fn reap(pid: libc::pid_t) {
    unsafe {
        libc::kill(pid, libc::SIGKILL);
        libc::waitpid(pid, std::ptr::null_mut(), 0);
    }
}

fn ptrace_child() -> String {
    let pid = sleeping_child();
    let result = unsafe { libc::ptrace(libc::PTRACE_ATTACH, pid, 0, 0) };
    let outcome = status(result);
    reap(pid);
    outcome
}

fn pidfd_getfd_child() -> String {
    let pid = sleeping_child();
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    let outcome = if pidfd < 0 {
        last_error()
    } else {
        // descriptor 1 of the child is a copy of this process's stdout pipe.
        status(unsafe { libc::syscall(libc::SYS_pidfd_getfd, pidfd, 1, 0) })
    };
    reap(pid);
    outcome
}

fn kill(pid: i64, signal: i32) -> String {
    status(i64::from(unsafe { libc::kill(pid as libc::pid_t, signal) }))
}

fn prlimit(pid: i64) -> String {
    let limit = libc::rlimit {
        rlim_cur: 7,
        rlim_max: 7,
    };
    let result = unsafe {
        libc::prlimit(
            pid as libc::pid_t,
            libc::RLIMIT_NOFILE,
            &limit,
            std::ptr::null_mut(),
        )
    };
    status(i64::from(result))
}

fn affinity(pid: i64) -> String {
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    unsafe { libc::CPU_SET(0, &mut set) };
    status(i64::from(unsafe {
        libc::sched_setaffinity(
            pid as libc::pid_t,
            std::mem::size_of::<libc::cpu_set_t>(),
            &set,
        )
    }))
}

fn setpriority(which: u32, who: u32, value: i32) -> String {
    status(i64::from(unsafe { libc::setpriority(which, who, value) }))
}

fn ioprio(which: i32, who: i32) -> String {
    // IOPRIO_CLASS_BE (2) << 13 | level 4
    status(unsafe { libc::syscall(libc::SYS_ioprio_set, which, who, (2 << 13) | 4) })
}

fn chmod(path: &str) -> String {
    status(i64::from(unsafe { libc::chmod(c(path).as_ptr(), 0o666) }))
}

fn chown(path: &str) -> String {
    let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
    status(i64::from(unsafe {
        libc::chown(c(path).as_ptr(), uid, gid)
    }))
}

fn setxattr(path: &str) -> String {
    status(i64::from(unsafe {
        libc::setxattr(
            c(path).as_ptr(),
            c("user.among-hostile").as_ptr(),
            b"1".as_ptr().cast(),
            1,
            0,
        )
    }))
}

fn utimensat(path: &str) -> String {
    let times = [
        libc::timespec {
            tv_sec: 1,
            tv_nsec: 0,
        },
        libc::timespec {
            tv_sec: 1,
            tv_nsec: 0,
        },
    ];
    status(i64::from(unsafe {
        libc::utimensat(libc::AT_FDCWD, c(path).as_ptr(), times.as_ptr(), 0)
    }))
}

fn truncate(path: &str) -> String {
    status(i64::from(unsafe { libc::truncate(c(path).as_ptr(), 0) }))
}

fn descriptors() -> String {
    // every descriptor number below the soft limit, not a fixed range: the
    // contract covers all of them.
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) };
    let top = limit.rlim_cur.min(1 << 20) as i32;
    let open: Vec<String> = (0..top)
        .filter(|fd| unsafe { libc::fcntl(*fd, libc::F_GETFD) } >= 0)
        .map(|fd| fd.to_string())
        .collect();
    format!("ok:{}", open.join(","))
}

fn environment() -> String {
    let mut names: Vec<String> = std::env::vars_os()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();
    names.sort();
    format!("ok:{}", names.join(","))
}

fn data_dir() -> String {
    format!(
        "ok:{}",
        std::env::var("AMONG_PLUGIN_DATA_DIR").unwrap_or_default()
    )
}

#[cfg(target_arch = "x86_64")]
fn x32() -> String {
    // the x32 entry point of an x86-64 kernel: the same instruction with the
    // x32 bit set in the call number. without confinement a kernel without x32
    // support answers ENOSYS; the filter kills the process instead.
    let mut result: i64 = libc::SYS_getpid | 0x4000_0000;
    unsafe {
        std::arch::asm!("syscall", inout("rax") result, out("rcx") _, out("r11") _);
    }
    if result < 0 {
        format!("err:{}", errno_name((-result) as i32))
    } else {
        "ok".into()
    }
}

#[cfg(target_arch = "x86_64")]
fn compat32() -> String {
    // int 0x80 is the 32-bit entry point; call number 20 is getpid there.
    let mut result: i64 = 20;
    unsafe {
        std::arch::asm!("int 0x80", inout("rax") result, out("rcx") _, out("r11") _);
    }
    if result < 0 {
        format!("err:{}", errno_name((-result) as i32))
    } else {
        "ok".into()
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn x32() -> String {
    "unsupported:architecture".into()
}

#[cfg(not(target_arch = "x86_64"))]
fn compat32() -> String {
    "unsupported:architecture".into()
}

/// Forks a child that signals `pid` after this process has exited and writes
/// its outcome to `result_path`; the caller reads the file.
fn orphan_signal(pid: i64, result_path: &str) -> String {
    match unsafe { libc::fork() } {
        -1 => last_error(),
        0 => {
            std::thread::sleep(std::time::Duration::from_millis(300));
            let outcome = kill(pid, libc::SIGUSR1);
            let _ = std::fs::write(result_path, outcome);
            unsafe { libc::_exit(0) }
        }
        _ => "ok:forked".into(),
    }
}

fn run(line: &str) -> Option<String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let (action, arguments) = words.split_first()?;
    let argument = |index: usize| arguments.get(index).copied().unwrap_or("");
    let outcome = match *action {
        "tcp" => tcp(number(arguments, 0) as u16),
        "udp" => udp(number(arguments, 0) as u16),
        "unix" => unix(argument(0)),
        "socketpair" => socketpair(),
        "io_uring" => io_uring(),
        "read" => read_file(argument(0)),
        "write" => write_file(argument(0)),
        "ptrace_child" => ptrace_child(),
        "pidfd_getfd_child" => pidfd_getfd_child(),
        "kill" => kill(number(arguments, 0), libc::SIGUSR1),
        "orphan_signal" => orphan_signal(number(arguments, 0), argument(1)),
        "prlimit" => prlimit(number(arguments, 0)),
        "affinity" => affinity(number(arguments, 0)),
        "setpriority_process" => setpriority(0, number(arguments, 0) as u32, 1),
        "setpriority_group_self" => setpriority(1, 0, 1),
        "setpriority_user_self" => setpriority(2, 0, 1),
        "ioprio_other" => ioprio(1, number(arguments, 0) as i32),
        "chmod" => chmod(argument(0)),
        "chown" => chown(argument(0)),
        "setxattr" => setxattr(argument(0)),
        "utimensat" => utimensat(argument(0)),
        "truncate" => truncate(argument(0)),
        "descriptors" => descriptors(),
        "environment" => environment(),
        "data_dir" => data_dir(),
        "x32" => x32(),
        "compat32" => compat32(),
        "abort" => {
            unsafe { libc::abort() };
        }
        "pid" => format!("ok:{}", std::process::id()),
        _ => format!("unknown:{action}"),
    };
    Some(format!("{action} {outcome}"))
}

fn managed(first: Value, lines: impl Iterator<Item = io::Result<String>>) -> io::Result<()> {
    let mut output = io::stdout().lock();
    writeln!(output, "{first}")?;
    output.flush()?;
    for line in lines {
        let line = line?;
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match message["type"].as_str() {
            Some("shutdown") => break,
            Some("status") => {
                let actions = std::fs::read_to_string("actions").unwrap_or_default();
                let results: Vec<String> = actions.lines().filter_map(run).collect();
                writeln!(
                    output,
                    "{}",
                    json!({"type":"status","session":message["session"],
                        "generation":message["generation"],"request_id":message["request_id"],
                        "refresh_ms":0,"fragments":[{"text":results.join(";")}]})
                )?;
                output.flush()?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn main() -> io::Result<()> {
    let mut lines = io::stdin().lock().lines();
    while let Some(line) = lines.next() {
        let line = line?;
        if let Ok(message) = serde_json::from_str::<Value>(&line) {
            if message["type"] == "managed_hello" {
                return managed(message, lines);
            }
        }
        if line.trim() == "exit" {
            break;
        }
        if let Some(answer) = run(&line) {
            let mut output = io::stdout().lock();
            writeln!(output, "{answer}")?;
            output.flush()?;
        }
    }
    Ok(())
}
