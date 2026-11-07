//! the seccomp program, built from `policy` before the plugin process exists
//! and loaded inside it with two raw system calls that allocate nothing.

use super::policy::{self, Denied};
use libc::sock_filter;

const KILL_PROCESS: u32 = 0x8000_0000;
const ERRNO: u32 = 0x0005_0000;
const ALLOW: u32 = 0x7fff_0000;

const MAX_INSTRUCTIONS: usize = 4096;

// offsets in `struct seccomp_data`: nr (u32), arch (u32), instruction_pointer
// (u64), args (6 x u64). the filter reads the low 32 bits of an argument,
// which is where a little-endian kernel keeps them.
const OFFSET_NR: u32 = 0;
const OFFSET_ARCH: u32 = 4;
const OFFSET_ARGS: u32 = 16;

#[cfg(not(target_endian = "little"))]
compile_error!("the seccomp program reads argument low words at little-endian offsets");

fn statement(code: u32, k: u32) -> sock_filter {
    sock_filter {
        code: code as u16,
        jt: 0,
        jf: 0,
        k,
    }
}

fn jump(code: u32, k: u32, jt: u8, jf: u8) -> sock_filter {
    sock_filter {
        code: code as u16,
        jt,
        jf,
        k,
    }
}

const LOAD_WORD: u32 = libc::BPF_LD | libc::BPF_W | libc::BPF_ABS;
const JUMP_EQUAL: u32 = libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K;
#[cfg(target_arch = "x86_64")]
const JUMP_SET: u32 = libc::BPF_JMP | libc::BPF_JSET | libc::BPF_K;
const RETURN: u32 = libc::BPF_RET | libc::BPF_K;

/// A finished filter. Immutable, so a clone shared with a forked child needs
/// no synchronisation.
#[derive(Clone, Debug)]
pub struct Program {
    instructions: Vec<sock_filter>,
}

impl Program {
    /// The filter for the compile target's policy.
    pub fn build() -> Self {
        let rules: Vec<&Denied> = policy::denied().collect();
        Self::from_rules(&rules)
    }

    fn from_rules(rules: &[&Denied]) -> Self {
        let mut out = vec![
            statement(LOAD_WORD, OFFSET_ARCH),
            jump(JUMP_EQUAL, policy::AUDIT_ARCH, 1, 0),
            statement(RETURN, KILL_PROCESS),
            statement(LOAD_WORD, OFFSET_NR),
        ];
        #[cfg(target_arch = "x86_64")]
        out.extend([
            jump(JUMP_SET, policy::X32_SYSCALL_BIT, 0, 1),
            statement(RETURN, KILL_PROCESS),
        ]);
        let deny = ERRNO | libc::EPERM as u32;
        for rule in rules {
            // the accumulator holds the call number on entry to every block:
            // a block that loads an argument always ends in a return.
            let conditions = rule.allowed_when.len();
            let block = if conditions == 0 {
                1
            } else {
                2 * conditions + 2
            };
            out.push(jump(JUMP_EQUAL, rule.number as u32, 0, block as u8));
            if conditions == 0 {
                out.push(statement(RETURN, deny));
                continue;
            }
            // a failed comparison jumps to the refusal at the end of the block.
            for (position, condition) in rule.allowed_when.iter().enumerate() {
                out.push(statement(
                    LOAD_WORD,
                    OFFSET_ARGS + 8 * u32::from(condition.index),
                ));
                let to_refusal = (2 * (conditions - position - 1) + 1) as u8;
                out.push(jump(JUMP_EQUAL, condition.value, 0, to_refusal));
            }
            out.push(statement(RETURN, ALLOW));
            out.push(statement(RETURN, deny));
        }
        out.push(statement(RETURN, ALLOW));
        assert!(out.len() <= MAX_INSTRUCTIONS, "seccomp program too long");
        Self { instructions: out }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.instructions.len()
    }

    /// Loads the filter into the calling thread. Requires `no_new_privs`.
    /// Allocates nothing and uses only raw system calls: it runs between
    /// `fork` and `exec` of a multithreaded process.
    pub fn load(&self) -> Result<(), i32> {
        let program = libc::sock_fprog {
            len: self.instructions.len() as u16,
            filter: self.instructions.as_ptr() as *mut sock_filter,
        };
        // SECCOMP_SET_MODE_FILTER = 1; the program outlives the call.
        let result = unsafe {
            libc::syscall(
                libc::SYS_seccomp,
                1 as libc::c_ulong,
                0 as libc::c_ulong,
                &program as *const libc::sock_fprog,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(unsafe { *libc::__errno_location() })
        }
    }

    #[cfg(test)]
    pub(super) fn instructions(&self) -> &[sock_filter] {
        &self.instructions
    }
}

#[cfg(test)]
pub(super) mod interpreter {
    //! just enough of the classic BPF machine to run the generated program on
    //! chosen system call data, so the policy is tested without a kernel.

    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    pub enum Verdict {
        Allow,
        Errno(i32),
        KillProcess,
    }

    pub struct Call {
        pub arch: u32,
        pub number: u32,
        pub args: [u64; 6],
    }

    fn word(call: &Call, offset: u32) -> u32 {
        match offset {
            OFFSET_NR => call.number,
            OFFSET_ARCH => call.arch,
            offset if offset >= OFFSET_ARGS => {
                let slot = ((offset - OFFSET_ARGS) / 8) as usize;
                let half = (offset - OFFSET_ARGS) % 8;
                let value = call.args[slot];
                if half == 0 {
                    value as u32
                } else {
                    (value >> 32) as u32
                }
            }
            other => panic!("filter reads offset {other}"),
        }
    }

    pub fn run(program: &Program, call: &Call) -> Verdict {
        let mut accumulator = 0u32;
        let mut counter = 0usize;
        loop {
            let instruction = program.instructions[counter];
            counter += 1;
            let code = u32::from(instruction.code);
            if code == LOAD_WORD {
                accumulator = word(call, instruction.k);
            } else if code == JUMP_EQUAL {
                counter += if accumulator == instruction.k {
                    usize::from(instruction.jt)
                } else {
                    usize::from(instruction.jf)
                };
            } else if cfg!(target_arch = "x86_64")
                && code == libc::BPF_JMP | libc::BPF_JSET | libc::BPF_K
            {
                counter += if accumulator & instruction.k != 0 {
                    usize::from(instruction.jt)
                } else {
                    usize::from(instruction.jf)
                };
            } else if code == RETURN {
                return match instruction.k {
                    ALLOW => Verdict::Allow,
                    KILL_PROCESS => Verdict::KillProcess,
                    value if value & 0xffff_0000 == ERRNO => {
                        Verdict::Errno((value & 0xffff) as i32)
                    }
                    other => panic!("filter returns {other:#x}"),
                };
            } else {
                panic!("filter uses opcode {code:#x}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::interpreter::{run, Call, Verdict};
    use super::*;
    use crate::plugin::sandbox::policy::{denied, AUDIT_ARCH};

    fn call(number: i64, args: [u64; 6]) -> Call {
        Call {
            arch: AUDIT_ARCH,
            number: number as u32,
            args,
        }
    }

    #[test]
    fn the_architecture_check_comes_first_and_kills_other_architectures() {
        let program = Program::build();
        let instructions = program.instructions();
        assert_eq!(u32::from(instructions[0].code), LOAD_WORD);
        assert_eq!(instructions[0].k, OFFSET_ARCH);
        assert_eq!(instructions[1].k, AUDIT_ARCH);
        assert_eq!(instructions[2].k, KILL_PROCESS);
        // 32-bit x86 on an x86-64 kernel, and 32-bit ARM on an arm64 kernel.
        for foreign in [0x4000_0003u32, 0x4000_0028, 0xC000_00F3, 0] {
            let verdict = run(
                &program,
                &Call {
                    arch: foreign,
                    number: libc::SYS_getpid as u32,
                    args: [0; 6],
                },
            );
            assert_eq!(verdict, Verdict::KillProcess, "arch {foreign:#x}");
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn the_x32_bit_check_comes_right_after_the_architecture_check_and_kills() {
        let program = Program::build();
        let instructions = program.instructions();
        assert_eq!(u32::from(instructions[3].code), LOAD_WORD);
        assert_eq!(instructions[3].k, OFFSET_NR);
        assert_eq!(u32::from(instructions[4].code), JUMP_SET);
        assert_eq!(instructions[4].k, policy::X32_SYSCALL_BIT);
        assert_eq!(instructions[5].k, KILL_PROCESS);
        // an allowed call and a denied call both die with the bit set.
        for number in [libc::SYS_getpid, libc::SYS_socket] {
            let verdict = run(
                &program,
                &call(number | i64::from(policy::X32_SYSCALL_BIT), [0; 6]),
            );
            assert_eq!(verdict, Verdict::KillProcess);
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn the_arm64_program_has_no_x32_check() {
        let program = Program::build();
        // arch check (3) and the number load, then rules start directly.
        assert_eq!(u32::from(program.instructions()[3].code), LOAD_WORD);
        assert_eq!(
            u32::from(program.instructions()[4].code),
            JUMP_EQUAL,
            "a rule follows the number load"
        );
    }

    #[test]
    fn program_is_within_the_kernel_limit_and_jumps_stay_inside() {
        let program = Program::build();
        assert!(program.len() <= MAX_INSTRUCTIONS);
        for (position, instruction) in program.instructions().iter().enumerate() {
            let code = u32::from(instruction.code);
            if code == JUMP_EQUAL {
                let furthest = position + 1 + usize::from(instruction.jt.max(instruction.jf));
                assert!(
                    furthest < program.len(),
                    "jump at {position} leaves the program"
                );
            }
        }
        // the last instruction allows: everything not named is permitted.
        assert_eq!(program.instructions().last().unwrap().k, ALLOW);
    }

    #[test]
    fn every_unconditionally_denied_call_returns_eperm_whatever_its_arguments() {
        let program = Program::build();
        for rule in denied().filter(|rule| rule.allowed_when.is_empty()) {
            for args in [[0u64; 6], [u64::MAX; 6], [1, 2, 3, 4, 5, 6]] {
                assert_eq!(
                    run(&program, &call(rule.number, args)),
                    Verdict::Errno(libc::EPERM),
                    "{} must stay denied",
                    rule.name
                );
            }
        }
    }

    #[test]
    fn path_based_file_attributes_are_refused_even_with_older_kernel_headers() {
        // Linux 6.17 assigns 468/469 in both x86-64 and the generic table.
        // Pin the ABI independently of libc and the generated policy table:
        // older build hosts need to protect plugins on newer runtime kernels.
        let program = Program::build();
        for number in [468, 469] {
            for args in [[0; 6], [u64::MAX; 6]] {
                assert_eq!(
                    run(&program, &call(number, args)),
                    Verdict::Errno(libc::EPERM)
                );
            }
        }
    }

    #[test]
    fn the_loaded_filter_returns_eperm_for_both_file_attribute_syscall_numbers() {
        let program = Program::build();
        // The child touches no files. Invalid pointers keep this safe even on
        // a new kernel; this proves filter dispatch, not successful file-attr
        // operations on kernels that implement them. Build before fork so the
        // child performs only raw calls and an allocation-free filter load.
        let child = unsafe { libc::fork() };
        assert!(child >= 0, "fork: {}", std::io::Error::last_os_error());
        if child == 0 {
            unsafe {
                if libc::prctl(
                    libc::PR_SET_NO_NEW_PRIVS,
                    1 as libc::c_ulong,
                    0 as libc::c_ulong,
                    0 as libc::c_ulong,
                    0 as libc::c_ulong,
                ) != 0
                {
                    libc::_exit(10);
                }
                if program.load().is_err() {
                    libc::_exit(11);
                }
                for number in [468 as libc::c_long, 469] {
                    let result = libc::syscall(
                        number,
                        0 as libc::c_long,
                        0 as libc::c_long,
                        0 as libc::c_long,
                        0 as libc::c_long,
                        0 as libc::c_long,
                    );
                    if result != -1 || *libc::__errno_location() != libc::EPERM {
                        libc::_exit(12);
                    }
                }
                libc::_exit(0);
            }
        }
        let mut status = 0;
        let waited = loop {
            let waited = unsafe { libc::waitpid(child, &mut status, 0) };
            if waited >= 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                break waited;
            }
        };
        assert_eq!(waited, child);
        assert!(libc::WIFEXITED(status), "child wait status {status}");
        assert_eq!(
            libc::WEXITSTATUS(status),
            0,
            "filter-load or syscall refusal failed"
        );
    }

    #[test]
    fn a_conditional_call_is_allowed_only_for_the_calling_process() {
        let program = Program::build();
        for rule in denied().filter(|rule| !rule.allowed_when.is_empty()) {
            let mut permitted = [0u64; 6];
            for condition in rule.allowed_when {
                permitted[usize::from(condition.index)] = u64::from(condition.value);
            }
            assert_eq!(
                run(&program, &call(rule.number, permitted)),
                Verdict::Allow,
                "{} for the caller",
                rule.name
            );
            for condition in rule.allowed_when {
                let index = usize::from(condition.index);
                // a different value in the low half is refused.
                let mut other = permitted;
                other[index] = u64::from(condition.value) + 1;
                assert_eq!(
                    run(&program, &call(rule.number, other)),
                    Verdict::Errno(libc::EPERM),
                    "{} with argument {index} changed",
                    rule.name
                );
                // a value only in the high half does not count as the permitted
                // low half: the kernel reads 32 bits, so 0 with junk above is
                // the caller, but junk below is not.
                let mut high_only = permitted;
                high_only[index] = u64::from(condition.value) | (1 << 40);
                assert_eq!(
                    run(&program, &call(rule.number, high_only)),
                    Verdict::Allow,
                    "{} reads only 32 bits of argument {index}",
                    rule.name
                );
            }
        }
    }

    #[test]
    fn setpriority_refuses_process_group_and_user_targets_even_with_who_zero() {
        let program = Program::build();
        let number = libc::SYS_setpriority;
        // PRIO_PROCESS = 0, PRIO_PGRP = 1, PRIO_USER = 2.
        assert_eq!(
            run(&program, &call(number, [0, 0, 0, 0, 0, 0])),
            Verdict::Allow
        );
        for which in [1u64, 2] {
            assert_eq!(
                run(&program, &call(number, [which, 0, 0, 0, 0, 0])),
                Verdict::Errno(libc::EPERM)
            );
        }
        // another process by PID.
        assert_eq!(
            run(&program, &call(number, [0, 4242, 0, 0, 0, 0])),
            Verdict::Errno(libc::EPERM)
        );
    }

    #[test]
    fn calls_the_plugins_need_stay_allowed() {
        let program = Program::build();
        for number in [
            libc::SYS_read,
            libc::SYS_write,
            libc::SYS_openat,
            libc::SYS_close,
            libc::SYS_mmap,
            libc::SYS_futex,
            libc::SYS_clock_nanosleep,
            libc::SYS_execve,
            libc::SYS_clone,
            libc::SYS_exit_group,
            libc::SYS_rt_sigaction,
            libc::SYS_getrandom,
            libc::SYS_prctl,
            libc::SYS_statfs,
            libc::SYS_landlock_create_ruleset,
        ] {
            assert_eq!(
                run(&program, &call(number, [0; 6])),
                Verdict::Allow,
                "syscall {number}"
            );
        }
    }
}
