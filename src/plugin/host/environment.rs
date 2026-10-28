//! managed children receive an explicit environment, not the parent's execution
//! authority. timezone bytes are forwarded, never interpreted by the host.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::process::Command;

const MAX_TZ_BYTES: usize = 1024;
const INVALID_TZ: &str = "managed TZ exceeds 1024 bytes or contains ASCII control bytes";

pub(super) fn configure(program: &mut Command) -> Result<(), &'static str> {
    let timezone = std::env::var_os("TZ");
    configure_timezone(program, timezone.as_deref())
}

fn configure_timezone(program: &mut Command, timezone: Option<&OsStr>) -> Result<(), &'static str> {
    if timezone.is_some_and(|value| {
        let bytes = value.as_bytes();
        bytes.len() > MAX_TZ_BYTES || bytes.iter().any(u8::is_ascii_control)
    }) {
        return Err(INVALID_TZ);
    }
    program
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("LANG", "C.UTF-8");
    for variable in [
        "HOME",
        "XDG_DATA_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_RUNTIME_DIR",
    ] {
        if let Some(value) = std::env::var_os(variable) {
            program.env(variable, value);
        }
    }
    if let Some(timezone) = timezone {
        program.env("TZ", timezone);
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn capture_for_test(mut command: Command, input: &[u8]) -> Vec<u8> {
    use std::io::{Read, Write};
    use std::process::{Child, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    struct Owned(Child);
    impl Drop for Owned {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_none() {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }

    // each reference gets a fresh empty pipe; a PIPE_BUF-sized input cannot
    // wait for a reader. stdout and exit are observed under one deadline.
    assert!(input.len() <= 4096);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut child = Owned(command.spawn().unwrap());
    let stdout = child.0.stdout.take().unwrap();
    let (sender, output) = mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.take(64 * 1024 + 1).read_to_end(&mut bytes);
        let _ = sender.send((result, bytes));
    });
    child.0.stdin.take().unwrap().write_all(input).unwrap();
    let (read, bytes) = output
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .unwrap();
    read.unwrap();
    assert!(bytes.len() <= 64 * 1024);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(
                status.success(),
                "owned environment/reference process failed: {status}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "owned reference process did not exit"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    while !reader.is_finished() {
        assert!(
            Instant::now() < deadline,
            "owned reference reader did not exit"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    reader.join().unwrap();
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    fn observed_environment(timezone: Option<&OsStr>) -> Vec<Vec<u8>> {
        let mut command = Command::new("/usr/bin/env");
        command
            .arg("-0")
            .env("TZ", "must-not-survive-clear")
            .env("TZDIR", "/must-not-be-inherited")
            .env("LD_PRELOAD", "/must-not-be-loaded")
            .env("AMONG_UNSELECTED_ENVIRONMENT", "not-authorized");
        configure_timezone(&mut command, timezone).unwrap();
        let output = capture_for_test(command, &[]);
        let fields = output
            .split(|byte| *byte == 0)
            .filter(|field| !field.is_empty())
            .map(<[u8]>::to_vec)
            .collect::<Vec<_>>();
        assert!(fields.iter().all(|field| {
            let equal = field.iter().position(|byte| *byte == b'=').unwrap();
            matches!(
                &field[..equal],
                b"PATH"
                    | b"LANG"
                    | b"HOME"
                    | b"XDG_DATA_HOME"
                    | b"XDG_CONFIG_HOME"
                    | b"XDG_CACHE_HOME"
                    | b"XDG_RUNTIME_DIR"
                    | b"TZ"
            )
        }));
        assert!(fields.contains(&b"PATH=/usr/local/bin:/usr/bin:/bin".to_vec()));
        assert!(fields.contains(&b"LANG=C.UTF-8".to_vec()));
        fields
    }

    fn timezone(fields: &[Vec<u8>]) -> Option<&[u8]> {
        let values = fields
            .iter()
            .filter_map(|field| field.strip_prefix(b"TZ="))
            .collect::<Vec<_>>();
        assert!(values.len() <= 1);
        values.first().copied()
    }

    #[test]
    fn managed_timezone_absent_and_empty_remain_distinct_in_actual_children() {
        assert_eq!(timezone(&observed_environment(None)), None);
        assert_eq!(
            timezone(&observed_environment(Some(OsStr::new("")))),
            Some(b"".as_slice())
        );
    }

    #[test]
    fn managed_timezone_budget_accepts_1024_bytes_and_rejects_the_next_byte() {
        let accepted = OsString::from("x".repeat(1024));
        assert_eq!(
            timezone(&observed_environment(Some(&accepted))),
            Some(accepted.as_bytes())
        );
        let mut command = Command::new("/usr/bin/env");
        assert_eq!(
            configure_timezone(&mut command, Some(&OsString::from("x".repeat(1025)))),
            Err(INVALID_TZ)
        );
    }

    #[test]
    fn managed_timezone_preserves_posix_file_and_non_utf8_bytes() {
        for value in [
            OsString::from("UTC-2"),
            OsString::from(" UTC "),
            OsString::from(":Asia/Tokyo"),
            OsString::from(":/usr/share/zoneinfo/./UTC"),
            OsString::from(":/usr/share/zoneinfo/Asia/Tokyo"),
            OsString::from("/usr/share/zoneinfo/UTC"),
            OsString::from_vec(vec![b'U', 0x80, 0x9f, 0xc2, 0x80, 0xff]),
        ] {
            assert_eq!(
                timezone(&observed_environment(Some(&value))),
                Some(value.as_bytes())
            );
        }
    }

    #[test]
    fn managed_timezone_rejects_every_ascii_control_before_command_mutation() {
        for byte in (0..=0x1f).chain(std::iter::once(0x7f)) {
            let value = OsString::from_vec(vec![b'U', byte, b'T']);
            let mut command = Command::new("/usr/bin/env");
            command.env("AMONG_ENVIRONMENT_UNTOUCHED", "before-validation");
            assert_eq!(
                configure_timezone(&mut command, Some(&value)),
                Err(INVALID_TZ)
            );
            assert_eq!(command.get_envs().count(), 1);
            assert!(command.get_envs().any(|(key, value)| {
                key == "AMONG_ENVIRONMENT_UNTOUCHED"
                    && value == Some(OsStr::new("before-validation"))
            }));
        }
    }
}
