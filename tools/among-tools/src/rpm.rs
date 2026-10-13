use crate::{package_version, storage, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

/// Fields a Fedora spec must carry before any package is built.
const REQUIRED: [&str; 7] = [
    "Name", "Version", "Release", "License", "Summary", "URL", "Source0",
];

/// Fedora expresses library requirements as pkgconfig capabilities. These three
/// are the ones the application actually links; the Debian control file carries
/// the equivalent minimums and both must stay present.
pub const PKGCONFIG: [&str; 3] = [
    "pkgconfig(gtk4)",
    "pkgconfig(libadwaita-1)",
    "pkgconfig(vte-2.91-gtk4)",
];

/// The Rust binding crate that selects the C API level of each pkgconfig
/// capability. The linked executable carries no usable minimum of its own: GTK 4
/// and VTE export unversioned symbols and libadwaita exports one `LIBADWAITA_1_0`
/// node for every 1.x release. The highest `vX_Y` feature enabled on the
/// binding is therefore the oldest library the code can call into.
const BINDINGS: [(&str, &str); 3] = [
    ("gtk4", "pkgconfig(gtk4)"),
    ("libadwaita", "pkgconfig(libadwaita-1)"),
    ("vte4", "pkgconfig(vte-2.91-gtk4)"),
];

/// Single-valued preamble fields. A second occurrence is either ignored by this
/// checker or overrides the first in rpmbuild, so the checked value and the
/// built value could differ.
const SINGLE: [&str; 8] = [
    "Name", "Version", "Release", "License", "Summary", "URL", "Source0", "Epoch",
];

/// The architecture of a binary RPM is the native builder's. Any of these tags
/// would let the spec claim, restrict or override it independently of the ELF
/// files it packages.
const ARCHITECTURE_TAGS: [&str; 4] = [
    "BuildArch",
    "BuildArchitectures",
    "ExclusiveArch",
    "ExcludeArch",
];

/// License materials the package ships from the source tree. They are declared
/// with `%license` because Fedora container images and many installations set
/// `tsflags=nodocs`, which silently drops every `%doc` file at install time.
pub const LICENSE_MATERIALS: [&str; 3] = ["LICENSE", "NOTICE", "THIRD_PARTY"];

/// Minimum C library versions derived from the binding features in the root
/// manifest, keyed by pkgconfig capability.
pub fn binding_minimums(root: &Path) -> Result<Vec<(String, (u32, u32))>> {
    let manifest: toml::Value = toml::from_str(&fs::read_to_string(storage::safe_path(
        root,
        Path::new("Cargo.toml"),
    )?)?)?;
    let dependencies = manifest
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .ok_or("root manifest has no dependency table")?;
    let mut minimums = Vec::new();
    for (crate_name, capability) in BINDINGS {
        let mut found = None;
        for (key, value) in dependencies {
            let package = value
                .get("package")
                .and_then(toml::Value::as_str)
                .unwrap_or(key);
            if package != crate_name {
                continue;
            }
            if found.is_some() {
                return Err(format!("binding {crate_name} is declared twice").into());
            }
            let mut highest = None;
            for feature in value
                .get("features")
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(level) = feature
                    .as_str()
                    .and_then(|feature| feature.strip_prefix('v'))
                    .and_then(|level| level.split_once('_'))
                else {
                    continue;
                };
                let level = (level.0.parse::<u32>()?, level.1.parse::<u32>()?);
                highest = highest.max(Some(level));
            }
            found = Some(
                highest
                    .ok_or_else(|| format!("binding {crate_name} selects no API level feature"))?,
            );
        }
        let level =
            found.ok_or_else(|| format!("root manifest does not depend on {crate_name}"))?;
        minimums.push((capability.to_string(), level));
    }
    Ok(minimums)
}

/// `pkgconfig(name) >= X.Y` from one BuildRequires value.
fn declared_minimum(build_requires: &[&str], capability: &str) -> Result<(u32, u32)> {
    let mut declared = None;
    for value in build_requires {
        let mut words = value.split_whitespace();
        if words.next() != Some(capability) {
            continue;
        }
        if declared.is_some() {
            return Err(format!("RPM BuildRequires names {capability} twice").into());
        }
        let (Some(">="), Some(version), None) = (words.next(), words.next(), words.next()) else {
            return Err(format!("RPM BuildRequires must state {capability} >= a minimum").into());
        };
        let (major, minor) = version
            .split_once('.')
            .ok_or("RPM pkgconfig minimum must be MAJOR.MINOR")?;
        declared = Some((major.parse::<u32>()?, minor.parse::<u32>()?));
    }
    declared.ok_or_else(|| format!("RPM BuildRequires is missing {capability}").into())
}

/// Payload paths the Snap, Debian and RPM packages must agree on, so the three
/// distributions cannot silently diverge.
const PAYLOAD: [&str; 4] = [
    "%{_bindir}/among",
    "%{_datadir}/applications/com.github.among-terminal.desktop",
    "%{_datadir}/icons/hicolor/scalable/apps/com.github.among-terminal.svg",
    "%{_datadir}/metainfo/com.github.among-terminal.metainfo.xml",
];

/// Metadata directives that appear inside `%files` and carry no packaged path.
const METADATA: [&str; 5] = ["%license", "%doc", "%dir", "%config", "%ghost"];

/// `%{_bindir}/among` is a packaged path, but `%license LICENSE` is a directive.
/// Accepts the bare directive name, the line, or the name with an argument.
fn is_metadata_directive(token: &str) -> bool {
    let token = token.strip_prefix('%').unwrap_or(token);
    let word = token.split_whitespace().next().unwrap_or(token);
    METADATA
        .iter()
        .any(|directive| directive.strip_prefix('%') == Some(word))
}

const APPLICATION_ID: &str = "com.github.among-terminal";
const PLUGINS: [&str; 3] = ["clock", "system-status", "pane-pwd"];

/// Parse the `Key:` headers of the preamble, skipping comments and section
/// directives. Continuation lines belong to the previous field rather than
/// being headers themselves, and body text under `%description` is not a header
/// at all, so neither is treated as a malformed line.
fn headers(spec: &str) -> Result<Vec<(String, String)>> {
    let preamble = spec.split("\n%").next().unwrap_or(spec);
    let mut fields = Vec::new();
    for line in preamble.lines() {
        if line.trim().is_empty() || line.starts_with(['#', '%']) {
            continue;
        }
        if line.starts_with([' ', '\t']) {
            continue;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or("malformed RPM spec header line")?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err("invalid RPM spec field name".into());
        }
        fields.push((name.to_string(), value.trim().to_string()));
    }
    Ok(fields)
}

fn value<'a>(fields: &'a [(String, String)], name: &str) -> Result<&'a str> {
    fields
        .iter()
        .find(|(field, _)| field == name)
        .map(|(_, value)| value.as_str())
        .ok_or_else(|| format!("RPM spec is missing {name}").into())
}

/// Validate the spec and the packaging inputs it references, without building a
/// package. Packaging policy only: nothing here constructs or installs an RPM.
pub fn check(root: &Path, spec: &Path) -> Result<Value> {
    let relative = spec
        .strip_prefix(root)
        .map_err(|_| "RPM spec must live inside the project")?;
    let text = fs::read_to_string(storage::safe_path(root, relative)?)?;
    let fields = headers(&text)?;

    for name in REQUIRED {
        if value(&fields, name)?.is_empty() {
            return Err(format!("RPM spec field {name} is empty").into());
        }
    }
    for name in SINGLE {
        if fields.iter().filter(|(field, _)| field == name).count() > 1 {
            return Err(format!("RPM spec declares {name} more than once").into());
        }
    }
    for name in ARCHITECTURE_TAGS {
        if fields.iter().any(|(field, _)| field == name) {
            return Err(format!(
                "RPM spec must not set {name}: the native builder decides the package architecture"
            )
            .into());
        }
    }

    // Fedora naming rules: no version in the file name, no leading 'v' on
    // Version, and a distribution-tagged Release.
    if spec.file_name().and_then(|name| name.to_str()) != Some("among.spec") {
        return Err("RPM spec filename must be among.spec without a version".into());
    }
    let version = value(&fields, "Version")?;
    if version.starts_with('v') || version.is_empty() {
        return Err("RPM Version must not carry a leading v".into());
    }
    let release = value(&fields, "Release")?;
    if !release.ends_with("%{?dist}") {
        return Err("RPM Release must end with %{?dist} for a repository build".into());
    }
    if value(&fields, "Name")? != "among" {
        return Err("RPM package name must remain among".into());
    }

    // The version in the spec must be the version that will actually be built,
    // otherwise the filename and the contents disagree.
    let declared = package_version::read(root)?;
    if version != declared {
        return Err("RPM spec Version differs from the Cargo package version".into());
    }

    let build_requires = fields
        .iter()
        .filter(|(name, _)| name == "BuildRequires")
        .map(|(_, value)| value.as_str())
        .collect::<Vec<_>>();
    // A floor below the binding API level would let the spec build against a
    // library that lacks symbols the code calls, failing only at link or run
    // time on that older system.
    let mut minimums = Vec::new();
    for (capability, required) in binding_minimums(root)? {
        let declared = declared_minimum(&build_requires, &capability)?;
        if declared < required {
            return Err(format!(
                "RPM BuildRequires understates {capability}: declares {}.{}, the binding needs {}.{}",
                declared.0, declared.1, required.0, required.1
            )
            .into());
        }
        minimums.push(json!({
            "capability": capability,
            "declared": format!("{}.{}", declared.0, declared.1),
            "binding_api_level": format!("{}.{}", required.0, required.1),
        }));
    }

    // Section body: from `%<name>` at column zero until the next line that
    // begins a new `%section`. Directives such as `%license` and `%doc` are
    // content inside the body, not the start of another section.
    let section = |name: &str| -> Result<String> {
        let target = format!("%{name}");
        let all = text.lines().collect::<Vec<_>>();
        if !all.iter().any(|line| *line == target) {
            return Err(format!("RPM spec has no %{name} section").into());
        }
        let mut body = Vec::new();
        let mut inside = false;
        for line in all {
            if line == target {
                inside = true;
                continue;
            }
            if !inside {
                continue;
            }
            // A new section header is a bare `%word`. `%license`/`%doc` carry a
            // path, and `%{_bindir}/among` is itself a packaged path, so neither
            // ends the section that is being read.
            let starts_section = line.starts_with('%')
                && !line.starts_with("%{")
                && line[1..]
                    .split(|c: char| c.is_whitespace())
                    .next()
                    .is_some_and(|word| !word.is_empty() && !is_metadata_directive(word));
            if starts_section {
                break;
            }
            body.push(line);
        }
        Ok(body.join("\n"))
    };

    let files = section("files")?;
    // A packaged path may itself start with a macro such as `%{_bindir}`, so
    // only the `%license`/`%doc`/`%dir` metadata directives are skipped.
    let mut listed = BTreeSet::new();
    for line in files
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !is_metadata_directive(line))
    {
        // rpmbuild warns and continues on a path listed twice; the second
        // entry usually carries different attributes that silently win.
        if !listed.insert(line) {
            return Err(format!("RPM %files lists {line} more than once").into());
        }
    }
    for path in PAYLOAD {
        if !listed.contains(path) {
            return Err(format!("RPM %files is missing {path}").into());
        }
    }
    if !listed.contains("%{_datadir}/among/plugins") {
        return Err("RPM %files must ship the example plugin directory".into());
    }
    // Without ownership of the parent, erasing the package leaves an empty
    // /usr/share/among behind that no package will ever remove.
    if !files
        .lines()
        .any(|line| line.trim() == "%dir %{_datadir}/among")
    {
        return Err("RPM %files must own the %{_datadir}/among directory".into());
    }
    let mut licensed = BTreeSet::new();
    for line in files.lines().map(str::trim) {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("%license") => {
                for name in words {
                    if !licensed.insert(name) {
                        return Err(format!("RPM %license lists {name} more than once").into());
                    }
                }
            }
            Some("%doc") if words.any(|name| LICENSE_MATERIALS.contains(&name)) => {
                return Err(
                    "RPM license materials must be %license, never %doc, or nodocs installs drop them"
                        .into(),
                );
            }
            _ => {}
        }
    }
    for name in LICENSE_MATERIALS {
        if !licensed.contains(name) {
            return Err(format!("RPM %files must ship {name} as %license").into());
        }
        // rpmbuild copies these from the unpacked source; a declaration whose
        // file is absent from the tree fails only at package build time.
        let path = storage::safe_path(root, Path::new(name))?;
        let present = if name == "THIRD_PARTY" {
            path.join("inventory.json").is_file()
        } else {
            path.is_file()
        };
        if !present {
            return Err(format!("license material {name} is missing from the source tree").into());
        }
    }

    // The packaged example set must match the three shipped examples exactly.
    // A substring search would accept a loop that installs a fourth plugin or
    // drops one, so the loop itself is compared against the recorded set.
    let install = section("install")?;
    let loop_line = install
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("for plugin in") && line.ends_with("; do"))
        .ok_or("RPM install must name the shipped example plugins explicitly")?;
    let looped = loop_line
        .trim_start_matches("for plugin in ")
        .trim_end_matches("; do")
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let listed_plugins = looped.iter().cloned().collect::<BTreeSet<_>>();
    // A repeated name collapses in the set comparison below, yet installs the
    // same file twice and hides a typo that was meant to be another plugin.
    if listed_plugins.len() != looped.len() {
        return Err("RPM install loop names an example plugin more than once".into());
    }
    let expected = PLUGINS
        .iter()
        .map(|name| name.to_string())
        .collect::<BTreeSet<_>>();
    if listed_plugins != expected {
        return Err(format!(
            "RPM example plugin set differs from the shipped set: expected {expected:?}, found {listed_plugins:?}"
        )
        .into());
    }

    // A themed hicolor icon must remain a name, not a cwd-relative file path.
    let desktop = fs::read_to_string(storage::safe_path(
        root,
        Path::new(&format!("data/{APPLICATION_ID}.desktop")),
    )?)?;
    if !desktop.contains(&format!("Icon={APPLICATION_ID}\n")) {
        return Err("desktop entry does not name the themed application icon".into());
    }
    let metainfo = fs::read_to_string(storage::safe_path(
        root,
        Path::new(&format!("data/{APPLICATION_ID}.metainfo.xml")),
    )?)?;
    if !metainfo.contains(APPLICATION_ID) {
        return Err("AppStream metadata does not carry the application ID".into());
    }
    if install.contains("s|^Icon=") || !install.contains("s|^Exec=among$|Exec=/usr/bin/among|") {
        return Err("RPM must preserve themed icon and select its own absolute executable".into());
    }

    Ok(json!({
        "schema": 1,
        "name": "among",
        "version": version,
        "release": release,
        "build_requires": build_requires,
        "library_minimums": minimums,
        "license_materials": LICENSE_MATERIALS,
        "payload": listed.into_iter().collect::<Vec<_>>(),
        "scope": "RPM spec and packaging input validation only; no package is built, installed or published",
    }))
}

pub fn spec_path(root: &Path) -> PathBuf {
    root.join("packaging/rpm/among.spec")
}

pub fn release_spec(root: &Path, tag: &str) -> Result<(String, String)> {
    check(root, &spec_path(root))?;
    let (base, version) = crate::deb::release_version(tag)?;
    if crate::package_version::read(root)? != base {
        return Err("RPM release tag base differs from Cargo version".into());
    }
    let original = fs::read_to_string(spec_path(root))?;
    let mut replaced = 0;
    let transformed = original
        .lines()
        .map(|line| {
            if line.starts_with("Version:") {
                replaced += 1;
                format!("Version:        {version}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    if replaced != 1 {
        return Err("RPM spec must contain exactly one Version header".into());
    }
    Ok((version, transformed))
}

pub fn prepare_release(root: &Path, output: &Path) -> Result<String> {
    let environment = std::env::vars().collect();
    let (tag, _) = crate::deb::github_tag_source(root, &environment)?;
    let (version, spec) = release_spec(root, &tag)?;
    let parent = output.parent().ok_or("RPM spec output parent absent")?;
    storage::atomic_write(
        parent,
        Path::new(output.file_name().ok_or("spec name absent")?),
        spec.as_bytes(),
        0o644,
    )?;
    Ok(version)
}
