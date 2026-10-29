//! select fault injection by the admitted receipt and executable inode, never
//! a basename: managed processes execute pinned /proc/self/fd paths.

use super::profile;
use crate::{plugin_install::release, storage, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Process {
    pid: u32,
    parent: u32,
    group: u32,
    start: u64,
    uid: u32,
}

fn proc_text(path: &Path, limit: u64) -> Result<String> {
    let mut bytes = Vec::new();
    OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
        .open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err("fault target proc input exceeds its bound".into());
    }
    Ok(String::from_utf8(bytes)?)
}

fn process(pid: u32) -> Result<Process> {
    let root = PathBuf::from(format!("/proc/{pid}"));
    let text = proc_text(&root.join("stat"), 16 * 1024)?;
    let (prefix, suffix) = text
        .rsplit_once(')')
        .ok_or("invalid fault target proc stat")?;
    let actual = prefix
        .split_once('(')
        .ok_or("missing proc stat pid")?
        .0
        .trim()
        .parse::<u32>()?;
    let fields = suffix.split_whitespace().collect::<Vec<_>>();
    if actual != pid || fields.len() < 20 || matches!(fields[0], "Z" | "X" | "x") {
        return Err("fault target is gone or no longer live".into());
    }
    Ok(Process {
        pid,
        parent: fields[1].parse()?,
        group: fields[2].parse()?,
        start: fields[19].parse()?,
        uid: root.metadata()?.uid(),
    })
}

fn inode(file: &File) -> Result<(u64, u64)> {
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

pub(super) struct Host {
    identity: Process,
    executable: File,
}

impl Host {
    pub(super) fn observe(pid: u32) -> Result<Self> {
        let identity = process(pid)?;
        if identity.uid != nix::unistd::geteuid().as_raw() {
            return Err("fault host is not owned by the checker".into());
        }
        let executable = File::open(format!("/proc/{pid}/exe"))?;
        let host = Self {
            identity,
            executable,
        };
        host.check()?;
        Ok(host)
    }
    fn check(&self) -> Result<()> {
        if process(self.identity.pid)? != self.identity
            || inode(&File::open(format!("/proc/{}/exe", self.identity.pid))?)?
                != inode(&self.executable)?
        {
            return Err("owned host changed; no fault signal sent".into());
        }
        Ok(())
    }
}

fn children(host: &Host) -> Result<BTreeSet<u32>> {
    host.check()?;
    let mut children = BTreeSet::new();
    // supervisors spawn from worker threads, whose children are not listed in
    // /proc/<host>/task/<main-thread>/children. Stay inside this owned host.
    for (count, entry) in fs::read_dir(format!("/proc/{}/task", host.identity.pid))?.enumerate() {
        if count >= 256 {
            return Err("owned host exceeds fault-injection thread bound".into());
        }
        let entry = entry?;
        let text = match proc_text(&entry.path().join("children"), 16 * 1024) {
            Ok(text) => text,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                continue
            }
            Err(error) => return Err(error),
        };
        for pid in text.split_whitespace() {
            children.insert(pid.parse::<u32>()?);
        }
        if children.len() > 512 {
            return Err("owned host exceeds fault-injection child bound".into());
        }
    }
    Ok(children)
}

struct Target {
    identity: Process,
    pidfd: File,
}

impl Target {
    fn check(&self, host: &Host, executable: &File) -> Result<()> {
        host.check()?;
        if process(self.identity.pid)? != self.identity
            || self.identity.parent != host.identity.pid
            || self.identity.group != self.identity.pid
            || self.identity.uid != host.identity.uid
            || inode(&File::open(format!("/proc/{}/exe", self.identity.pid))?)?
                != inode(executable)?
        {
            return Err("supervised child binding changed; no fault signal sent".into());
        }
        Ok(())
    }
    fn kill(&self, host: &Host, executable: &File) -> Result<()> {
        self.check(host, executable)?;
        // pidfd prevents a reaped/reused numeric PID from receiving this signal.
        // unavailable pidfd support is a refusal, never a numeric-kill fallback.
        if unsafe {
            nix::libc::syscall(
                nix::libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                nix::libc::SIGKILL,
                std::ptr::null::<nix::libc::siginfo_t>(),
                0,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
}

fn target(host: &Host, executable: &File) -> Result<Target> {
    let expected = inode(executable)?;
    let children = children(host)?;
    let mut matched = Vec::new();
    let mut unavailable = 0;
    let mut wrong_parent = 0;
    let mut wrong_group = 0;
    let mut wrong_uid = 0;
    let mut unreadable_executable = 0;
    let mut wrong_inode = 0;
    for &pid in &children {
        let observed = match process(pid) {
            Ok(observed) => observed,
            Err(_) => {
                unavailable += 1;
                continue;
            }
        };
        if observed.parent != host.identity.pid
            || observed.group != pid
            || observed.uid != host.identity.uid
        {
            wrong_parent += usize::from(observed.parent != host.identity.pid);
            wrong_group += usize::from(observed.group != pid);
            wrong_uid += usize::from(observed.uid != host.identity.uid);
            continue;
        }
        let actual = match File::open(format!("/proc/{pid}/exe")) {
            Ok(file) => file,
            Err(_) => {
                unreadable_executable += 1;
                continue;
            }
        };
        if inode(&actual)? != expected {
            wrong_inode += 1;
            continue;
        }
        let fd = unsafe { nix::libc::syscall(nix::libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let candidate = Target {
            identity: observed,
            pidfd: unsafe { File::from_raw_fd(i32::try_from(fd)?) },
        };
        candidate.check(host, executable)?;
        matched.push(candidate);
    }
    if matched.len() != 1 {
        // The owned child set is bounded to 512 entries. Identity-only refusal
        // evidence distinguishes missing/ambiguous targets without a retry or
        // a basename/numeric-PID fallback (issue #50).
        return Err(format!(
            "selected managed executable has no unique live supervised child; no signal sent; matches={}; host={:?}; expected_inode={expected:?}; children={children:?}; unavailable={unavailable}; wrong_parent={wrong_parent}; wrong_group={wrong_group}; wrong_uid={wrong_uid}; unreadable_executable={unreadable_executable}; wrong_inode={wrong_inode}",
            matched.len(),
            host.identity
        )
        .into());
    }
    matched
        .pop()
        .ok_or_else(|| "selected supervised child absent".into())
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("fault receipt lacks {key}").into())
}

pub(super) fn inject(host: &Host, profile: &Path, base: &Path) -> Result<()> {
    let root = base.join("data/among/managed");
    let observed = profile::bounded(
        &storage::safe_path(profile, Path::new("managed/installed.json"))?,
        256 * 1024,
    )?;
    let selection = profile::bounded(
        &storage::safe_path(&root, Path::new("installed.json"))?,
        256 * 1024,
    )?;
    if selection != observed {
        return Err(
            "live selection differs from the validated installed profile; no signal sent".into(),
        );
    }
    let document = storage::parse_json(&selection)?;
    let receipts = document["installations"]
        .as_array()
        .ok_or("live selection lacks installations")?;
    let matching = receipts
        .iter()
        .filter(|receipt| {
            receipt["repository_id"] == 1431055733u64 && receipt["plugin_id"] == "system-status"
        })
        .collect::<Vec<_>>();
    let [receipt] = matching.as_slice() else {
        return Err("resource selection absent or duplicated; no signal sent".into());
    };
    let generation = text(receipt, "generation")?;
    if generation.len() != 64
        || !generation
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("invalid resource generation".into());
    }
    let package = storage::safe_path(
        &root,
        Path::new(&format!(
            "generations/1431055733/system-status/{generation}"
        )),
    )?;
    if storage::parse_json(&profile::bounded(
        &storage::safe_path(&package, Path::new(".among-receipt.json"))?,
        64 * 1024,
    )?)? != **receipt
    {
        return Err("resource seal differs from selected receipt; no signal sent".into());
    }
    let lease = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
        .open(storage::safe_path(&package, Path::new(".among-lease"))?)?;
    if !lease.metadata()?.is_file()
        || unsafe { nix::libc::flock(lease.as_raw_fd(), nix::libc::LOCK_SH | nix::libc::LOCK_NB) }
            != 0
    {
        return Err("resource generation lease unavailable; no signal sent".into());
    }
    let manifest = profile::bounded(
        &storage::safe_path(&package, Path::new("among-plugin.json"))?,
        256 * 1024,
    )?;
    if storage::sha256(manifest.as_slice())? != text(receipt, "manifest_sha256")? {
        return Err("resource manifest differs from receipt; no signal sent".into());
    }
    let manifest = storage::parse_json(&manifest)?;
    let entrypoint = text(&manifest, "entrypoint")?;
    let path = storage::safe_path(&package, Path::new(entrypoint))?;
    let executable = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
        .open(&path)?;
    let metadata = executable.metadata()?;
    if !metadata.is_file() || metadata.mode() & 0o111 == 0 {
        return Err("resource entrypoint is not a regular executable".into());
    }
    let inputs = release::profile_inputs(profile)?;
    let catalogue = release::catalogue(profile, &inputs)?;
    let (_, artifact) = release::select(&catalogue, &crate::platform::discover()?.architecture)?
        .into_iter()
        .find(|(version, _)| version["manifest"]["id"] == "system-status")
        .ok_or("resource artifact absent")?;
    let member = artifact["members"]
        .as_array()
        .ok_or("resource members absent")?
        .iter()
        .find(|member| member["path"] == entrypoint)
        .ok_or("resource entrypoint lacks public digest")?;
    if release::observed_asset(&inputs, text(artifact, "asset")?)?["id"] != receipt["asset_id"]
        || artifact["sha256"] != receipt["package_sha256"]
        || member["executable"] != true
        || member["size"] != metadata.len()
        || storage::sha256(executable.try_clone()?.take(metadata.len() + 1))?
            != text(member, "sha256")?
    {
        return Err(
            "resource executable differs from selected public artifact; no signal sent".into(),
        );
    }
    let target = target(host, &executable)?;
    if profile::bounded(
        &storage::safe_path(&root, Path::new("installed.json"))?,
        256 * 1024,
    )? != selection
        || inode(
            &OpenOptions::new()
                .read(true)
                .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
                .open(&path)?,
        )? != inode(&executable)?
    {
        return Err(
            "resource selection/path changed before fault injection; no signal sent".into(),
        );
    }
    let mut proof = json!({"scope":"targeted renderer fault injection, not installation acceptance","receipt":receipt,"pid":target.identity.pid,"host_pid":host.identity.pid,"start_ticks":target.identity.start,"executable_device":metadata.dev(),"executable_inode":metadata.ino(),"pidfd":true,"signal_sent":false});
    storage::atomic_write(
        base,
        Path::new("resource-fault-target.json"),
        &serde_json::to_vec_pretty(&proof)?,
        0o600,
    )?;
    target.kill(host, &executable)?;
    proof["signal_sent"] = true.into();
    storage::atomic_write(
        base,
        Path::new("resource-fault-target.json"),
        &serde_json::to_vec_pretty(&proof)?,
        0o600,
    )?;
    println!("resource_fault_target=github:1431055733/system-status pid={} receipt_pinned_inode=true pidfd_signal_sent=true",target.identity.pid);
    Ok(())
}

#[cfg(test)]
mod tests;
