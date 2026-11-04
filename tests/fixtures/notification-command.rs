// synthetic adversarial executable, compiled by Rust tests; not shipped.
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

extern "C" {
    fn signal(signal: i32, handler: usize) -> usize;
    fn getppid() -> i32;
    fn raise(signal: i32) -> i32;
}

fn main() {
    let arguments: Vec<_> = std::env::args().collect();
    let mode = &arguments[1];
    let root = Path::new(&arguments[2]);
    let pid = std::process::id();
    fs::write(
        root.join(format!("supervisor-{pid}")),
        unsafe { getppid() }.to_string(),
    )
    .unwrap();
    if mode == "child" || mode == "grandchild" {
        unsafe {
            signal(15, 1);
        }
        fs::write(root.join(format!("child-{pid}")), pid.to_string()).unwrap();
        if mode == "child" {
            spawn(root, "grandchild");
        }
        loop {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    fs::write(root.join(format!("started-{pid}")), pid.to_string()).unwrap();
    if mode == "hang" || mode == "tree" {
        unsafe {
            signal(15, 1);
        }
        if mode == "tree" {
            spawn(root, "child");
        }
        loop {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    if mode == "inherited" {
        spawn(root, "child");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while fs::read_dir(root)
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("child-")
            })
            .count()
            < 2
        {
            assert!(
                std::time::Instant::now() < deadline,
                "inherited-pipe tree did not start"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        return;
    }
    let mut payload = Vec::new();
    std::io::stdin().read_to_end(&mut payload).unwrap();
    fs::write(root.join(format!("payload-{pid}.json")), &payload).unwrap();
    fs::write(root.join(format!("eof-{pid}")), b"actual EOF observed").unwrap();
    fs::write(
        root.join(format!("argv-{pid}.bin")),
        arguments[3..].join("\0"),
    )
    .unwrap();
    let mut environment: Vec<_> = std::env::vars()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    environment.sort();
    fs::write(root.join(format!("env-{pid}.bin")), environment.join("\0")).unwrap();
    fs::write(
        root.join(format!("cwd-{pid}.txt")),
        std::env::current_dir()
            .unwrap()
            .as_os_str()
            .as_encoded_bytes(),
    )
    .unwrap();
    let descriptors = fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter_map(|entry| {
            fs::read_link(entry.ok()?.path())
                .ok()
                .map(|path| path.to_string_lossy().into_owned())
        })
        .collect::<Vec<_>>();
    fs::write(root.join(format!("fds-{pid}.txt")), descriptors.join("\n")).unwrap();
    if mode == "combined" {
        let _ = std::io::stdout().write_all(&vec![b'o'; 20 * 1024]);
        let _ = std::io::stderr().write_all(&vec![b'e'; 20 * 1024]);
    }
    if mode == "exit7" {
        std::process::exit(7);
    }
    if mode == "signal" {
        unsafe {
            raise(10);
        }
        std::process::exit(99);
    }
    fs::write(root.join(format!("finished-{pid}")), b"local fixture exit").unwrap();
}

fn spawn(root: &Path, mode: &str) {
    Command::new(std::env::current_exe().unwrap())
        .arg(mode)
        .arg(root)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
}
