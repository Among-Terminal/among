use among_tools::{process::OwnedProcess, storage, Result};
use serde_json::json;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn fixture() -> Result<Fixture> {
    // On disk inside the build's own target directory, so the suite needs no
    // pre-created shared directory and works for an unprivileged build user.
    let f = Fixture(storage::private_directory(
        &Path::new(env!("CARGO_TARGET_TMPDIR")).canonicalize()?,
        "among-snap-rust-fixture",
    )?);
    Ok(f)
}
#[test]
fn actual_host_probe_records_real_cwd_maps_environment_and_refuses_foreign_or_existing_output(
) -> Result<()> {
    let f = fixture()?;
    let output = f.0.join("observed.json");
    let run_under = |marker: &str, root: &Path| {
        std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
            .env_clear()
            .current_dir(&f.0)
            .env("AMONG_HOST_MARKER", marker)
            .env("AMONG_CHECK_ROOT", root)
            .env("HOME", f.0.join("home"))
            .env("PATH", "/synthetic/tools:/usr/bin:/bin")
            .env("LD_LIBRARY_PATH", "")
            .env("GSETTINGS_SCHEMA_DIR", f.0.join("schemas"))
            .env("XDG_DATA_DIRS", f.0.join("data"))
            .arg("snap-host-probe")
            .arg(&output)
            .output()
    };
    let run = |marker: &str| run_under(marker, &f.0);
    let denied = run("not-approved")?;
    assert!(!denied.status.success());
    assert!(!output.exists());
    // Output outside the check root the probe was given is foreign evidence
    // space and must be refused even with the approved marker.
    let foreign = run_under("vm-fixture", &f.0.join("other-root"))?;
    assert!(!foreign.status.success());
    assert!(String::from_utf8(foreign.stderr)?.contains("owned private evidence directory"));
    assert!(!output.exists());
    let observed = run("vm-fixture")?;
    assert!(
        observed.status.success(),
        "{}",
        String::from_utf8_lossy(&observed.stderr)
    );
    let bytes = fs::read(&output)?;
    let record = storage::parse_json(&bytes)?;
    assert_eq!(record["cwd"], f.0.to_str().ok_or("fixture path not UTF-8")?);
    assert_eq!(record["marker"], "vm-fixture");
    assert_eq!(record["path"], "/synthetic/tools:/usr/bin:/bin");
    assert!(record["maps"]
        .as_str()
        .ok_or("maps absent")?
        .contains("libc.so"));
    assert!(!run("vm-fixture")?.status.success());
    assert_eq!(fs::read(output)?, bytes);
    Ok(())
}
#[test]
fn actual_external_plugin_observes_managed_binding_scope_request_ids_and_shutdown_and_refuses_malformed_messages(
) -> Result<()> {
    let f = fixture()?;
    let env: BTreeMap<OsString, OsString> = BTreeMap::new();
    let log = fs::File::create(f.0.join("plugin.stderr"))?;
    let mut process = OwnedProcess::spawn_interactive(
        Path::new(env!("CARGO_BIN_EXE_among-tools")),
        &["snap-external-plugin"],
        &env,
        &f.0,
        &log,
    )?;
    let mut input = process.take_stdin()?;
    for message in [
        json!({"type":"managed_hello","major":1,"minor":2,"identity":"github:12/vm-external","session":"s","generation":"g","features":["status"]}),
        json!({"type":"status","session":"s","generation":"g","request_id":13,"scope":{"slot":"app_header","target":{"kind":"application"}}}),
        json!({"type":"status","session":"s","generation":"g","request_id":17,"scope":{"slot":"pane_footer","target":{"kind":"pane","reference":"1"}}}),
        json!({"type":"shutdown"}),
    ] {
        serde_json::to_writer(&mut input, &message)?;
        input.write_all(b"\n")?;
    }
    drop(input);
    assert!(process.wait(Duration::from_secs(5))?.success());
    let mut stdout = String::new();
    process.take_stdout()?.read_to_string(&mut stdout)?;
    let messages = stdout
        .lines()
        .map(|line| storage::parse_json(line.as_bytes()))
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(messages.len(), 3);
    assert_eq!(
        messages[0],
        json!({"type":"managed_hello","major":1,"minor":2,"identity":"github:12/vm-external","session":"s","generation":"g","features":["status"]})
    );
    assert_eq!(messages[1]["session"], "s");
    assert_eq!(messages[1]["generation"], "g");
    assert_eq!(messages[1]["request_id"], 13);
    assert_eq!(messages[1]["fragments"][0]["text"], "VM-EXTERNAL");
    assert_eq!(messages[2]["request_id"], 17);
    assert_eq!(messages[2]["fragments"], json!([]));
    for bytes in [
        b"not JSON\n".as_slice(),
        b"{\"type\":\"hello\"}".as_slice(),
        b"{\"type\":\"status\",\"scope\":\"app_header\"}\n".as_slice(),
        b"{\"type\":\"managed_hello\",\"major\":1,\"minor\":1}\n".as_slice(),
    ] {
        let mut process = OwnedProcess::spawn_interactive(
            Path::new(env!("CARGO_BIN_EXE_among-tools")),
            &["snap-external-plugin"],
            &env,
            &f.0,
            &log,
        )?;
        let mut input = process.take_stdin()?;
        input.write_all(bytes)?;
        drop(input);
        assert!(!process.wait(Duration::from_secs(5))?.success());
    }
    Ok(())
}
#[test]
fn actual_runtime_cli_refuses_unowned_display_and_unported_replacement_option() -> Result<()> {
    for args in [
        vec!["check-snap-runtime", "--github-hosted"],
        vec!["check-snap-runtime", "--replace-vte"],
    ] {
        let checks = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("among-snap-runtime-refusal-{}", std::process::id()));
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
            .env_clear()
            .env("AMONG_CHECK_ROOT", &checks)
            .args(&args)
            .output()?;
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        if checks.exists() {
            assert_eq!(fs::read_dir(&checks)?.count(), 0, "{args:?}");
            fs::remove_dir(&checks)?;
        }
    }
    Ok(())
}
