use crate::{deb_workflow, process::OwnedProcess, storage, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::time::Duration;

fn require(value: bool, message: &str) -> Result<()> {
    if value {
        Ok(())
    } else {
        Err(message.into())
    }
}
pub fn validate(
    metadata: &Value,
    manifests: &[toml::Value; 2],
    xml: &str,
    desktop: &str,
) -> Result<Vec<String>> {
    require(
        metadata["name"] == "among"
            && metadata["base"] == "core24"
            && metadata["confinement"] == "classic",
        "Snap name/base/confinement differs",
    )?;
    require(
        metadata["license"] == "Apache-2.0" && metadata["grade"] == "devel",
        "candidate recipe license/grade differs; stable overlay requires separate readiness review",
    )?;
    require(
        metadata["platforms"]
            == json!({"amd64":{"build-on":["amd64"],"build-for":["amd64"]},"arm64":{"build-on":["arm64"],"build-for":["arm64"]}}),
        "Snap must build both native architectures",
    )?;
    require(
        metadata["apps"]["among"]["command"] == "bin/among" && metadata["adopt-info"] == "among",
        "Snap command/adopt-info differs",
    )?;
    let part = &metadata["parts"]["among"];
    require(
        part["rust-channel"] == "1.98.1" && part["rust-path"] == json!([".", "examples/plugins"]),
        "Snap Rust version/path differs",
    )?;
    require(
        !part.get("rust-cargo-parameters").is_some_and(|value| {
            value
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item == "--locked"))
        }),
        "Snap Rust plugin already supplies --locked",
    )?;
    require(
        part["rust-inherit-ldflags"] == true,
        "Snap Rust build must inherit linker flags",
    )?;
    let flags = part["build-environment"][0]["LDFLAGS"]
        .as_str()
        .ok_or("Snap linker flags absent")?;
    require(flags.contains("$CRAFT_ARCH_TRIPLET_BUILD_FOR")&&!flags.contains("--dynamic-linker"),"classic linker flags must select target architecture and retain automatic interpreter patching")?;
    let stage = part["override-stage"]
        .as_str()
        .ok_or("Snap override-stage absent")?;
    require(
        !stage.contains("x86_64-linux-gnu"),
        "SVG loader setup must work on both architectures",
    )?;
    require(
        part["stage-packages"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item == "librsvg2-common")),
        "runtime SVG loader package absent",
    )?;
    require(
        part["build-attributes"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item == "enable-patchelf"))
            && part.get("override-prime").is_none(),
        "automatic classic ELF patching must not be bypassed",
    )?;
    require(
        stage.contains(
            "glib-compile-schemas --strict \"$CRAFT_PART_INSTALL/usr/share/glib-2.0/schemas\"",
        ),
        "strict installed schemas compilation absent",
    )?;
    let mut versions = Vec::new();
    for manifest in manifests {
        let package = manifest
            .get("package")
            .ok_or("product manifest package absent")?;
        require(
            package.get("license").and_then(toml::Value::as_str) == Some("Apache-2.0"),
            "product/Snap licenses differ",
        )?;
        versions.push(
            package
                .get("version")
                .and_then(toml::Value::as_str)
                .ok_or("product version absent")?,
        );
    }
    require(versions[0] == versions[1], "product/plugin versions differ")?;
    let document = roxmltree::Document::parse(xml)?;
    let component = document.root_element();
    let text = |name| {
        component
            .children()
            .find(|node| node.has_tag_name(name))
            .and_then(|node| node.text())
    };
    require(
        text("id") == Some("com.github.among-terminal")
            && text("project_license") == Some("Apache-2.0")
            && text("launchable") == Some("com.github.among-terminal.desktop"),
        "AppStream identity/license/launchable differs",
    )?;
    let release = component
        .children()
        .find(|node| node.has_tag_name("releases"))
        .and_then(|node| node.children().find(|node| node.has_tag_name("release")));
    require(
        release.and_then(|node| node.attribute("version")) == Some(versions[0]),
        "AppStream version differs",
    )?;
    let mut section = "";
    let mut entry = BTreeMap::new();
    let mut seen = false;
    for line in desktop
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with(';'))
    {
        if line.starts_with('[') && line.ends_with(']') {
            section = &line[1..line.len() - 1];
            if section == "Desktop Entry" {
                require(!seen, "duplicate desktop group")?;
                seen = true;
            }
            continue;
        }
        if section == "Desktop Entry" {
            let (name, value) = line.split_once('=').ok_or("invalid desktop property")?;
            require(
                entry.insert(name.trim(), value.trim()).is_none(),
                "duplicate desktop property",
            )?;
        }
    }
    require(
        entry.get("Exec") == Some(&"among")
            && entry.get("Icon") == Some(&"com.github.among-terminal")
            && entry.get("Terminal") == Some(&"false")
            && entry.get("Name").copied() == text("name"),
        "desktop launch/icon/name/terminal metadata differs",
    )?;
    Ok(vec![
        part["override-build"]
            .as_str()
            .ok_or("Snap override-build absent")?
            .into(),
        stage.into(),
    ])
}

pub fn run(root: &Path, scratch: &Path) -> Result<()> {
    let read = |name: &str| -> Result<String> {
        Ok(fs::read_to_string(storage::safe_path(
            root,
            Path::new(name),
        )?)?)
    };
    let metadata = deb_workflow::read(&read("snap/snapcraft.yaml")?)?;
    let manifests = [
        toml::from_str(&read("Cargo.toml")?)?,
        toml::from_str(&read("examples/plugins/Cargo.toml")?)?,
    ];
    let scripts = validate(
        &metadata,
        &manifests,
        &read("data/com.github.among-terminal.metainfo.xml")?,
        &read("data/com.github.among-terminal.desktop")?,
    )?;
    let base = storage::private_directory(scratch, "among-release-metadata")?;
    let env = BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]);
    for (index, script) in scripts.iter().enumerate() {
        let log = File::create(base.join(format!("shell-{index}.log")))?;
        let mut child =
            OwnedProcess::spawn_interactive(Path::new("/bin/bash"), &["-n"], &env, &base, &log)?;
        let mut stdin = child.take_stdin()?;
        stdin.write_all(script.as_bytes())?;
        drop(stdin);
        require(
            child.wait(Duration::from_secs(10))?.success(),
            "Snap override shell syntax failed",
        )?;
    }
    println!("PASS package/desktop/version/license consistency and override shell syntax; Snap runtime is NOT tested");
    Ok(())
}
