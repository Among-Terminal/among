use super::*;
use crate::process::OwnedProcess;
use std::{
    collections::BTreeMap,
    os::unix::{fs::PermissionsExt, process::ExitStatusExt},
    time::Duration,
};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Result<Self> {
        Ok(Self(storage::private_directory(
            &std::env::temp_dir(),
            "among-pinned-fault-test",
        )?))
    }
    fn program(&self, directory: &str) -> Result<PathBuf> {
        let directory = self.0.join(directory);
        fs::create_dir(&directory)?;
        let path = directory.join("same-name");
        fs::copy("/bin/sleep", &path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        Ok(path)
    }
    fn spawn(&self, path: &Path) -> Result<OwnedProcess> {
        let name = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .ok_or("fixture name absent")?;
        let log = File::create(self.0.join(format!("{name}.log")))?;
        OwnedProcess::spawn(path, &["30"], &BTreeMap::new(), &self.0, &log)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn pinned_inode_selects_worker_thread_child_without_signalling_same_basename_sibling() -> Result<()>
{
    let fixture = Fixture::new()?;
    let selected = fixture.program("selected")?;
    let unrelated = fixture.program("unrelated")?;
    let mut sibling = fixture.spawn(&unrelated)?;
    let base = fixture.0.clone();
    let program = selected.clone();
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let (finish, finished) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || -> Result<()> {
        let log = File::create(base.join("worker.log"))?;
        let child = OwnedProcess::spawn(&program, &["30"], &BTreeMap::new(), &base, &log)?;
        send.send(child)
            .map_err(|_| "fixture child receiver closed")?;
        finished
            .recv()
            .map_err(|_| "fixture thread completion cancelled")?;
        Ok(())
    });
    let mut child = receive.recv_timeout(Duration::from_secs(5))?;
    let host = Host::observe(std::process::id())?;
    let executable = File::open(&selected)?;
    let target = target(&host, &executable)?;
    assert_eq!(target.identity.pid, child.id());
    assert_ne!(inode(&File::open(&unrelated)?)?, inode(&executable)?);
    target.kill(&host, &executable)?;
    assert_eq!(
        child.wait(Duration::from_secs(5))?.signal(),
        Some(nix::libc::SIGKILL)
    );
    assert!(
        sibling.is_running()?,
        "same-basename sibling must remain live"
    );
    finish.send(())?;
    worker
        .join()
        .map_err(|_| "fixture spawning thread failed")??;
    Ok(())
}

#[test]
fn missing_or_ambiguous_selected_inode_refuses_without_signalling_owned_children() -> Result<()> {
    let fixture = Fixture::new()?;
    let selected = fixture.program("selected")?;
    let unrelated = fixture.program("unrelated")?;
    let mut sibling = fixture.spawn(&unrelated)?;
    let host = Host::observe(std::process::id())?;
    let executable = File::open(&selected)?;
    assert!(target(&host, &executable).is_err());
    assert!(sibling.is_running()?);
    let mut first = fixture.spawn(&selected)?;
    let mut second = fixture.spawn(&selected)?;
    assert!(target(&host, &executable).is_err());
    assert!(first.is_running()? && second.is_running()? && sibling.is_running()?);
    Ok(())
}

#[test]
fn exited_target_is_not_replaced_by_a_new_process_of_the_same_executable() -> Result<()> {
    let fixture = Fixture::new()?;
    let selected = fixture.program("selected")?;
    let mut child = fixture.spawn(&selected)?;
    let host = Host::observe(std::process::id())?;
    let executable = File::open(&selected)?;
    let target = target(&host, &executable).inspect_err(|_| {
        eprintln!(
            "initial fault fixture selection: intended_pid={}; running={:?}; executable_inode={:?}",
            child.id(),
            child.is_running(),
            inode(&executable)
        );
    })?;
    child.signal(nix::sys::signal::Signal::SIGTERM)?;
    child.wait(Duration::from_secs(5))?;
    let mut replacement = fixture.spawn(&selected)?;
    assert!(target.kill(&host, &executable).is_err());
    assert!(
        replacement.is_running()?,
        "stale target must not signal a replacement"
    );
    Ok(())
}

#[test]
fn an_executable_child_of_another_host_is_not_a_fault_target() -> Result<()> {
    let fixture = Fixture::new()?;
    let selected = fixture.program("selected")?;
    let other = fixture.program("other-host")?;
    let mut child = fixture.spawn(&selected)?;
    let mut other_host = fixture.spawn(&other)?;
    let host = Host::observe(other_host.id())?;
    assert!(target(&host, &File::open(&selected)?).is_err());
    assert!(child.is_running()? && other_host.is_running()?);
    Ok(())
}

#[test]
fn changed_missing_and_duplicate_selection_refuse_before_any_fault_signal() -> Result<()> {
    let fixture = Fixture::new()?;
    let selected = fixture.program("selected")?;
    let mut child = fixture.spawn(&selected)?;
    let host = Host::observe(std::process::id())?;
    let profile = fixture.0.join("profile");
    let base = fixture.0.join("renderer");
    fs::create_dir_all(profile.join("managed"))?;
    fs::create_dir_all(base.join("data/among/managed"))?;
    for mutation in ["changed", "missing", "duplicate"] {
        // refusal fixtures only, not fabricated public installation evidence.
        let receipt = json!({"repository_id":1431055733u64,"plugin_id":"system-status"});
        let original = match mutation {
            "duplicate" => json!({"version":1,"installations":[receipt.clone(),receipt]}),
            _ => json!({"version":1,"installations":[]}),
        };
        let bytes = serde_json::to_vec(&original)?;
        fs::write(profile.join("managed/installed.json"), &bytes)?;
        fs::write(
            base.join("data/among/managed/installed.json"),
            if mutation == "changed" {
                b"{}".as_slice()
            } else {
                bytes.as_slice()
            },
        )?;
        assert!(inject(&host, &profile, &base).is_err(), "{mutation}");
        assert!(!base.join("resource-fault-target.json").exists());
        assert!(
            child.is_running()?,
            "selection refusal must not signal a child"
        );
    }
    Ok(())
}
