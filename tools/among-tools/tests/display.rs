use among_tools::display;
use std::io::{self, Read};
use std::time::Duration;

struct PartialLine {
    bytes: std::collections::VecDeque<u8>,
    calls: usize,
}
impl Read for PartialLine {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.calls += 1;
        if self.calls.is_multiple_of(2) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        match self.bytes.pop_front() {
            Some(byte) => {
                output[0] = byte;
                Ok(1)
            }
            None => Ok(0),
        }
    }
}

#[test]
fn partial_display_number_waits_for_complete_line_and_rejects_bad_values() -> among_tools::Result<()>
{
    let mut line = PartialLine {
        bytes: b"123\n".iter().copied().collect(),
        calls: 0,
    };
    assert_eq!(
        display::read_display(&mut line, Duration::from_secs(1))?,
        ":123"
    );
    for mut bytes in [b"\n".as_slice(), b"bad\n", b"123", b"12345678901\n"] {
        assert!(display::read_display(&mut bytes, Duration::from_secs(1)).is_err());
    }
    let mut line = PartialLine {
        bytes: b"123\n".iter().copied().collect(),
        calls: 1,
    };
    assert!(display::read_display(&mut line, Duration::ZERO).is_err());
    Ok(())
}

#[test]
fn openbox_retains_unrelated_bindings_and_replaces_maximize_only() -> among_tools::Result<()> {
    let source = r#"<openbox_config xmlns="http://openbox.org/3.4/rc"><keyboard><keybind key="A-F4"><action name="Close"/></keybind><keybind key="A-F10"><action name="Other"/></keybind></keyboard></openbox_config>"#;
    let output = display::openbox_configuration(source)?;
    assert_eq!(output.matches("A-F10").count(), 1);
    assert!(
        output.contains("A-F4") && output.contains("Close") && output.contains("ToggleMaximize")
    );
    assert!(!output.contains("Other"));
    assert!(display::openbox_configuration("<bad>").is_err());
    assert!(display::openbox_configuration("<config/>").is_err());
    Ok(())
}
#[test]
fn actual_environment_probe_preserves_both_hosted_markers_without_forwarding_credentials(
) -> among_tools::Result<()> {
    use std::{collections::BTreeMap, ffi::OsString, fs, path::Path, time::Duration};
    let caller: BTreeMap<OsString, OsString> = BTreeMap::from([
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("GITHUB_ACTIONS".into(), "true".into()),
        ("RUNNER_ENVIRONMENT".into(), "github-hosted".into()),
        ("RUNNER_ENV".into(), "wrong-marker".into()),
        ("GH_TOKEN".into(), "synthetic-secret".into()),
    ]);
    let env = among_tools::display::launch_environment(&caller);
    let base = among_tools::storage::private_directory(&std::env::temp_dir(), "among-display-env")?;
    let log = base.join("environment.log");
    let file = fs::File::create(&log)?;
    let mut process = among_tools::process::OwnedProcess::spawn(
        Path::new("/usr/bin/env"),
        &[],
        &env,
        &std::env::temp_dir(),
        &file,
    )?;
    assert!(process.wait(Duration::from_secs(5))?.success());
    let text = fs::read_to_string(&log)?;
    fs::remove_file(log)?;
    fs::remove_dir(base)?;
    assert!(text.lines().any(|line| line == "GITHUB_ACTIONS=true"));
    assert!(text
        .lines()
        .any(|line| line == "RUNNER_ENVIRONMENT=github-hosted"));
    assert!(!text.contains("synthetic-secret"));
    assert!(!text.contains("wrong-marker"));
    Ok(())
}
