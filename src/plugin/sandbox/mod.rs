//! kernel-enforced confinement of managed plugin processes: no sockets, no
//! files except the plugin's own data directory and what it needs to execute,
//! and a report from the child of what it really applied.
//!
//! Landlock restricts the filesystem, a seccomp filter denies sockets, process
//! control and file-metadata calls. Both are built in the host before the
//! plugin process exists and applied inside it before `exec`.

pub mod bpf;
pub mod data_dir;
pub mod launch;
pub mod level;
pub mod policy;
pub mod status;

#[cfg(test)]
mod hostile;
#[cfg(test)]
mod hostile_tests;
#[cfg(test)]
mod launch_tests;

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: launch::allocation_guard::Guard = launch::allocation_guard::Guard;
