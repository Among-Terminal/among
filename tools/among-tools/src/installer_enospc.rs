use crate::process::{allowed_environment, OwnedProcess};
use crate::{ignored_tests, storage, Result};
use nix::sys::statfs::{fstatfs, TMPFS_MAGIC};
use nix::sys::statvfs::{fstatvfs, FsFlags};
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const TEST: &str = "plugin::install::store::tests::real_enospc_before_selection_keeps_previous_installation_and_never_reports_success";
const MOUNT: &str = "/owned-full";
const MAX_BYTES: u64 = 128 * 1024 * 1024;

fn dedicated_mount(mountinfo: &str) -> Result<()> {
    let matches = mountinfo
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            (fields.get(4) == Some(&MOUNT)).then_some(fields)
        })
        .collect::<Vec<_>>();
    let [fields] = matches.as_slice() else {
        return Err("ENOSPC requires exactly one dedicated /owned-full mount".into());
    };
    let separator = fields.iter().position(|value| *value == "-");
    if fields.get(3) != Some(&"/")
        || !separator.is_some_and(|index| {
            fields.get(index + 1) == Some(&"tmpfs") && fields.get(index + 2) == Some(&"tmpfs")
        })
    {
        return Err(
            "ENOSPC fixture must be a dedicated tmpfs, not a bind or host directory".into(),
        );
    }
    Ok(())
}

fn bounded_volume(file: &File) -> Result<u64> {
    let stat = fstatfs(file)?;
    let bytes = stat
        .blocks()
        .checked_mul(u64::try_from(stat.block_size())?)
        .ok_or("ENOSPC fixture capacity overflow")?;
    let flags = fstatvfs(file)?.flags();
    if stat.filesystem_type() != TMPFS_MAGIC
        || bytes == 0
        || bytes > MAX_BYTES
        || !flags.contains(FsFlags::ST_NOSUID | FsFlags::ST_NODEV | FsFlags::ST_NOEXEC)
        || flags.contains(FsFlags::ST_RDONLY)
    {
        return Err(
            "ENOSPC requires a writable nosuid,nodev,noexec tmpfs capped at 128 MiB".into(),
        );
    }
    Ok(bytes)
}

fn select(
    records: &Path,
    base: &Path,
    env: &std::collections::BTreeMap<std::ffi::OsString, std::ffi::OsString>,
) -> Result<PathBuf> {
    let mut selected = None;
    for (index, executable) in ignored_tests::executables(&fs::read_to_string(records)?)?
        .into_iter()
        .enumerate()
    {
        if !fs::symlink_metadata(&executable)?.file_type().is_file() {
            return Err("ENOSPC Cargo test executable must be a regular file".into());
        }
        let path = base.join(format!("listing-{index}.log"));
        let log = File::create(&path)?;
        let mut child =
            OwnedProcess::spawn(&executable, &["--list", "--ignored"], env, base, &log)?;
        if !child.wait(Duration::from_secs(30))?.success() {
            return Err("ENOSPC test enumeration failed".into());
        }
        let count = fs::read_to_string(path)?
            .lines()
            .filter(|line| *line == format!("{TEST}: test"))
            .count();
        if count > 1 || (count == 1 && selected.replace(executable).is_some()) {
            return Err("multiple ENOSPC test declarations are ambiguous".into());
        }
    }
    selected.ok_or_else(|| "Cargo records contain no exact ignored installer ENOSPC test".into())
}

pub fn run(records: &Path, scratch: &Path) -> Result<PathBuf> {
    // an arbitrary small /dev/shm is not an owned fixture: only the explicitly
    // declared container mount is eligible for the destructive fill operation.
    if !Path::new("/.dockerenv").is_file() {
        return Err("run installer ENOSPC in its disposable Docker container".into());
    }
    if nix::unistd::geteuid().is_root() {
        return Err("installer ENOSPC requires the container's non-root test user".into());
    }
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")?;
    dedicated_mount(&mountinfo)?;
    let mount = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
        .open(MOUNT)?;
    let capacity = bounded_volume(&mount)?;
    if fs::read_dir(MOUNT)?.next().is_some() {
        return Err("ENOSPC requires a fresh empty dedicated tmpfs".into());
    }
    let base = storage::private_directory(scratch, "among-installer-enospc")?;
    if fstatfs(&File::open(&base)?)?.filesystem_type() == TMPFS_MAGIC {
        return Err("ENOSPC evidence and ordinary temporary storage must be disk-backed".into());
    }
    fs::write(base.join("mountinfo.txt"), mountinfo)?;
    let mut env = allowed_environment(&std::env::vars_os().collect(), &["PATH", "LANG"]);
    for key in [
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
        "TMPDIR",
    ] {
        let directory = base.join(key);
        fs::create_dir(&directory)?;
        env.insert(key.into(), directory.into_os_string());
    }
    let executable = select(records, &base, &env)?;
    let fixture = storage::private_directory(Path::new(MOUNT), "fixture")?;
    let fixture_file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
        .open(&fixture)?;
    if bounded_volume(&fixture_file)? != capacity
        || fixture_file.metadata()?.dev() != mount.metadata()?.dev()
        || fixture_file.metadata()?.uid() != nix::unistd::geteuid().as_raw()
    {
        return Err("ENOSPC child directory differs from its owned bounded mount".into());
    }
    fs::write(
        base.join("binding.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "test": TEST,
            "executable": executable,
            "sha256": storage::sha256(File::open(&executable)?)?,
            "fixture": fixture,
            "capacity_bytes": capacity,
            "uid": fixture_file.metadata()?.uid(),
            "argv": [TEST, "--exact", "--ignored", "--test-threads=1", "--nocapture"],
        }))?,
    )?;
    env.insert(
        "AMONG_INSTALLER_FULL_DISK_ROOT".into(),
        fixture.clone().into_os_string(),
    );
    let path = base.join("test.log");
    let log = File::create(&path)?;
    let mut child = OwnedProcess::spawn(
        &executable,
        &[
            TEST,
            "--exact",
            "--ignored",
            "--test-threads=1",
            "--nocapture",
        ],
        &env,
        &base,
        &log,
    )?;
    let status = child.wait(Duration::from_secs(90))?;
    fs::write(base.join("status.txt"), format!("{status}\n"))?;
    let output = fs::read_to_string(path)?;
    print!("{output}");
    if !status.success()
        || !output.contains("test result: ok. 1 passed; 0 failed; 0 ignored;")
        || !output.contains("PASS real ENOSPC on owned tmpfs after staging and before selection:")
    {
        return Err(format!(
            "original installer ENOSPC assertion test failed; evidence: {}",
            base.display()
        )
        .into());
    }
    fs::copy(
        fixture.join("installed.json"),
        base.join("installed.after.json"),
    )?;
    if fixture.join("owned-enospc-fixture").exists() {
        return Err("ENOSPC test left its filler behind".into());
    }
    Ok(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_standalone_tmpfs_mount_is_eligible() {
        let valid =
            "42 21 0:91 / /owned-full rw,nosuid,nodev,noexec - tmpfs tmpfs rw,size=131072k\n";
        assert!(dedicated_mount(valid).is_ok());
        for invalid in [
            "",
            "42 21 0:91 / /dev/shm rw,nosuid,nodev,noexec - tmpfs tmpfs rw,size=131072k\n",
            "42 21 0:91 /fixture /owned-full rw - tmpfs tmpfs rw,size=131072k\n",
            "42 21 8:1 / /owned-full rw - ext4 /dev/root rw\n",
            "42 21 0:91 / /owned-full rw -\n",
        ] {
            assert!(dedicated_mount(invalid).is_err(), "{invalid}");
        }
        assert!(dedicated_mount(&format!("{valid}{valid}")).is_err());
    }

    #[test]
    fn a_regular_disk_cannot_be_filled() -> Result<()> {
        assert!(bounded_volume(&File::open(env!("CARGO_MANIFEST_DIR"))?).is_err());
        Ok(())
    }

    #[test]
    fn real_libtest_enumeration_requires_one_exact_artifact_and_propagates_failures() -> Result<()>
    {
        let base = storage::private_directory(&std::env::temp_dir(), "among-enospc-selection")?;
        let executable = base.join("selection-only");
        let compile = std::process::Command::new("rustc")
            .args(["--edition=2021", "--test"])
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/installer-enospc-selection.rs"),
            )
            .arg("-o")
            .arg(&executable)
            .output()?;
        assert!(
            compile.status.success(),
            "{}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let record = |path: &Path| {
            serde_json::json!({
                "reason": "compiler-artifact",
                "profile": {"test": true},
                "executable": path,
            })
            .to_string()
        };
        let records = base.join("cargo.jsonl");
        let env = allowed_environment(&std::env::vars_os().collect(), &["PATH", "LANG"]);
        fs::write(&records, record(&executable))?;
        assert_eq!(select(&records, &base, &env)?, executable);
        let other = base.join("second-artifact");
        fs::copy(&executable, &other)?;
        fs::write(
            &records,
            format!("{}\n{}", record(&executable), record(&other)),
        )?;
        let error = select(&records, &base, &env)
            .err()
            .ok_or("ambiguous artifacts were accepted")?;
        assert!(error.to_string().contains("ambiguous"));
        fs::write(&records, record(Path::new("/usr/bin/true")))?;
        let error = select(&records, &base, &env)
            .err()
            .ok_or("missing exact test was accepted")?;
        assert!(error.to_string().contains("no exact"));
        fs::write(&records, record(Path::new("/usr/bin/false")))?;
        let error = select(&records, &base, &env)
            .err()
            .ok_or("failed enumeration was accepted")?;
        assert!(error.to_string().contains("enumeration failed"));
        Ok(())
    }
}
