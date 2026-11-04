//! what a confined managed plugin may do, as plain data. nothing here talks to
//! the kernel: the builders in the sibling modules turn this data into a
//! Landlock ruleset and a seccomp program, and the tests pin every entry.

use libc::c_long;

/// Landlock ABI from which the filesystem protection counts as full: it is the
/// first one that mediates `truncate(2)`, so a plugin cannot empty a file it
/// may only read.
pub const FULL_LANDLOCK_ABI: u32 = 3;

/// Directories and files a plugin may read and execute: the dynamic loader,
/// system libraries and the timezone database. Entries that do not exist on a
/// host (`/lib64` on arm64) are skipped when the ruleset is built.
pub const SYSTEM_READ: &[&str] = &[
    "/usr",
    "/lib",
    "/lib64",
    "/etc/ld.so.cache",
    "/etc/localtime",
];

/// Read-only files added by the `system.metrics` feature. Never a directory:
/// `/proc/<pid>/environ` of a user's shell can hold tokens.
pub const SYSTEM_METRICS_READ: &[&str] = &["/proc/stat", "/proc/meminfo", "/proc/loadavg"];

/// The manifest feature that adds [`SYSTEM_METRICS_READ`].
pub const SYSTEM_METRICS_FEATURE: &str = "system.metrics";

/// Files a plugin may additionally read because of the features it was granted.
pub fn grant_reads(features: &std::collections::BTreeSet<String>) -> Vec<&'static str> {
    let mut paths = Vec::new();
    if features.contains(SYSTEM_METRICS_FEATURE) {
        paths.extend_from_slice(SYSTEM_METRICS_READ);
    }
    paths
}

/// One argument comparison: the 32-bit value the kernel reads from the
/// argument (`int` and `pid_t` parameters ignore the upper half of the
/// register, so the filter must too).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArgEquals {
    pub index: u8,
    pub value: u32,
}

/// A system call the filter refuses with `EPERM`, unless every comparison in
/// `allowed_when` holds (an empty list refuses always).
#[derive(Clone, Copy, Debug)]
pub struct Denied {
    pub name: &'static str,
    pub number: c_long,
    pub allowed_when: &'static [ArgEquals],
}

const fn always(name: &'static str, number: c_long) -> Denied {
    Denied {
        name,
        number,
        allowed_when: &[],
    }
}

const fn only_self(name: &'static str, number: c_long) -> Denied {
    Denied {
        name,
        number,
        allowed_when: &[ArgEquals { index: 0, value: 0 }],
    }
}

// `PRIO_PROCESS` and `IOPRIO_WHO_PROCESS`: with `who == 0` both mean "the
// calling process". `PRIO_PGRP` and `PRIO_USER` with `who == 0` reach the
// caller's whole process group or every process of its user.
const PRIO_PROCESS: u32 = 0;
const IOPRIO_WHO_PROCESS: u32 = 1;

// calls that exist on every architecture the host supports.
const COMMON: &[Denied] = &[
    // sockets: the network and every local service.
    always("socket", libc::SYS_socket),
    always("socketpair", libc::SYS_socketpair),
    // io_uring can open and use sockets without going through socket().
    always("io_uring_setup", libc::SYS_io_uring_setup),
    always("io_uring_enter", libc::SYS_io_uring_enter),
    always("io_uring_register", libc::SYS_io_uring_register),
    // other processes: inspect, copy descriptors out of, signal.
    always("ptrace", libc::SYS_ptrace),
    always("process_vm_readv", libc::SYS_process_vm_readv),
    always("process_vm_writev", libc::SYS_process_vm_writev),
    always("pidfd_open", libc::SYS_pidfd_open),
    always("pidfd_getfd", libc::SYS_pidfd_getfd),
    always("pidfd_send_signal", libc::SYS_pidfd_send_signal),
    always("process_madvise", libc::SYS_process_madvise),
    always("kcmp", libc::SYS_kcmp),
    always("kill", libc::SYS_kill),
    always("tkill", libc::SYS_tkill),
    always("tgkill", libc::SYS_tgkill),
    always("rt_sigqueueinfo", libc::SYS_rt_sigqueueinfo),
    always("rt_tgsigqueueinfo", libc::SYS_rt_tgsigqueueinfo),
    // kernel facilities that widen the attack surface or hold secrets.
    always("perf_event_open", libc::SYS_perf_event_open),
    always("bpf", libc::SYS_bpf),
    always("userfaultfd", libc::SYS_userfaultfd),
    always("keyctl", libc::SYS_keyctl),
    always("add_key", libc::SYS_add_key),
    always("request_key", libc::SYS_request_key),
    // the mount family.
    always("mount", libc::SYS_mount),
    always("umount2", libc::SYS_umount2),
    always("pivot_root", libc::SYS_pivot_root),
    always("move_mount", libc::SYS_move_mount),
    always("open_tree", libc::SYS_open_tree),
    always("mount_setattr", libc::SYS_mount_setattr),
    always("fsopen", libc::SYS_fsopen),
    always("fsconfig", libc::SYS_fsconfig),
    always("fsmount", libc::SYS_fsmount),
    always("fspick", libc::SYS_fspick),
    // file metadata: Landlock does not mediate these, so a plugin could
    // otherwise change the mode, owner, attributes or times of a user file it
    // can open for reading.
    always("fchmod", libc::SYS_fchmod),
    always("fchmodat", libc::SYS_fchmodat),
    always("fchmodat2", 452),
    always("fchown", libc::SYS_fchown),
    always("fchownat", libc::SYS_fchownat),
    always("setxattr", libc::SYS_setxattr),
    always("lsetxattr", libc::SYS_lsetxattr),
    always("fsetxattr", libc::SYS_fsetxattr),
    always("setxattrat", 463),
    always("removexattr", libc::SYS_removexattr),
    always("lremovexattr", libc::SYS_lremovexattr),
    always("fremovexattr", libc::SYS_fremovexattr),
    always("removexattrat", 466),
    always("utimensat", libc::SYS_utimensat),
    // process control that names a target process. Allowed only for the
    // caller, where the call has a fixed "calling process" value.
    only_self("prlimit64", libc::SYS_prlimit64),
    only_self("sched_setaffinity", libc::SYS_sched_setaffinity),
    only_self("sched_setscheduler", libc::SYS_sched_setscheduler),
    only_self("sched_setparam", libc::SYS_sched_setparam),
    only_self("sched_setattr", libc::SYS_sched_setattr),
    only_self("migrate_pages", libc::SYS_migrate_pages),
    only_self("move_pages", libc::SYS_move_pages),
    Denied {
        name: "setpriority",
        number: libc::SYS_setpriority,
        allowed_when: &[
            ArgEquals {
                index: 0,
                value: PRIO_PROCESS,
            },
            ArgEquals { index: 1, value: 0 },
        ],
    },
    Denied {
        name: "ioprio_set",
        number: libc::SYS_ioprio_set,
        allowed_when: &[
            ArgEquals {
                index: 0,
                value: IOPRIO_WHO_PROCESS,
            },
            ArgEquals { index: 1, value: 0 },
        ],
    },
];

// older calls that only the x86-64 table has; arm64 reaches the same behaviour
// through the *at variants above.
#[cfg(target_arch = "x86_64")]
const LEGACY: &[Denied] = &[
    always("chmod", libc::SYS_chmod),
    always("chown", libc::SYS_chown),
    always("lchown", libc::SYS_lchown),
    always("utime", libc::SYS_utime),
    always("utimes", libc::SYS_utimes),
    always("futimesat", libc::SYS_futimesat),
];

#[cfg(not(target_arch = "x86_64"))]
const LEGACY: &[Denied] = &[];

/// Every call the filter treats specially, in the order the program tests them.
pub fn denied() -> impl Iterator<Item = &'static Denied> {
    COMMON.iter().chain(LEGACY)
}

/// The audit architecture the filter accepts; any other value kills the
/// process, which closes the 32-bit entry points of an x86-64 or arm64 kernel.
#[cfg(target_arch = "x86_64")]
pub const AUDIT_ARCH: u32 = 0xC000_003E;
#[cfg(target_arch = "aarch64")]
pub const AUDIT_ARCH: u32 = 0xC000_00B7;

/// Set in the call number of the x32 entry point of an x86-64 kernel, which
/// shares the audit architecture but uses a different call table.
#[cfg(target_arch = "x86_64")]
pub const X32_SYSCALL_BIT: u32 = 0x4000_0000;

/// A digest over everything the policy grants, stable between runs: it names
/// the exact rule set a start was confined with.
pub fn digest(features: &std::collections::BTreeSet<String>) -> String {
    let mut text = String::new();
    text.push_str(&format!("abi:{FULL_LANDLOCK_ABI}\narch:{AUDIT_ARCH:#x}\n"));
    for path in SYSTEM_READ {
        text.push_str(&format!("read:{path}\n"));
    }
    for path in grant_reads(features) {
        text.push_str(&format!("grant:{path}\n"));
    }
    for rule in denied() {
        text.push_str(&format!("deny:{}:{}", rule.name, rule.number));
        for condition in rule.allowed_when {
            text.push_str(&format!(":{}={}", condition.index, condition.value));
        }
        text.push('\n');
    }
    let mut checksum =
        glib::Checksum::new(glib::ChecksumType::Sha256).expect("SHA-256 is always available");
    checksum.update(text.as_bytes());
    checksum.string().expect("a finished checksum has a string")
}
