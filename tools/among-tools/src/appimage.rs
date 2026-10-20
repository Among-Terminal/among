use crate::{deb, storage, Result};
use serde_json::json;
use std::{
    fs::{self, File},
    os::unix::fs::PermissionsExt,
    path::Path,
};

const ID: &str = "com.github.among-terminal";

pub fn verify_raster_icons(appdir: &Path) -> Result<()> {
    for (size, expected) in [
        (
            128,
            include_bytes!("../../../data/com.github.among-terminal-128.png").as_slice(),
        ),
        (
            256,
            include_bytes!("../../../data/com.github.among-terminal-256.png").as_slice(),
        ),
    ] {
        let relative = format!("usr/share/icons/hicolor/{size}x{size}/apps/{ID}.png");
        if fs::read(storage::safe_path(appdir, Path::new(&relative))?)? != expected {
            return Err(format!("AppImage raster artwork differs at {relative}").into());
        }
    }
    Ok(())
}

fn launcher_desktop(contents: &str) -> Result<String> {
    if contents
        .lines()
        .filter(|line| *line == "Exec=/usr/bin/among")
        .count()
        != 1
    {
        return Err("staged desktop executable is ambiguous".into());
    }
    Ok(contents
        .split_inclusive('\n')
        .map(|line| {
            if line.trim_end_matches(['\r', '\n']) == "Exec=/usr/bin/among" {
                format!("Exec=AppRun{}", &line["Exec=/usr/bin/among".len()..])
            } else {
                line.to_string()
            }
        })
        .collect())
}

/// Stage project-owned payload only. Native library/source auditing must run
/// after deployment, against the final image, not this pre-bundling directory.
pub fn stage(
    root: &Path,
    binaries: &Path,
    launcher: &Path,
    architecture: &str,
    destination: &Path,
) -> Result<()> {
    crate::release_metadata::artwork(root)?;
    for name in ["among", "clock", "system-status", "pane-pwd"] {
        deb::elf_architecture(
            &storage::safe_path(binaries, Path::new(name))?,
            architecture,
        )?;
    }
    if !launcher.symlink_metadata()?.is_file()
        || launcher.metadata()?.permissions().mode() & 0o111 == 0
    {
        return Err("AppImage launcher must be an executable regular file".into());
    }
    deb::elf_architecture(launcher, architecture)?;
    let launcher = fs::read(launcher)?;
    deb::stage(root, binaries, destination)?;
    storage::atomic_write(destination, Path::new("AppRun"), &launcher, 0o755)?;
    let desktop = format!("usr/share/applications/{ID}.desktop");
    let contents = fs::read_to_string(destination.join(&desktop))?;
    // AppImage desktop integration must select its launcher, never a coincident
    // host /usr/bin/among. The root copy is consumed by appimagetool.
    let contents = launcher_desktop(&contents)?;
    storage::atomic_write(destination, Path::new(&desktop), contents.as_bytes(), 0o644)?;
    storage::atomic_write(
        destination,
        Path::new(&format!("{ID}.desktop")),
        contents.as_bytes(),
        0o644,
    )?;
    let icon =
        fs::read(destination.join(format!("usr/share/icons/hicolor/scalable/apps/{ID}.svg")))?;
    storage::atomic_write(destination, Path::new(&format!("{ID}.svg")), &icon, 0o644)?;
    // libappimage's thumbnailer prefers these exact-size paths over scalable
    // SVGs, whose bundled renderer can silently produce fully transparent PNGs.
    for size in [128, 256] {
        let icon = fs::read(root.join(format!("data/{ID}-{size}.png")))?;
        let directory = destination.join(format!("usr/share/icons/hicolor/{size}x{size}/apps"));
        fs::create_dir_all(&directory)?;
        storage::atomic_write(&directory, Path::new(&format!("{ID}.png")), &icon, 0o644)?;
    }
    verify_raster_icons(destination)?;
    let document = json!({"schema":1,"architecture":architecture,
        "app_sha256":storage::sha256(File::open(destination.join("usr/bin/among"))?)?,
        "launcher_sha256":storage::sha256(File::open(destination.join("AppRun"))?)?,
        "scope":"project-owned AppDir payload only; native deployment, corresponding-source and runtime checks remain separate",
        "release_acceptance":false});
    storage::atomic_write(
        destination,
        Path::new("owned-payload.json"),
        &serde_json::to_vec_pretty(&document)?,
        0o644,
    )
}

#[cfg(test)]
mod desktop_tests {
    use super::*;
    #[test]
    fn launcher_rewrite_preserves_newlines_and_unrelated_values() -> Result<()> {
        for newline in ["\n", "\r\n"] {
            for terminated in [false, true] {
                let ending = if terminated { newline } else { "" };
                let prefix =
                    format!("[Desktop Entry]{newline}Comment=Exec=/usr/bin/among{newline}");
                assert_eq!(
                    launcher_desktop(&format!("{prefix}Exec=/usr/bin/among{ending}"))?,
                    format!("{prefix}Exec=AppRun{ending}")
                );
            }
        }
        for invalid in [
            "Comment=Exec=/usr/bin/among\n",
            "Exec=/usr/bin/among\nExec=/usr/bin/among\n",
            "Exec=/usr/bin/among --argument\n",
        ] {
            assert!(launcher_desktop(invalid).is_err());
        }
        Ok(())
    }
}
