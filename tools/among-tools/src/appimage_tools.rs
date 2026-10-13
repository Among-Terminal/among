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

fn tools(architecture: &str) -> Result<Vec<Tool<'static>>> {
    let (suffix, linuxdeploy, appimagetool, runtime) = match architecture {
        "amd64" => (
            "x86_64",
            "8aea8da0f7f7039d2a2cecb14657d752a222a5e1d3825caeef186c82f751cdd1",
            "95cbe7cce9717fce90c484e34052ee7c7f1d7635b33c12525b4776826a7d29b6",
            "156f4bdbde9c52d01814600013e0a273f0118dc2de98975f3c8c63427ec79074",
        ),
        "arm64" => (
            "aarch64",
            "5c1fddf96066891e829831cac0d84424690f3b22846c7f8f1bb9990a5c6c73f4",
            "a595ea34cd6136c7f595e9dcbb16f3e9725d7610efb9e43b38c3c6e86cafc270",
            "b4ff0030242d0c3bb12ce40541828303cf167493f4793456f0436edd6255c39d",
        ),
        _ => return Err("unsupported AppImage helper architecture".into()),
    };
    Ok(vec![
        Tool { filename: "linuxdeploy.AppImage", url: format!("https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-{suffix}.AppImage"), sha256: linuxdeploy, elf: true },
        Tool { filename: "appimagetool.AppImage", url: format!("https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-{suffix}.AppImage"), sha256: appimagetool, elf: true },
        Tool { filename: "runtime", url: format!("https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-{suffix}"), sha256: runtime, elf: true },
        Tool { filename: "linuxdeploy-plugin-gtk.sh", url: "https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/7a3fbc31a9e5075073ff8790f26effbac5f84453/linuxdeploy-plugin-gtk.sh".into(), sha256: "b0f4cbc684a0103a9651f0955b635eaea0096b3a66c0f5a2c2aa337960375171", elf: false },
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
            "AppImage helper {} changed upstream; refusing unreviewed bytes",
            tool.filename
        )
        .into());
    }
    if tool.elf {
        deb::elf_architecture(path, architecture)?;
    }
    Ok(digest)
}

/// Continuous URLs are mutable: the reviewed byte hashes, not their labels,
/// grant execution authority. No helper is executed by this download command.
/// Runtime source/licence delivery is a separate gate, not implied by a pin.
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
                assert!(tool.url.starts_with("https://"));
            }
        }
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
