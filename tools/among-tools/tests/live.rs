#[test]
fn unsolicited_escape_consumes_the_next_command_character_in_real_bash_readline(
) -> among_tools::Result<()> {
    use std::{fs, process::Command};
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let base =
        among_tools::storage::private_directory(&std::env::temp_dir(), "among-readline-prefix")?;
    let _cleanup = Scratch(base.clone());
    for escape in [false, true] {
        let input = base.join("input");
        fs::write(
            &input,
            if escape {
                b"\x1bcd /tmp\rpwd\rexit\r".as_slice()
            } else {
                b"cd /tmp\rpwd\rexit\r".as_slice()
            },
        )?;
        let output = Command::new("/usr/bin/script")
            .args([
                "--quiet",
                "--return",
                "--command",
                "/bin/bash --noprofile --norc -i",
                "/dev/null",
            ])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("TERM", "dumb")
            .env("INPUTRC", "/dev/null")
            .current_dir(&base)
            .stdin(fs::File::open(input)?)
            .output()?;
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout)?;
        if escape {
            assert!(text.contains("bash: d: command not found"), "{text}");
            assert!(
                text.contains(&format!(
                    "\r\n{}\r\n",
                    base.to_str().ok_or("fixture path not UTF-8")?
                )),
                "{text}"
            );
            assert!(!text.contains("\r\n/tmp\r\n"), "{text}");
        } else {
            assert!(!text.contains("command not found"), "{text}");
            assert!(text.contains("\r\n/tmp\r\n"), "{text}");
        }
    }
    fs::remove_dir_all(base)?;
    Ok(())
}

#[test]
fn status_startup_does_not_inject_readline_meta_prefix() -> among_tools::Result<()> {
    let source = include_str!("../src/live_status.rs");
    let startup = source
        .split_once("let pid = h.start(\"app\")?;")
        .ok_or("status startup absent")?
        .1
        .split_once("h.key(\"F11\")?")
        .ok_or("fullscreen check absent")?
        .0;
    assert!(
        !startup.contains("h.key(\"Escape\")"),
        "unsolicited Escape must not reach the fresh shell"
    );
    Ok(())
}

#[test]
fn documented_osc7_hook_encodes_physical_directory_without_external_commands(
) -> among_tools::Result<()> {
    use std::{fs, os::unix::fs::symlink, path::Path, process::Command};
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let docs = fs::read_to_string(root.join("docs/plugins.md"))?;
    let hook = docs
        .split_once("among_osc7() {")
        .and_then(|(_, tail)| tail.split_once("```"))
        .ok_or("documented OSC7 hook absent")?
        .0;
    let base = among_tools::storage::private_directory(&std::env::temp_dir(), "among-osc7")?;
    let name = "space %?#'目录\n";
    let directory = base.join(name);
    fs::create_dir(&directory)?;
    let alias = base.join("alias");
    symlink(&directory, &alias)?;
    let result = (|| -> among_tools::Result<()> {
        let output = Command::new("/bin/bash")
            .env_clear()
            .env("PATH", "/no-external-commands")
            .current_dir(&alias)
            .args([
                "--noprofile",
                "--norc",
                "-c",
                &format!("among_osc7() {{{hook}"),
            ])
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let prefix = directory.parent().ok_or("parent missing")?.canonicalize()?;
        let expected = format!(
            "\x1b]7;file://{}/space%20%25%3F%23%27%E7%9B%AE%E5%BD%95%0A\x07",
            prefix.display()
        );
        assert_eq!(output.stdout, expected.as_bytes());
        assert!(output.stderr.is_empty());
        Ok(())
    })();
    fs::remove_dir_all(base)?;
    result
}

#[test]
fn native_live_commands_refuse_unowned_display() -> among_tools::Result<()> {
    for args in [
        vec!["check-live-focus", "/bin/true"],
        vec![
            "check-live-status",
            "/unread-source",
            "/bin/true",
            "/unread-plugins",
        ],
        vec!["check-desktop", "/", "/bin/true"],
        vec!["check-context-menu-launch", "/bin/true"],
        vec![
            "check-context-menu",
            "/bin/true",
            "/unread-clipboard",
            "light",
        ],
        vec![
            "check-context-menu-snap",
            "/unread-clipboard",
            "light",
            "--github-hosted",
        ],
        vec![
            "desktop-session",
            "/synthetic-personal-home",
            "/unread-entry",
        ],
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
            .env_clear()
            .env("HOME", "/synthetic-personal-home")
            .args(args)
            .output()?;
        assert!(!output.status.success());
        assert!(String::from_utf8(output.stderr)?.contains("owned display runner"));
    }
    Ok(())
}
