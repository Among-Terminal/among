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
    let joined = build_requires.join(" ");
    for capability in PKGCONFIG {
        if !joined.contains(capability) {
            return Err(format!("RPM BuildRequires is missing {capability}").into());
        }
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
    let listed = files
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !is_metadata_directive(line))
        .collect::<BTreeSet<_>>();
    for path in PAYLOAD {
        if !listed.contains(path) {
            return Err(format!("RPM %files is missing {path}").into());
        }
    }
    if !listed.contains("%{_datadir}/among/plugins") {
        return Err("RPM %files must ship the example plugin directory".into());
    }
    if !files.contains("%license LICENSE") {
        return Err("RPM %files must ship the original license".into());
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
    let listed_plugins = loop_line
        .trim_start_matches("for plugin in ")
        .trim_end_matches("; do")
        .split_whitespace()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
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

    // Identity must agree across the desktop entry, the AppStream metadata and
    // the packaged icon. The source desktop entry deliberately names the themed
    // icon; each packager rewrites it to the installed file, exactly as the
    // Debian and Snap builds already do, so the rewrite is verified here rather
    // than assumed of the source.
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
    // Without the rewrite the packaged desktop entry would ask the icon theme
    // for a name that no longer resolves once the file is installed directly.
    if !install.contains(&format!(
        "s|^Icon={APPLICATION_ID}$|Icon=usr/share/icons/hicolor/scalable/apps/{APPLICATION_ID}.svg|"
    )) {
        return Err("RPM install does not point the desktop entry at the packaged icon".into());
    }

    Ok(json!({
        "schema": 1,
        "name": "among",
        "version": version,
        "release": release,
        "build_requires": build_requires,
        "payload": listed.into_iter().collect::<Vec<_>>(),
        "scope": "RPM spec and packaging input validation only; no package is built, installed or published",
    }))
}

pub fn spec_path(root: &Path) -> PathBuf {
    root.join("packaging/rpm/among.spec")
}
