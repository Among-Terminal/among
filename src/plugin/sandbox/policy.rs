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
    // named, per-user IPC objects outside Landlock: SysV shared memory,
    // message queues and semaphores have no per-user namespace, and the *_STAT
    // commands list objects without /proc, so another process of the same user
    // could be reached through its segment, queue or semaphore set.
    always("shmget", libc::SYS_shmget),
    always("shmat", libc::SYS_shmat),
    always("shmctl", libc::SYS_shmctl),
    always("shmdt", libc::SYS_shmdt),
    always("msgget", libc::SYS_msgget),
    always("msgsnd", libc::SYS_msgsnd),
    always("msgrcv", libc::SYS_msgrcv),
    always("msgctl", libc::SYS_msgctl),
    always("semget", libc::SYS_semget),
    always("semop", libc::SYS_semop),
    always("semtimedop", libc::SYS_semtimedop),
    always("semctl", libc::SYS_semctl),
    // POSIX message queues are named objects of the same kind.
    always("mq_open", libc::SYS_mq_open),
    always("mq_unlink", libc::SYS_mq_unlink),
    always("mq_timedsend", libc::SYS_mq_timedsend),
    always("mq_timedreceive", libc::SYS_mq_timedreceive),
    always("mq_notify", libc::SYS_mq_notify),
    always("mq_getsetattr", libc::SYS_mq_getsetattr),
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
    // open_tree plus mount_setattr in one call (Linux 6.15). The libc crate
    // has no constant for it; the number is the same in the x86-64 and the
    // generic table, which the header comparison test checks on each machine.
    always("open_tree_attr", 467),
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
    // Path-based file attributes (Linux 6.17), outside Landlock mediation.
    // Both supported native syscall tables assign these same numbers.
    always("file_getattr", 468),
    always("file_setattr", 469),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn names() -> BTreeSet<&'static str> {
        denied().map(|rule| rule.name).collect()
    }

    fn rule(name: &str) -> &'static Denied {
        denied()
            .find(|rule| rule.name == name)
            .unwrap_or_else(|| panic!("`{name}` is no longer filtered"))
    }

    fn strings(list: &[&'static str]) -> BTreeSet<&'static str> {
        list.iter().copied().collect()
    }

    #[test]
    fn the_filtered_calls_are_exactly_the_reviewed_list() {
        // written out here, not derived from the table: removing or adding a
        // call must be a decision that shows up in this list too.
        let mut expected = strings(&[
            "socket",
            "socketpair",
            "io_uring_setup",
            "io_uring_enter",
            "io_uring_register",
            "ptrace",
            "process_vm_readv",
            "process_vm_writev",
            "pidfd_open",
            "pidfd_getfd",
            "pidfd_send_signal",
            "process_madvise",
            "kcmp",
            "shmget",
            "shmat",
            "shmctl",
            "shmdt",
            "msgget",
            "msgsnd",
            "msgrcv",
            "msgctl",
            "semget",
            "semop",
            "semtimedop",
            "semctl",
            "mq_open",
            "mq_unlink",
            "mq_timedsend",
            "mq_timedreceive",
            "mq_notify",
            "mq_getsetattr",
            "kill",
            "tkill",
            "tgkill",
            "rt_sigqueueinfo",
            "rt_tgsigqueueinfo",
            "perf_event_open",
            "bpf",
            "userfaultfd",
            "keyctl",
            "add_key",
            "request_key",
            "mount",
            "umount2",
            "pivot_root",
            "move_mount",
            "open_tree",
            "open_tree_attr",
            "mount_setattr",
            "fsopen",
            "fsconfig",
            "fsmount",
            "fspick",
            "fchmod",
            "fchmodat",
            "fchmodat2",
            "file_getattr",
            "file_setattr",
            "fchown",
            "fchownat",
            "setxattr",
            "lsetxattr",
            "fsetxattr",
            "setxattrat",
            "removexattr",
            "lremovexattr",
            "fremovexattr",
            "removexattrat",
            "utimensat",
            "prlimit64",
            "sched_setaffinity",
            "sched_setscheduler",
            "sched_setparam",
            "sched_setattr",
            "migrate_pages",
            "move_pages",
            "setpriority",
            "ioprio_set",
        ]);
        if cfg!(target_arch = "x86_64") {
            expected.extend(["chmod", "chown", "lchown", "utime", "utimes", "futimesat"]);
        }
        assert_eq!(names(), expected);
    }

    fn always_denied(list: &[&str]) {
        for name in list {
            assert!(
                rule(name).allowed_when.is_empty(),
                "{name} must be denied for every argument"
            );
        }
    }

    #[test]
    fn sockets_of_every_family_and_the_io_uring_detour_are_denied() {
        always_denied(&[
            "socket",
            "socketpair",
            "io_uring_setup",
            "io_uring_enter",
            "io_uring_register",
        ]);
    }

    #[test]
    fn named_ipc_objects_of_the_same_user_are_denied() {
        always_denied(&[
            "shmget",
            "shmat",
            "shmctl",
            "shmdt",
            "msgget",
            "msgsnd",
            "msgrcv",
            "msgctl",
            "semget",
            "semop",
            "semtimedop",
            "semctl",
            "mq_open",
            "mq_unlink",
            "mq_timedsend",
            "mq_timedreceive",
            "mq_notify",
            "mq_getsetattr",
        ]);
    }

    #[test]
    fn inspecting_other_processes_is_denied() {
        always_denied(&[
            "ptrace",
            "process_vm_readv",
            "process_vm_writev",
            "pidfd_open",
            "pidfd_getfd",
            "process_madvise",
            "kcmp",
        ]);
    }

    #[test]
    fn every_way_to_send_a_signal_is_denied_without_an_exception_for_the_own_pid() {
        always_denied(&[
            "kill",
            "tkill",
            "tgkill",
            "rt_sigqueueinfo",
            "rt_tgsigqueueinfo",
            "pidfd_send_signal",
        ]);
    }

    #[test]
    fn kernel_facilities_that_widen_the_attack_surface_are_denied() {
        always_denied(&[
            "perf_event_open",
            "bpf",
            "userfaultfd",
            "keyctl",
            "add_key",
            "request_key",
        ]);
    }

    #[test]
    fn the_mount_family_is_denied() {
        always_denied(&[
            "mount",
            "umount2",
            "pivot_root",
            "move_mount",
            "open_tree",
            "open_tree_attr",
            "mount_setattr",
            "fsopen",
            "fsconfig",
            "fsmount",
            "fspick",
        ]);
    }

    #[test]
    fn mode_owner_attribute_and_time_changes_are_denied_for_every_path() {
        always_denied(&[
            "fchmod",
            "fchmodat",
            "fchmodat2",
            "file_getattr",
            "file_setattr",
            "fchown",
            "fchownat",
            "setxattr",
            "lsetxattr",
            "fsetxattr",
            "setxattrat",
            "removexattr",
            "lremovexattr",
            "fremovexattr",
            "removexattrat",
            "utimensat",
        ]);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn the_legacy_x86_64_metadata_calls_are_denied_too() {
        always_denied(&["chmod", "chown", "lchown", "utime", "utimes", "futimesat"]);
    }

    #[test]
    fn calls_that_name_a_process_are_allowed_only_with_the_calling_process_as_target() {
        for name in [
            "prlimit64",
            "sched_setaffinity",
            "sched_setscheduler",
            "sched_setparam",
            "sched_setattr",
            "migrate_pages",
            "move_pages",
        ] {
            assert_eq!(
                rule(name).allowed_when,
                [ArgEquals { index: 0, value: 0 }],
                "{name} is allowed only with pid 0"
            );
        }
    }

    #[test]
    fn priority_changes_are_allowed_only_for_the_calling_process() {
        assert_eq!(
            rule("setpriority").allowed_when,
            [
                ArgEquals { index: 0, value: 0 },
                ArgEquals { index: 1, value: 0 }
            ],
            "PRIO_PROCESS with who == 0 only: PRIO_PGRP and PRIO_USER with 0 reach other processes"
        );
        assert_eq!(
            rule("ioprio_set").allowed_when,
            [
                ArgEquals { index: 0, value: 1 },
                ArgEquals { index: 1, value: 0 }
            ],
            "IOPRIO_WHO_PROCESS with who == 0 only"
        );
    }

    #[test]
    fn only_the_native_architecture_is_accepted_and_x32_is_known_on_x86_64() {
        #[cfg(target_arch = "x86_64")]
        {
            assert_eq!(AUDIT_ARCH, 0xC000_003E, "AUDIT_ARCH_X86_64");
            assert_eq!(X32_SYSCALL_BIT, 1 << 30, "__X32_SYSCALL_BIT");
        }
        #[cfg(target_arch = "aarch64")]
        assert_eq!(AUDIT_ARCH, 0xC000_00B7, "AUDIT_ARCH_AARCH64");
    }

    #[test]
    fn the_filesystem_policy_is_the_reviewed_set() {
        assert_eq!(
            SYSTEM_READ,
            [
                "/usr",
                "/lib",
                "/lib64",
                "/etc/ld.so.cache",
                "/etc/localtime"
            ]
        );
        assert_eq!(
            SYSTEM_METRICS_READ,
            ["/proc/stat", "/proc/meminfo", "/proc/loadavg"]
        );
        assert_eq!(FULL_LANDLOCK_ABI, 3);
    }

    #[test]
    fn only_the_system_metrics_feature_adds_read_access() {
        let none = BTreeSet::new();
        assert!(grant_reads(&none).is_empty());
        let others: BTreeSet<String> = ["status", "settings", "context.cwd", "events.terminal"]
            .map(String::from)
            .into();
        assert!(grant_reads(&others).is_empty());
        let metrics: BTreeSet<String> =
            ["status".to_string(), SYSTEM_METRICS_FEATURE.to_string()].into();
        assert_eq!(grant_reads(&metrics), SYSTEM_METRICS_READ);
    }

    #[test]
    fn the_digest_is_stable_and_names_the_grants_that_were_applied() {
        let none = BTreeSet::new();
        let metrics: BTreeSet<String> = [SYSTEM_METRICS_FEATURE.to_string()].into();
        assert_eq!(digest(&none), digest(&none));
        assert_eq!(digest(&none).len(), 64);
        assert!(digest(&none).bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(digest(&none), digest(&metrics));
        // features that add no access do not change what was applied.
        let status: BTreeSet<String> = ["status".to_string()].into();
        assert_eq!(digest(&none), digest(&status));
    }

    #[test]
    fn call_numbers_match_the_kernel_headers_installed_on_this_machine() {
        // x86-64 numbers are in the architecture header; arm64 uses the
        // generic table. a name absent from an older header is reported, not
        // assumed.
        let (path, file) = if cfg!(target_arch = "x86_64") {
            (
                "/usr/include/x86_64-linux-gnu/asm/unistd_64.h",
                std::fs::read_to_string("/usr/include/x86_64-linux-gnu/asm/unistd_64.h"),
            )
        } else {
            (
                "/usr/include/asm-generic/unistd.h",
                std::fs::read_to_string("/usr/include/asm-generic/unistd.h"),
            )
        };
        let Ok(header) = file else {
            eprintln!("NOT VERIFIED: {path} is not installed; call numbers were not compared");
            return;
        };
        let mut missing = Vec::new();
        for rule in denied() {
            let wanted = format!("#define __NR_{} ", rule.name);
            let number = header.lines().find_map(|line| {
                line.strip_prefix(&wanted)
                    .and_then(|rest| rest.split_whitespace().next())
                    .and_then(|number| number.parse::<c_long>().ok())
            });
            match number {
                Some(number) => {
                    assert_eq!(number, rule.number, "{} differs from {path}", rule.name)
                }
                None => missing.push(rule.name),
            }
        }
        if !missing.is_empty() {
            eprintln!("NOT VERIFIED against {path} (absent from this header version): {missing:?}");
        }
    }
}

#[cfg(test)]
mod review_tests {
    //! a kernel that gains a call touching sockets, processes, signals, file
    //! metadata, mounts or privileges must not slip past the deny list
    //! unnoticed: such a name has to be filtered or listed here as reviewed.

    use super::denied;
    use std::collections::BTreeSet;

    /// Name fragments of calls whose reach a reviewer has to judge.
    const WATCHED: &[&str] = &[
        "xattr",
        "file_getattr",
        "file_setattr",
        "chmod",
        "chown",
        "utime",
        "mount",
        "sock",
        "signal",
        "kill",
        "pid",
        "ptrace",
        "uring",
        "perf",
        "bpf",
        "key",
        "priority",
        "sched_set",
        "affinity",
        "rlimit",
        "move_",
        "migrate",
        "setns",
        "unshare",
        "process_",
        "kcmp",
        "shm",
        "msg",
        "sem",
        "mq_",
        "userfault",
        "fsopen",
        "fsconfig",
        "fsmount",
        "fspick",
        "open_tree",
        "clone",
        "reboot",
        "kexec",
        "swap",
        "chroot",
        "quotactl",
        "acct",
        "ioprio",
        "personality",
        "memfd",
        "seccomp",
        "landlock",
        "prctl",
        "ioperm",
        "iopl",
        "mknod",
        "link",
        "rename",
        "statmount",
        "listmount",
    ];

    /// Watched calls that are deliberately allowed, each with the reason.
    const REVIEWED_ALLOWED: &[(&str, &str)] = &[
        ("fstat", "reads an open descriptor"),
        ("fstatfs", "reads an open descriptor; statfs is needed for disk usage"),
        ("fstatfs64", "32-bit table only"),
        ("fstatat64", "32-bit table only"),
        ("fstat64", "32-bit table only"),
        ("getpid", "own process"),
        ("getppid", "own parent"),
        ("getsockname", "needs a socket, which cannot be created"),
        ("setsockopt", "needs a socket, which cannot be created"),
        ("getsockopt", "needs a socket, which cannot be created"),
        ("clone", "children inherit the sandbox"),
        ("clone3", "children inherit the sandbox"),
        ("fsync", "own open file"),
        ("rename", "Landlock mediates the paths"),
        ("renameat", "Landlock mediates the paths"),
        ("renameat2", "Landlock mediates the paths"),
        ("link", "Landlock mediates the paths"),
        ("linkat", "Landlock mediates the paths"),
        ("symlink", "Landlock mediates the paths"),
        ("symlinkat", "Landlock mediates the paths"),
        ("mknod", "Landlock mediates creation in the data directory"),
        ("mknodat", "Landlock mediates creation in the data directory"),
        ("getrlimit", "read only, own process"),
        ("setrlimit", "own process only"),
        ("personality", "execution domain of the own process; the filter checks the call's architecture"),
        ("getpriority", "read only"),
        ("sched_get_priority_max", "read only"),
        ("sched_get_priority_min", "read only"),
        ("sched_getaffinity", "read only"),
        ("prctl", "own process; no_new_privs is already set"),
        ("arch_prctl", "own process"),
        ("chroot", "needs CAP_SYS_CHROOT"),
        ("acct", "needs CAP_SYS_PACCT"),
        ("swapon", "needs CAP_SYS_ADMIN"),
        ("swapoff", "needs CAP_SYS_ADMIN"),
        ("reboot", "needs CAP_SYS_BOOT"),
        ("iopl", "needs CAP_SYS_RAWIO"),
        ("ioperm", "needs CAP_SYS_RAWIO"),
        ("quotactl", "needs CAP_SYS_ADMIN"),
        ("quotactl_fd", "needs CAP_SYS_ADMIN"),
        ("kexec_load", "needs CAP_SYS_BOOT"),
        ("kexec_file_load", "needs CAP_SYS_BOOT"),
        ("getxattr", "read only"),
        ("lgetxattr", "read only"),
        ("fgetxattr", "read only"),
        ("getxattrat", "read only"),
        ("listxattr", "read only"),
        ("llistxattr", "read only"),
        ("flistxattr", "read only"),
        ("listxattrat", "read only"),
        ("unshare", "namespaces give rights only inside them; mount and sockets stay denied, Landlock and seccomp are inherited"),
        ("setns", "joining a namespace needs privilege over it; the same reasoning as unshare"),
        ("signalfd", "receives signals; cannot send them"),
        ("signalfd4", "receives signals; cannot send them"),
        ("pkey_mprotect", "own memory"),
        ("pkey_alloc", "own memory"),
        ("pkey_free", "own memory"),
        ("seccomp", "can only add restrictions"),
        ("landlock_create_ruleset", "can only add restrictions"),
        ("landlock_add_rule", "can only add restrictions"),
        ("landlock_restrict_self", "can only add restrictions"),
        ("memfd_create", "anonymous memory; no access to user files"),
        ("memfd_secret", "anonymous memory"),
        ("process_mrelease", "needs a pidfd, and pidfd_open and pidfd_getfd are denied"),
        ("statmount", "read only mount information"),
        ("listmount", "read only mount information"),
        ("ioprio_get", "read only"),
        ("getpmsg", "STREAMS, not implemented by Linux"),
        ("putpmsg", "STREAMS, not implemented by Linux"),
        ("sendmsg", "needs a socket, which cannot be created or inherited"),
        ("sendmmsg", "needs a socket, which cannot be created or inherited"),
        ("recvmsg", "needs a socket, which cannot be created or inherited"),
        ("recvmmsg", "needs a socket, which cannot be created or inherited"),
        ("recvmmsg_time64", "32-bit table only; a foreign architecture is killed"),
        ("mq_timedsend_time64", "32-bit table only; a foreign architecture is killed"),
        ("mq_timedreceive_time64", "32-bit table only; a foreign architecture is killed"),
        ("semtimedop_time64", "32-bit table only; a foreign architecture is killed"),
        ("readlink", "Landlock mediates the paths"),
        ("readlinkat", "Landlock mediates the paths"),
        ("unlink", "Landlock mediates the paths"),
        ("unlinkat", "Landlock mediates the paths"),
        ("utimensat_time64","32-bit table only; a foreign architecture is killed"),
    ];

    fn call_names(header: &str) -> BTreeSet<String> {
        header
            .lines()
            .filter_map(|line| line.strip_prefix("#define __NR_"))
            .filter_map(|rest| rest.split_whitespace().next())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn every_watched_kernel_call_in_the_installed_headers_is_filtered_or_reviewed() {
        let filtered: BTreeSet<&str> = denied().map(|rule| rule.name).collect();
        let reviewed: BTreeSet<&str> = REVIEWED_ALLOWED.iter().map(|(name, _)| *name).collect();
        let mut seen = 0;
        let mut unreviewed = Vec::new();
        for path in [
            "/usr/include/x86_64-linux-gnu/asm/unistd_64.h",
            "/usr/include/asm-generic/unistd.h",
        ] {
            let Ok(header) = std::fs::read_to_string(path) else {
                eprintln!("NOT VERIFIED: {path} is not installed");
                continue;
            };
            seen += 1;
            for name in call_names(&header) {
                // the filter's own table is per architecture; a name denied on
                // the other one still counts as reviewed here.
                let watched = WATCHED.iter().any(|fragment| name.contains(fragment));
                if watched
                    && !filtered.contains(name.as_str())
                    && !reviewed.contains(name.as_str())
                    && !LEGACY_X86_ONLY.contains(&name.as_str())
                {
                    unreviewed.push(format!("{name} ({path})"));
                }
            }
        }
        if seen == 0 {
            return;
        }
        assert!(
            unreviewed.is_empty(),
            "kernel calls to review: filter them in policy.rs or list them with a reason \
             in REVIEWED_ALLOWED: {unreviewed:?}"
        );
    }

    /// Denied on x86-64 only; the generic table has no such call.
    const LEGACY_X86_ONLY: &[&str] = &["chmod", "chown", "lchown", "utime", "utimes", "futimesat"];

    #[test]
    fn every_reviewed_exception_has_a_reason() {
        for (name, reason) in REVIEWED_ALLOWED {
            assert!(!reason.is_empty(), "{name}");
        }
    }
}
