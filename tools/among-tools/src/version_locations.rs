//! The one list of places that spell the application version.
//!
//! `release_plan` rewrites exactly these places when it prepares a release, and
//! `check` reads the same list and requires every place to agree. A place that
//! is added to one list but not the other cannot exist: both use `LOCATIONS`.
//!
//! Not release locations, on purpose:
//! * `tools/among-tools` has its own version. The tooling package is never
//!   shipped and its version does not describe the application.
//! * `packaging/debian/changelog` and the RPM `%changelog` are history. The
//!   packages take their version from the release tag (the Debian changelog of
//!   a build is rewritten from the tag, the RPM `Version:` is a location
//!   below), so these files may lag; they must never be ahead of the
//!   application.
//! * `packaging/ci/deb-upgrade/previous-release.json` pins the last *published*
//!   release for upgrade tests and changes after publication, not in a release.

use crate::{storage, Result};
use std::fs;
use std::path::Path;

pub const SPEC: &str = "packaging/rpm/among.spec";
pub const METAINFO: &str = "data/com.github.among-terminal.metainfo.xml";
const DEBIAN_CHANGELOG: &str = "packaging/debian/changelog";

pub struct Location {
    pub path: &'static str,
    read: fn(&str) -> Result<String>,
    write: fn(&str, &str) -> Result<String>,
}

pub const LOCATIONS: [Location; 6] = [
    Location {
        path: "Cargo.toml",
        read: |text| cargo_manifest_version(text, "among"),
        write: |text, version| set_cargo_manifest_version(text, version),
    },
    Location {
        path: "Cargo.lock",
        read: |text| cargo_lock_version(text, "among"),
        write: |text, version| set_cargo_lock_version(text, "among", version),
    },
    Location {
        path: "examples/plugins/Cargo.toml",
        read: |text| cargo_manifest_version(text, "among-example-plugins"),
        write: |text, version| set_cargo_manifest_version(text, version),
    },
    Location {
        path: "examples/plugins/Cargo.lock",
        read: |text| cargo_lock_version(text, "among-example-plugins"),
        write: |text, version| set_cargo_lock_version(text, "among-example-plugins", version),
    },
    Location {
        path: SPEC,
        read: spec_version,
        write: set_spec_version,
    },
    Location {
        path: METAINFO,
        read: metainfo_version,
        // A release entry is added by `release_plan`, which owns its text; the
        // version of the newest entry is only read here.
        write: |text, _| Ok(text.to_string()),
    },
];

fn require(value: bool, message: impl Into<String>) -> Result<()> {
    if value {
        Ok(())
    } else {
        Err(message.into().into())
    }
}

fn cargo_manifest_version(text: &str, name: &str) -> Result<String> {
    let document: toml::Value = toml::from_str(text)?;
    let package = document.get("package").ok_or("Cargo.toml has no package")?;
    require(
        package.get("name").and_then(toml::Value::as_str) == Some(name),
        format!("Cargo.toml is not the {name} package"),
    )?;
    Ok(package
        .get("version")
        .and_then(toml::Value::as_str)
        .ok_or("Cargo.toml package version is absent")?
        .to_string())
}

fn cargo_lock_version(text: &str, name: &str) -> Result<String> {
    let document: toml::Value = toml::from_str(text)?;
    let mut found = document
        .get("package")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("name").and_then(toml::Value::as_str) == Some(name));
    let entry = found
        .next()
        .ok_or_else(|| format!("Cargo.lock has no {name} package"))?;
    require(
        found.next().is_none(),
        format!("Cargo.lock lists {name} twice"),
    )?;
    Ok(entry
        .get("version")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| format!("Cargo.lock {name} has no version"))?
        .to_string())
}

/// Replaces the quoted value of a `version = "..."` line, keeping the line's
/// layout. `None` when the line is not a version assignment.
fn rewrite_version_line(line: &str, version: &str) -> Option<String> {
    let rest = line.strip_prefix("version")?;
    let after_equals = rest.trim_start().strip_prefix('=')?;
    let quote = after_equals.find('"')?;
    let end = after_equals[quote + 1..].find('"')? + quote + 1;
    let prefix_length = line.len() - after_equals.len() + quote + 1;
    let suffix = &after_equals[end..];
    Some(format!("{}{version}{suffix}", &line[..prefix_length]))
}

fn set_cargo_manifest_version(text: &str, version: &str) -> Result<String> {
    let mut in_package = false;
    let mut replaced = 0;
    let mut output = String::new();
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let ending = &line[body.len()..];
        if body.starts_with('[') {
            in_package = body.trim() == "[package]";
        }
        match in_package
            .then(|| rewrite_version_line(body, version))
            .flatten()
        {
            Some(changed) if replaced == 0 => {
                replaced += 1;
                output.push_str(&changed);
                output.push_str(ending);
            }
            _ => output.push_str(line),
        }
    }
    require(replaced == 1, "Cargo.toml package version line not found")?;
    Ok(output)
}

fn set_cargo_lock_version(text: &str, name: &str, version: &str) -> Result<String> {
    let wanted = format!("name = \"{name}\"");
    let mut matching = false;
    let mut replaced = 0;
    let mut output = String::new();
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let ending = &line[body.len()..];
        if body == "[[package]]" {
            matching = false;
        } else if body == wanted {
            matching = true;
        }
        match matching
            .then(|| rewrite_version_line(body, version))
            .flatten()
        {
            Some(changed) if replaced == 0 => {
                replaced += 1;
                matching = false;
                output.push_str(&changed);
                output.push_str(ending);
            }
            _ => output.push_str(line),
        }
    }
    require(
        replaced == 1,
        format!("Cargo.lock {name} version line not found"),
    )?;
    Ok(output)
}

fn spec_version(text: &str) -> Result<String> {
    let mut versions = text
        .lines()
        .filter_map(|line| line.strip_prefix("Version:"))
        .map(|value| value.trim().to_string());
    let version = versions.next().ok_or("RPM spec has no Version header")?;
    require(
        versions.next().is_none(),
        "RPM spec has more than one Version header",
    )?;
    Ok(version)
}

fn set_spec_version(text: &str, version: &str) -> Result<String> {
    let mut replaced = 0;
    let mut output = String::new();
    for line in text.split_inclusive('\n') {
        match line.strip_prefix("Version:") {
            Some(rest) => {
                let spaces = rest.len() - rest.trim_start().len();
                let ending = &line[line.trim_end_matches(['\n', '\r']).len()..];
                output.push_str(&format!("Version:{}{version}{ending}", &rest[..spaces]));
                replaced += 1;
            }
            None => output.push_str(line),
        }
    }
    require(
        replaced == 1,
        "RPM spec must have exactly one Version header",
    )?;
    Ok(output)
}

fn metainfo_version(text: &str) -> Result<String> {
    let document = roxmltree::Document::parse(text)?;
    document
        .root_element()
        .children()
        .find(|node| node.has_tag_name("releases"))
        .and_then(|node| node.children().find(|node| node.has_tag_name("release")))
        .and_then(|node| node.attribute("version"))
        .map(str::to_string)
        .ok_or_else(|| "AppStream metainfo has no current release version".into())
}

/// `MAJOR.MINOR.PATCH` without leading zeros.
pub fn triple(version: &str) -> Result<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let mut next = || -> Result<u64> {
        let part = parts
            .next()
            .ok_or_else(|| format!("{version} is not MAJOR.MINOR.PATCH"))?;
        require(
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part == "0" || !part.starts_with('0')),
            format!("{version} is not MAJOR.MINOR.PATCH"),
        )?;
        Ok(part.parse()?)
    };
    let value = (next()?, next()?, next()?);
    require(
        parts.next().is_none(),
        format!("{version} is not MAJOR.MINOR.PATCH"),
    )?;
    Ok(value)
}

/// The application version a changelog entry belongs to: the part before a
/// distribution revision (`-1`) or a candidate suffix (`~rc.1`, `-rc.1`).
fn entry_base(version: &str) -> &str {
    version.split(['~', '-']).next().unwrap_or(version)
}

fn newest_rpm_entry(spec: &str) -> Result<String> {
    let after = spec
        .split_once("%changelog")
        .ok_or("RPM spec has no %changelog")?
        .1;
    let line = after
        .lines()
        .find(|line| line.starts_with("* "))
        .ok_or("RPM %changelog has no entry")?;
    let version = line
        .rsplit_once(" - ")
        .ok_or("RPM %changelog entry has no version")?
        .1
        .trim();
    // `0.1.2~rc.1-1`: the last dash starts the package release number.
    let version = version.rsplit_once('-').map_or(version, |(base, _)| base);
    Ok(entry_base(version).to_string())
}

fn newest_debian_entry(changelog: &str) -> Result<String> {
    let line = changelog
        .lines()
        .find(|line| !line.trim().is_empty())
        .ok_or("Debian changelog is empty")?;
    let version = line
        .split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .ok_or("Debian changelog entry has no version")?
        .0;
    let version = version.rsplit_once('-').map_or(version, |(base, _)| base);
    Ok(entry_base(version).to_string())
}

pub fn read_file(root: &Path, relative: &str) -> Result<String> {
    Ok(fs::read_to_string(storage::safe_path(
        root,
        Path::new(relative),
    )?)?)
}

/// Every version location read from `root`, as `(path, version)`.
pub fn read_all(root: &Path) -> Result<Vec<(&'static str, String)>> {
    let mut found = Vec::new();
    for location in &LOCATIONS {
        let text = read_file(root, location.path)
            .map_err(|error| format!("version location {}: {error}", location.path))?;
        let version = (location.read)(&text)
            .map_err(|error| format!("version location {}: {error}", location.path))?;
        found.push((location.path, version));
    }
    Ok(found)
}

/// Rewritten text of every location that holds the version, as
/// `(path, new text)`. Nothing is written.
pub fn rewrite_all(root: &Path, version: &str) -> Result<Vec<(&'static str, String)>> {
    triple(version)?;
    let mut changed = Vec::new();
    for location in &LOCATIONS {
        let text = read_file(root, location.path)?;
        let rewritten = (location.write)(&text, version)
            .map_err(|error| format!("version location {}: {error}", location.path))?;
        changed.push((location.path, rewritten));
    }
    Ok(changed)
}

fn check_changelogs(root: &Path, version: &str) -> Result<()> {
    let current = triple(version)?;
    for (name, newest) in [
        (SPEC, newest_rpm_entry(&read_file(root, SPEC)?)?),
        (
            DEBIAN_CHANGELOG,
            newest_debian_entry(&read_file(root, DEBIAN_CHANGELOG)?)?,
        ),
    ] {
        require(
            triple(&newest)? <= current,
            format!("{name}: newest entry {newest} is ahead of application version {version}"),
        )?;
    }
    Ok(())
}

/// Reads all locations, requires them to agree and returns the version.
pub fn check(root: &Path) -> Result<String> {
    let found = read_all(root)?;
    let version = found
        .first()
        .map(|(_, version)| version.clone())
        .ok_or("no version locations")?;
    triple(&version)?;
    for (path, other) in &found {
        require(
            *other == version,
            format!("version location {path} says {other}, Cargo.toml says {version}"),
        )?;
    }
    check_changelogs(root, &version)?;
    Ok(version)
}
