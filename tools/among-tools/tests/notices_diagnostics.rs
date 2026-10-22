use among_tools::{notices, storage, Result};
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn actual_cli_retains_exact_private_errors_but_only_exports_bounded_safe_metadata() -> Result<()> {
    let fixture = Fixture(storage::private_directory(
        &std::env::temp_dir(),
        "among-notices-diagnostics-test",
    )?);
    let bin = fixture.0.join("bin");
    let output = fixture.0.join("THIRD_PARTY");
    fs::create_dir(&bin)?;
    fs::create_dir(&output)?;
    fs::write(output.join("inventory.json"), b"retained inventory")?;
    fs::write(output.join("license.txt"), b"retained authentic bytes")?;
    fs::write(bin.join("graph.json"), br#"{"packages":[]}"#)?;
    for manifest in notices::MANIFESTS {
        let lock = fixture.0.join(manifest).with_file_name("Cargo.lock");
        fs::create_dir_all(lock.parent().ok_or("lock parent absent")?)?;
        fs::write(
            lock,
            "version=4\n[[package]]\nname='fixture'\nversion='1.0.0'\n",
        )?;
    }
    let compile = Command::new("rustc")
        .arg("--edition=2021")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/notices-cargo.rs"))
        .arg("-o")
        .arg(bin.join("cargo"))
        .output()?;
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    for mode in ["check", "generate"] {
        for (index, manifest) in notices::MANIFESTS.iter().enumerate() {
            let checks = fixture.0.join(format!("checks-{mode}-{index}"));
            let mut raw = if index == 0 {
                b"error: failed to download `fixture v1.0.0`\nCaused by:\n --offline was specified\n".to_vec()
            } else if index == 1 {
                b"error: failed to download `private-token v1.0.0`\nCaused by:\n --offline was specified\n".to_vec()
            } else {
                b"error: unrecognized external failure\n".to_vec()
            };
            raw.extend_from_slice(b"\x1b[31mhttps://user:password@example.test/source?token=private-token\nAuthorization: Bearer private-credential\nHOME=/private/profile\nsettings=private-settings\n\xff");
            raw.extend(std::iter::repeat_n(b'x', 128 * 1024));
            fs::write(bin.join("failure-manifest"), manifest)?;
            fs::write(bin.join("failure-stderr"), &raw)?;
            fs::write(bin.join("calls"), b"")?;

            let result = Command::new(env!("CARGO_BIN_EXE_among-tools"))
                .env_clear()
                .env("PATH", &bin)
                .env("HOME", "/private/profile")
                .env("CARGO_HOME", "/private/cache")
                .env("AMONG_CHECK_ROOT", &checks)
                .args(["third-party-notices"])
                .arg(&fixture.0)
                .arg(&output)
                .arg(mode)
                .output()?;
            assert!(!result.status.success());
            assert!(result.stdout.is_empty());
            let error = String::from_utf8_lossy(&result.stderr);
            assert!(
                error.contains(&format!("graph {index} ({manifest})")),
                "{error}"
            );
            assert!(
                error.contains(&format!("exit status: {}", 37 + index)),
                "{error}"
            );
            assert!(result.stderr.len() < 2048);
            let directories = fs::read_dir(&checks)?.collect::<std::io::Result<Vec<_>>>()?;
            assert_eq!(directories.len(), 1);
            let evidence = directories[0].path();
            assert_eq!(fs::metadata(&evidence)?.permissions().mode() & 0o077, 0);
            assert_eq!(
                fs::read(evidence.join(format!("metadata-{index}.stderr")))?,
                raw
            );
            let public_bytes = fs::read(evidence.join("metadata-failure.json"))?;
            assert!(public_bytes.len() < 1024);
            let public = storage::parse_json(&public_bytes)?;
            assert_eq!(public["schema"], 1);
            assert_eq!(public["graph_index"], index);
            assert_eq!(public["manifest"], *manifest);
            assert_eq!(public["status"], format!("exit status: {}", 37 + index));
            assert_eq!(public["stderr_bytes"], raw.len());
            if index == 0 {
                assert_eq!(
                    public["diagnostic"],
                    "offline metadata requires download of locked crate fixture v1.0.0"
                );
            } else if index == 1 {
                assert_eq!(
                    public["diagnostic"],
                    "offline metadata requires an unavailable download"
                );
            } else {
                assert_eq!(
                    public["diagnostic"],
                    "Cargo diagnostic withheld; inspect the private raw stderr"
                );
            }
            for text in [&*error, std::str::from_utf8(&public_bytes)?] {
                for secret in [
                    "https://",
                    "password",
                    "private-token",
                    "private-credential",
                    "/private/profile",
                    "private-settings",
                    "\x1b",
                ] {
                    assert!(!text.contains(secret), "unsafe exported diagnostic: {text}");
                }
            }
            assert_eq!(
                fs::read_to_string(bin.join("calls"))?
                    .lines()
                    .collect::<Vec<_>>(),
                notices::MANIFESTS[..=index]
            );
            assert_eq!(
                fs::read(output.join("inventory.json"))?,
                b"retained inventory"
            );
            assert_eq!(
                fs::read(output.join("license.txt"))?,
                b"retained authentic bytes"
            );
            assert_eq!(fs::read_dir(&output)?.count(), 2);
        }
    }
    Ok(())
}

#[test]
fn workflow_prepares_full_graphs_and_only_uploads_sanitized_notice_diagnostics() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let workflow: Value =
        serde_yaml_ng::from_slice(&fs::read(root.join(".github/workflows/checks.yml"))?)?;
    let steps = workflow["jobs"]["native"]["steps"]
        .as_array()
        .ok_or("native steps absent")?;
    let preparation = steps
        .iter()
        .position(|step| step["name"] == "Prepare complete locked notice graphs")
        .ok_or("full-graph preparation absent")?;
    let verification = steps
        .iter()
        .position(|step| step["id"] == "notices")
        .ok_or("offline notices step absent")?;
    assert!(preparation < verification);
    let commands = steps[preparation]["run"]
        .as_str()
        .ok_or("preparation commands absent")?
        .lines()
        .collect::<Vec<_>>();
    assert_eq!(
        commands,
        notices::MANIFESTS
            .map(|manifest| format!("cargo fetch --locked --manifest-path {manifest}"))
    );
    assert_eq!(
        steps[verification]["run"],
        "tools/among-tools/target/release/among-tools third-party-notices . THIRD_PARTY check"
    );
    assert_eq!(
        steps[verification]["env"]["AMONG_CHECK_ROOT"],
        "${{ runner.temp }}/among-checks/notices"
    );
    assert!(steps[verification].get("continue-on-error").is_none());
    let upload = steps
        .iter()
        .find(|step| step["name"] == "Retain sanitized notice failure diagnostics")
        .ok_or("notice failure upload absent")?;
    assert_eq!(
        upload["if"],
        "${{ failure() && steps.notices.outcome == 'failure' }}"
    );
    assert_eq!(
        upload["uses"],
        "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02"
    );
    assert_eq!(
        upload["with"]["path"],
        "${{ runner.temp }}/among-checks/notices/among-notices-*/metadata-failure.json"
    );
    assert_eq!(upload["with"]["if-no-files-found"], "error");
    assert!(upload.get("continue-on-error").is_none());
    assert_eq!(workflow["permissions"], json!({"contents":"read"}));
    Ok(())
}
