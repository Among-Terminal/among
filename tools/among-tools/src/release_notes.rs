//! Renders the description of a tag's GitHub Release from the files the
//! assembly staged. The download table is built from the same targets and
//! naming functions as the staging, and every staged file must end up linked
//! exactly once, so the description cannot point at a file the release lacks
//! or leave one out.

use crate::{deb, release_assets, storage, Result};
use std::{collections::BTreeSet, fs, io::Write, path::Path};

fn os_name(target: &str) -> Result<&'static str> {
    Ok(match target {
        "debian13" => "Debian 13",
        "ubuntu24.04" => "Ubuntu 24.04",
        "fedora43" => "Fedora 43",
        "fedora44" => "Fedora 44",
        _ => return Err(format!("no display name for {target}").into()),
    })
}

/// Names listed in the staged `SHA256SUMS`; they must be exactly the attached
/// set, which the staging already verified byte by byte.
fn staged_names(assets: &Path) -> Result<BTreeSet<String>> {
    let text = fs::read_to_string(storage::safe_path(assets, Path::new("SHA256SUMS"))?)?;
    let mut names = BTreeSet::new();
    for line in text.lines() {
        let (_, name) = line
            .split_once("  ")
            .ok_or("malformed staged SHA256SUMS line")?;
        if !names.insert(name.to_string()) {
            return Err(format!("{name} is listed twice in SHA256SUMS").into());
        }
    }
    if names != release_assets::attached_names() {
        return Err("staged SHA256SUMS does not list exactly the attached files".into());
    }
    Ok(names)
}

/// A stable tag must have hand-written highlights; a release candidate may.
fn highlights(directory: &Path, tag: &str) -> Result<Option<String>> {
    let path = storage::safe_path(directory, Path::new(&format!("{tag}.md")))?;
    let stable = !tag.contains("-rc.");
    match fs::read_to_string(&path) {
        Ok(text) if !text.trim().is_empty() => Ok(Some(text.trim().to_string())),
        Ok(_) if stable => Err(format!("{} is empty", path.display()).into()),
        Err(error) if stable => Err(format!(
            "stable tag {tag} needs highlights in {}: {error}",
            path.display()
        )
        .into()),
        _ => Ok(None),
    }
}

pub fn render(
    assets: &Path,
    repository: &str,
    tag: &str,
    commit: &str,
    run_url: &str,
    highlights_directory: &Path,
) -> Result<String> {
    deb::release_version(tag)?;
    let names = staged_names(assets)?;
    let site = format!("https://github.com/{repository}");
    let link = |name: &str| format!("[{name}]({site}/releases/download/{tag}/{name})");
    let row = |system: &str, format: &str, name: &dyn Fn(&str) -> String| {
        let [amd64, arm64] =
            release_assets::ARCHITECTURES.map(|architecture| link(&name(architecture)));
        format!("| {system} | {format} | {amd64} | {arm64} |\n")
    };

    let mut body = String::new();
    if let Some(text) = highlights(highlights_directory, tag)? {
        body.push_str(&text);
        body.push_str("\n\n");
    }
    body.push_str("## Download\n\n");
    body.push_str(
        "| System | Format | x86-64 (`amd64`) | ARM 64-bit (`arm64`) |\n|---|---|---|---|\n",
    );
    for target in release_assets::DEB_TARGETS.iter().rev() {
        body.push_str(&row(os_name(target)?, "DEB", &|architecture| {
            release_assets::package_asset(target, architecture, "deb")
        }));
    }
    for target in release_assets::RPM_TARGETS.iter().rev() {
        body.push_str(&row(os_name(target)?, "RPM", &|architecture| {
            release_assets::package_asset(target, architecture, "rpm")
        }));
    }
    body.push_str(&row(
        "Other distributions with glibc 2.39 or newer",
        "AppImage",
        &release_assets::appimage_asset,
    ));
    for target in release_assets::DEB_TARGETS.iter().rev() {
        body.push_str(&row(os_name(target)?, "Binary archive", &|architecture| {
            release_assets::package_asset(target, architecture, "tar.gz")
        }));
    }
    let guide = format!("{site}/blob/{tag}/docs/install.md");
    body.push_str(&format!(
        "\n`uname -m` prints `x86_64` for amd64 and `aarch64` for arm64.

## Install

Debian and Ubuntu (APT installs the libraries Among needs):

```sh
sudo apt install ./among-terminal-ubuntu24.04-amd64.deb
```

Fedora:

```sh
sudo dnf install ./among-terminal-fedora44-amd64.rpm
```

AppImage (needs FUSE from the `fuse3` package; installs nothing):

```sh
chmod +x among-terminal-amd64.AppImage
./among-terminal-amd64.AppImage
```

Use the file name you downloaded. Start **Among Terminal** from the applications
menu, or run `among`. Plugins are disabled by default. The binary archive does
not bundle libraries; see the [installation guide]({guide}#binary-archive) for it,
for AppImage menu integration, updates and removal.

## Verify

Download `SHA256SUMS` into the same directory and run:

```sh
sha256sum --check --ignore-missing SHA256SUMS
```

## What was checked

Before this release was created, every DEB and RPM was installed in a clean
container of its distribution on both architectures, without compilers, and
started as an unprivileged user under X11 (Xvfb with Openbox): desktop launch,
shell input, focus, status plugins and session restoration, then an upgrade
from an older package, removal and reinstallation. The AppImage was started on
clean Debian 13, Ubuntu 24.04 and Fedora 43 hosts. GNOME and Wayland sessions
are not part of these checks.

## Source

Built from commit [`{commit}`]({site}/commit/{commit}) by [this workflow run]({run_url}).
Source RPMs: {srpms}. Sources of the libraries bundled in the AppImage: {sources}.
Licences: [LICENSE]({site}/blob/{tag}/LICENSE), [NOTICE]({site}/blob/{tag}/NOTICE),
[third-party notices]({site}/tree/{tag}/THIRD_PARTY).
",
        srpms = release_assets::RPM_TARGETS
            .map(|target| link(&release_assets::srpm_asset(target)))
            .join(", "),
        sources = release_assets::ARCHITECTURES
            .map(|architecture| link(&release_assets::source_archive_name(architecture)))
            .join(", "),
    ));

    for name in &names {
        let url = format!("({site}/releases/download/{tag}/{name})");
        let count = body.matches(&url).count();
        if count != 1 {
            return Err(format!("{name} is linked {count} times, expected once").into());
        }
    }
    let prefix = format!("{site}/releases/download/{tag}/");
    let linked = body.matches(&prefix).count();
    if linked != names.len() {
        return Err(format!("{linked} download links for {} files", names.len()).into());
    }
    Ok(body)
}

/// Writes the description to `output`, which must not exist yet.
pub fn write(
    assets: &Path,
    repository: &str,
    tag: &str,
    commit: &str,
    run_url: &str,
    highlights_directory: &Path,
    output: &Path,
) -> Result<()> {
    let body = render(
        assets,
        repository,
        tag,
        commit,
        run_url,
        highlights_directory,
    )?;
    let mut file = fs::File::options()
        .write(true)
        .create_new(true)
        .open(output)?;
    file.write_all(body.as_bytes())?;
    file.sync_all()?;
    Ok(())
}
