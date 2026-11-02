use crate::{deb, process::OwnedProcess, storage, Result};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs::{self, File},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

struct Tool<'a> {
    filename: &'static str,
    url: String,
    sha256: &'a str,
    elf: bool,
}

/// Upstream addresses change in place: `continuous` assets are replaced by every
/// upstream build and the assets of a plain tag can be overwritten too, so a
/// pinned hash stops matching with no change on our side and every AppImage
/// build is refused at once. CI must not depend on them. Each helper is
/// therefore served from an immutable release of our own repository, whose
/// assets and tag cannot change. A new helper version means a new release there
/// and a separate pull request here, as `docs/build.md` describes.
const HELPER_RELEASE: &str =
    "https://github.com/Among-Terminal/appimage-tools/releases/download/2026.11.02";

fn tools(architecture: &str) -> Result<Vec<Tool<'static>>> {
    let (suffix, linuxdeploy, appimagetool, runtime) = match architecture {
        "amd64" => (
            "x86_64",
            "c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d",
            "ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0",
            "617aa77314d57c8b8e4fae9c3f0b35af7b30d5529cb69e98f64f92510ed98a46",
        ),
        "arm64" => (
            "aarch64",
            "620095110d693282b8ebeb244a95b5e911cf8f65f76c88b4b47d16ae6346fcff",
            "f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158",
            "9a72a744d221576c9347ec074243acc5c3c7568896b12714e0e82e9f0bf47900",
        ),
        _ => return Err("unsupported AppImage helper architecture".into()),
    };
    Ok(vec![
        Tool {
            filename: "linuxdeploy.AppImage",
            url: format!("{HELPER_RELEASE}/linuxdeploy-{suffix}.AppImage"),
            sha256: linuxdeploy,
            elf: true,
        },
        Tool {
            filename: "appimagetool.AppImage",
            url: format!("{HELPER_RELEASE}/appimagetool-{suffix}.AppImage"),
            sha256: appimagetool,
            elf: true,
        },
        Tool {
            filename: "runtime",
            url: format!("{HELPER_RELEASE}/runtime-{suffix}"),
            sha256: runtime,
            elf: true,
        },
        Tool {
            filename: "linuxdeploy-plugin-gtk.sh",
            url: format!("{HELPER_RELEASE}/linuxdeploy-plugin-gtk.sh"),
            sha256: "b0f4cbc684a0103a9651f0955b635eaea0096b3a66c0f5a2c2aa337960375171",
            elf: false,
        },
    ])
}

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn validate(path: &Path, tool: &Tool<'_>, architecture: &str) -> Result<String> {
    if !path.symlink_metadata()?.is_file() {
        return Err("AppImage helper is not a regular file".into());
    }
    let digest = storage::sha256(File::open(path)?)?;
    if digest != tool.sha256 {
        return Err(format!(
            "AppImage helper {} does not match its pin; refusing unreviewed bytes",
            tool.filename
        )
        .into());
    }
    if tool.elf {
        deb::elf_architecture(path, architecture)?;
    }
    Ok(digest)
}

/// The reviewed byte hashes, not the release address, grant execution
/// authority. No helper is executed by this download command. Source and
/// licence delivery for the helpers themselves is the helper release's job;
/// what a built AppImage must carry for its embedded runtime is a separate
/// release gate, not implied by a pin.
pub fn fetch(architecture: &str, destination: &Path) -> Result<()> {
    let tools = tools(architecture)?;
    if destination.symlink_metadata().is_ok() {
        return Err("AppImage helper destination must be new".into());
    }
    let name = destination
        .file_name()
        .ok_or("helper destination has no name")?;
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()?;
    let scratch = Scratch(storage::private_directory(
        &parent,
        "among-appimage-helpers",
    )?);
    let payload = scratch.0.join("payload");
    fs::create_dir(&payload)?;
    let environment = BTreeMap::from([
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("LC_ALL".into(), "C".into()),
    ]);
    let mut records = Vec::new();
    for tool in tools {
        let output = payload.join(tool.filename);
        let mut download = OwnedProcess::spawn(
            Path::new("/usr/bin/curl"),
            &[
                "--disable",
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--retry",
                "3",
                "--retry-max-time",
                "540",
                "--max-filesize",
                "67108864",
                "--max-redirs",
                "5",
                "--max-time",
                "120",
                "--output",
                output.to_str().ok_or("helper path is not UTF-8")?,
                &tool.url,
            ],
            &environment,
            &scratch.0,
            &File::create(scratch.0.join("download.log"))?,
        )?;
        if !download.wait(Duration::from_secs(650))?.success() {
            return Err(format!("AppImage helper download failed: {}", tool.filename).into());
        }
        let digest = validate(&output, &tool, architecture)?;
        fs::set_permissions(&output, fs::Permissions::from_mode(0o755))?;
        File::open(&output)?.sync_all()?;
        records.push(json!({"filename":tool.filename,"url":tool.url,"sha256":digest,"bytes":output.metadata()?.len()}));
    }
    storage::atomic_write(
        &payload,
        Path::new("tool-inputs.json"),
        &serde_json::to_vec_pretty(
            &json!({"schema":1,"architecture":architecture,"tools":records,"scope":"pinned helper inputs only; no runtime/source/license acceptance"}),
        )?,
        0o644,
    )?;
    fs::set_permissions(&payload, fs::Permissions::from_mode(0o755))?;
    storage::publish_directory(&payload, &parent.join(name))?;
    Ok(())
}

/// Copies every pinned helper from a fetched directory into a private
/// directory and checks the copies, not the originals. Execution authority is
/// granted only to the bytes that were hashed, so a fetched directory that is
/// modified after the check cannot swap in a different program.
pub fn verified_copy(
    architecture: &str,
    fetched: &Path,
    destination: &Path,
) -> Result<Vec<serde_json::Value>> {
    let tools = tools(architecture)?;
    let mut records = Vec::new();
    for tool in tools {
        let source = storage::safe_path(fetched, Path::new(tool.filename))?;
        if !source.symlink_metadata()?.is_file() {
            return Err(format!("AppImage helper {} is not a regular file", tool.filename).into());
        }
        let copy = storage::safe_path(destination, Path::new(tool.filename))?;
        fs::copy(&source, &copy)?;
        let digest = validate(&copy, &tool, architecture)?;
        fs::set_permissions(&copy, fs::Permissions::from_mode(0o755))?;
        records.push(json!({"filename":tool.filename,"url":tool.url,"sha256":digest}));
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_native_pins_refuse_changed_bytes_and_unsupported_targets() -> Result<()> {
        for architecture in ["amd64", "arm64"] {
            let inputs = tools(architecture)?;
            assert_eq!(inputs.len(), 4);
            for tool in inputs {
                assert_eq!(tool.sha256.len(), 64);
                assert!(tool
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')));
                // Any other host would bring back a dependency on addresses
                // whose bytes can change under a pinned hash.
                assert!(tool.url.starts_with(&format!("{HELPER_RELEASE}/")));
                assert_eq!(tool.url.matches("https://").count(), 1);
            }
        }
        // The asset names are the release's public layout: a mismatch here is a
        // failed download in CI rather than a pin mismatch.
        let assets = |architecture| -> Result<Vec<String>> {
            Ok(tools(architecture)?
                .into_iter()
                .map(|tool| tool.url[HELPER_RELEASE.len() + 1..].to_string())
                .collect())
        };
        assert_eq!(
            assets("amd64")?,
            [
                "linuxdeploy-x86_64.AppImage",
                "appimagetool-x86_64.AppImage",
                "runtime-x86_64",
                "linuxdeploy-plugin-gtk.sh",
            ]
        );
        assert_eq!(
            assets("arm64")?,
            [
                "linuxdeploy-aarch64.AppImage",
                "appimagetool-aarch64.AppImage",
                "runtime-aarch64",
                "linuxdeploy-plugin-gtk.sh",
            ]
        );
        assert!(tools("universal").is_err());
        let root = Scratch(storage::private_directory(
            &std::env::temp_dir(),
            "among-appimage-tool-pin",
        )?);
        let path = root.0.join("changed");
        fs::write(&path, b"unreviewed replacement")?;
        assert!(validate(&path, &tools("amd64")?[0], "amd64").is_err());
        let mut elf = [0u8; 20];
        elf[..6].copy_from_slice(b"\x7fELF\x02\x01");
        elf[18] = 62;
        fs::write(&path, elf)?;
        let digest = storage::sha256(elf.as_slice())?;
        let fixture = Tool {
            filename: "fixture",
            url: String::new(),
            sha256: &digest,
            elf: true,
        };
        assert_eq!(validate(&path, &fixture, "amd64")?, digest);
        assert!(validate(&path, &fixture, "arm64").is_err());
        let destination = root.0.join("refused");
        assert!(fetch("universal", &destination).is_err());
        assert!(!destination.exists());
        let first = root.0.join("first");
        let second = root.0.join("second");
        fs::create_dir(&first)?;
        fs::create_dir(&second)?;
        assert!(storage::publish_directory(&first, &second).is_err());
        assert!(first.is_dir() && second.is_dir());
        let new = root.0.join("published");
        storage::publish_directory(&first, &new)?;
        assert!(!first.exists() && new.is_dir());
        Ok(())
    }
}
