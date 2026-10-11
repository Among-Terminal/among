use among_tools::process::{allowed_environment, OwnedProcess};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use std::time::Instant;

#[test]
fn filtering_and_real_child_output_exclude_credentials() -> among_tools::Result<()> {
    let input = BTreeMap::from([
        (OsString::from("VISIBLE"), OsString::from("approved")),
        (OsString::from("TOKEN"), OsString::from("synthetic-secret")),
    ]);
    let env = allowed_environment(&input, &["VISIBLE"]);
    assert_eq!(env.len(), 1);
    let base = among_tools::storage::private_directory(&std::env::temp_dir(), "among-process-env")?;
    let path = base.join("environment.log");
    let log = fs::File::create(&path)?;
    let mut process = OwnedProcess::spawn(
        Path::new("/bin/sh"),
        &["-c", "printf '%s:%s' \"$VISIBLE\" \"${TOKEN-absent}\""],
        &env,
        Path::new("/tmp"),
        &log,
    )?;
    assert!(process.wait(Duration::from_secs(2))?.success());
    assert_eq!(fs::read_to_string(&path)?, "approved:absent");
    fs::remove_file(path)?;
    fs::remove_dir(base)?;
    Ok(())
}

#[test]
fn timeout_drop_and_spawn_failure_preserve_unrelated_process() -> among_tools::Result<()> {
    let mut unrelated = Command::new("/bin/sleep").arg("30").spawn()?;
    let log = fs::OpenOptions::new().write(true).open("/dev/null")?;
    let env = BTreeMap::new();
    let attempt = OwnedProcess::spawn(
        Path::new("/no-such-owned-executable"),
        &[],
        &env,
        Path::new("/tmp"),
        &log,
    );
    assert!(attempt.is_err());
    let mut owned = OwnedProcess::spawn(
        Path::new("/bin/sleep"),
        &["30"],
        &env,
        Path::new("/tmp"),
        &log,
    )?;
    let pid = owned.id();
    assert!(owned.wait(Duration::from_millis(20)).is_err());
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
    assert!(unrelated.try_wait()?.is_none());
    {
        let owned = OwnedProcess::spawn(
            Path::new("/bin/sleep"),
            &["30"],
            &env,
            Path::new("/tmp"),
            &log,
        )?;
        let pid = owned.id();
        drop(owned);
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }
    assert!(unrelated.try_wait()?.is_none());
    unrelated.kill()?;
    unrelated.wait()?;
    Ok(())
}

#[test]
fn timeout_stops_owned_descendant_as_well_as_group_leader() -> among_tools::Result<()> {
    let base =
        among_tools::storage::private_directory(&std::env::temp_dir(), "among-process-child")?;
    let path = base.join("child.log");
    let log = fs::File::create(&path)?;
    let mut owned = OwnedProcess::spawn(
        Path::new("/bin/sh"),
        &["-c", "/bin/sleep 30 & echo $!; wait"],
        &BTreeMap::new(),
        Path::new("/tmp"),
        &log,
    )?;
    let deadline = Instant::now() + Duration::from_secs(2);
    let descendant: u32 = loop {
        if let Ok(pid) = fs::read_to_string(&path)?.trim().parse() {
            break pid;
        }
        if Instant::now() >= deadline {
            return Err("owned descendant never became ready".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(owned.wait(Duration::from_millis(20)).is_err());
    let state = fs::read_to_string(format!("/proc/{descendant}/stat"));
    if let Ok(state) = state {
        let (_, suffix) = state
            .rsplit_once(')')
            .ok_or("invalid descendant proc state")?;
        assert!(
            suffix.trim_start().starts_with('Z'),
            "descendant remains live: {state}"
        );
    }
    fs::remove_file(path)?;
    fs::remove_dir(base)?;
    Ok(())
}

#[test]
fn observing_exit_keeps_leader_identity_until_owned_group_cleanup() -> among_tools::Result<()> {
    let log = fs::OpenOptions::new().write(true).open("/dev/null")?;
    let mut child = OwnedProcess::spawn(
        Path::new("/bin/false"),
        &[],
        &BTreeMap::new(),
        Path::new("/tmp"),
        &log,
    )?;
    let pid = child.id();
    let deadline = Instant::now() + Duration::from_secs(2);
    while child.is_running()? {
        if Instant::now() >= deadline {
            return Err("exit observation timed out".into());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    assert!(stat
        .rsplit_once(')')
        .ok_or("invalid child state")?
        .1
        .trim_start()
        .starts_with('Z'));
    assert_eq!(child.wait(Duration::from_secs(1))?.code(), Some(1));
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
    assert_eq!(child.wait(Duration::from_secs(1))?.code(), Some(1));
    Ok(())
}
