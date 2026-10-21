use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

fn busy_executable() -> (PathBuf, Command, std::fs::File) {
    let root = std::env::var_os("AMONG_CHECK_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("opencode"))
        .join(format!("among-busy-exec-{}", glib::uuid_string_random()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.join("plugin");
    let fixture = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/fake-plugin");
    std::fs::copy(fixture, &path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    assert_eq!(
        Command::new(&path)
            .arg("echo")
            .spawn()
            .unwrap_err()
            .raw_os_error(),
        Some(libc::ETXTBSY),
        "the kernel must actually refuse this inode while its writer is held"
    );
    let directory = super::super::data::owned_directory(&root).unwrap();
    let executable = super::super::data::contained_file(&directory, "plugin").unwrap();
    let mut command = pinned_program(directory, executable);
    command.arg("echo");
    (root, command, writer)
}

fn until(predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(4);
    while !predicate() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn a_real_busy_pinned_executable_retries_off_loop_then_starts_the_same_inode() {
    let (root, command, writer) = busy_executable();
    let start = Instant::now();
    let process = Process::start(command).unwrap();
    assert!(start.elapsed() < Duration::from_millis(100));
    process
        .send(&Request::Hello { major: 1, minor: 2 })
        .unwrap();
    std::thread::sleep(Duration::from_millis(50));
    assert!(!process.started());
    assert!(process.failure().is_none());
    drop(writer);
    let response = std::cell::RefCell::new(None);
    until(|| {
        *response.borrow_mut() = process
            .frames
            .try_recv()
            .ok()
            .or_else(|| response.borrow().clone());
        response.borrow().is_some()
    });
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(response.borrow().as_ref().unwrap()).unwrap()
            ["type"],
        "hello"
    );
    assert!(process.started());
    process.stop();
    until(|| process.finished());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn persistent_real_executable_busy_has_a_finite_startup_deadline() {
    let (root, command, writer) = busy_executable();
    let start = Instant::now();
    let process = Process::start(command).unwrap();
    let failed = std::cell::Cell::new(false);
    until(|| {
        if let Some(failure) = process.failure() {
            assert!(
                matches!(failure,Failure::Missing(reason) if reason.contains("Text file busy"))
            );
            failed.set(true);
        }
        failed.get()
    });
    assert!(start.elapsed() >= SPAWN_BUSY_TIMEOUT);
    assert!(!process.started());
    until(|| process.finished());
    drop(writer);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn cancellation_reaps_a_busy_start_without_waiting_for_the_retry_deadline() {
    let (root, command, writer) = busy_executable();
    let process = Process::start(command).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let start = Instant::now();
    process.stop();
    until(|| process.finished());
    assert!(start.elapsed() < Duration::from_millis(500));
    assert!(!process.started());
    drop(writer);
    std::fs::remove_dir_all(root).unwrap();
}
