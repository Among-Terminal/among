//! Checks on the bytes a release RPM actually carries, run in the build job
//! after `rpmbuild` and before the candidate leaves it.
//!
//! The spec checker proves what the spec declares; these prove what the
//! package and its `Source0` archive contain.

use crate::{storage, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::Path;

/// Where `%license` places the license materials of package `among`.
pub const LICENSE_DIRECTORY: &str = "usr/share/licenses/among";

/// Relative path to SHA-256 of every regular file below `directory`.
fn tree(directory: &Path) -> Result<BTreeMap<String, String>> {
    fn walk(root: &Path, relative: &Path, out: &mut BTreeMap<String, String>) -> Result<()> {
        for entry in fs::read_dir(root.join(relative))? {
            let entry = entry?;
            let relative = relative.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_dir() {
                walk(root, &relative, out)?;
            } else if kind.is_file() {
                let name = relative.to_str().ok_or("notice path is not UTF-8")?;
                out.insert(
                    name.to_string(),
                    storage::sha256(File::open(entry.path())?)?,
                );
            } else {
                return Err(format!(
                    "unexpected non-regular notice member {name:?}",
                    name = relative
                )
                .into());
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    if !directory.symlink_metadata()?.is_dir() {
        return Err(format!("{} is not a directory", directory.display()).into());
    }
    walk(directory, Path::new(""), &mut out)?;
    Ok(out)
}

/// Compare the license materials inside an extracted RPM payload with the
/// source tree, and separate what the package ships from what Fedora supplies.
///
/// `requires` is the output of `rpm -qp --requires`. The locked-graph check of
/// the notice texts themselves is `third-party-notices <root> <dir> check`,
/// run on the packaged directory by the same workflow step; this function adds
/// the parts that check cannot see: a stale or extra packaged file, a missing
/// original license, and a bundled native library.
pub fn check_notices(root: &Path, payload: &Path, requires: &str) -> Result<Value> {
    let licenses = storage::safe_path(payload, Path::new(LICENSE_DIRECTORY))?;
    for name in ["LICENSE", "NOTICE"] {
        let packaged = licenses.join(name);
        if !packaged.symlink_metadata().is_ok_and(|meta| meta.is_file()) {
            return Err(
                format!("the package does not ship {name} under /{LICENSE_DIRECTORY}").into(),
            );
        }
        if fs::read(&packaged)? != fs::read(storage::safe_path(root, Path::new(name))?)? {
            return Err(format!("packaged {name} differs from the tagged source").into());
        }
    }
    let packaged = tree(&licenses.join("THIRD_PARTY"))?;
    let source = tree(&storage::safe_path(root, Path::new("THIRD_PARTY"))?)?;
    if packaged != source {
        let missing = source
            .keys()
            .filter(|name| !packaged.contains_key(*name))
            .count();
        let extra = packaged
            .keys()
            .filter(|name| !source.contains_key(*name))
            .count();
        return Err(format!(
            "packaged THIRD_PARTY differs from the tagged source: {missing} missing, {extra} extra, {} changed",
            packaged
                .iter()
                .filter(|(name, digest)| source.get(*name).is_some_and(|other| other != *digest))
                .count()
        )
        .into());
    }

    // Native libraries are Fedora's: their licenses ship with their own
    // packages. A shared object inside this payload would make it ours to
    // license and to keep patched, which this package does not do.
    let mut bundled = Vec::new();
    for name in tree(payload)?.keys() {
        let file = Path::new(name)
            .file_name()
            .and_then(|file| file.to_str())
            .unwrap_or_default();
        if file.ends_with(".so") || file.contains(".so.") {
            bundled.push(name.clone());
        }
    }
    if !bundled.is_empty() {
        return Err(format!("the payload bundles native libraries: {bundled:?}").into());
    }
    let system = requires
        .lines()
        .map(str::trim)
        .filter(|line| line.contains(".so"))
        .map(str::to_string)
        .collect::<Vec<_>>();
    if system.is_empty() {
        return Err("the package declares no system shared library requirement".into());
    }
    let inventory = storage::parse_json(&fs::read(licenses.join("THIRD_PARTY/inventory.json"))?)?;
    let shipped = inventory["packages"]
        .as_array()
        .ok_or("notice inventory has no package list")?
        .iter()
        .map(|package| {
            Ok(format!(
                "{}-{}",
                package["name"]
                    .as_str()
                    .ok_or("notice package name absent")?,
                package["version"]
                    .as_str()
                    .ok_or("notice package version absent")?
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(json!({
        "schema": 1,
        "license_directory": format!("/{LICENSE_DIRECTORY}"),
        "rust_components_noticed": shipped.len(),
        "system_native_libraries": system,
        "scope": "packaged LICENSE/NOTICE/THIRD_PARTY bytes equal the tagged source; native libraries are required from Fedora, none is shipped; the notice inventory covers all three locked Cargo graphs, a superset of the crates linked into the packaged executables",
    }))
}

/// File names GNU tar `--exclude-vcs` drops at any depth, among those Git
/// tracks. A tracked `.gitignore` is legitimately absent from `Source0`.
const VCS_METADATA: [&str; 3] = [".gitignore", ".gitattributes", ".gitmodules"];

/// Tracked top-level directories the release workflow leaves out of `Source0`
/// on purpose: CI definitions are not inputs to building the package.
const EXCLUDED_TRACKED: [&str; 1] = [".github/"];

/// Top-level names that must never reach a published source archive, even if
/// one were committed by mistake: process notes, agent state and the private
/// video workshop live beside the public tree, not in it.
const PRIVATE: [&str; 6] = [
    ".private",
    ".claude",
    ".opencode",
    ".agents",
    "AGENTS.md",
    "video",
];

/// Prove that a `Source0` member listing (`tar -tzf`) is exactly the tracked
/// files of the tagged commit under `prefix/`, minus the deliberate exclusions.
/// An untracked file in the checkout, a build directory or a private file
/// would otherwise ship inside the SRPM unnoticed.
pub fn check_source_listing(tracked: &[String], listing: &str, prefix: &str) -> Result<usize> {
    let top = format!("{prefix}/");
    let mut members = BTreeMap::new();
    for line in listing.lines() {
        // `tar -c .` records the archived directory itself as `./`; the
        // workflow's `--transform` rewrites only names below it.
        if line == "./" {
            continue;
        }
        let name = line
            .strip_prefix(&top)
            .ok_or_else(|| format!("Source0 member outside {top}: {line}"))?;
        if name.is_empty() || name.ends_with('/') {
            continue;
        }
        let first = name.split('/').next().unwrap_or(name);
        if PRIVATE.contains(&first)
            || name
                .split('/')
                .any(|part| matches!(part, "target" | ".git"))
        {
            return Err(format!("Source0 carries a private or build path: {name}").into());
        }
        if members.insert(name.to_string(), ()).is_some() {
            return Err(format!("Source0 lists {name} twice").into());
        }
    }
    let expected = tracked
        .iter()
        .filter(|name| {
            !EXCLUDED_TRACKED
                .iter()
                .any(|prefix| name.starts_with(prefix))
        })
        .filter(|name| {
            let file = name.rsplit('/').next().unwrap_or(name);
            !VCS_METADATA.contains(&file)
        })
        .map(|name| (name.clone(), ()))
        .collect::<BTreeMap<_, _>>();
    if members != expected {
        let extra = members
            .keys()
            .filter(|name| !expected.contains_key(*name))
            .collect::<Vec<_>>();
        let missing = expected
            .keys()
            .filter(|name| !members.contains_key(*name))
            .collect::<Vec<_>>();
        return Err(format!(
            "Source0 differs from the tagged tree: extra {extra:?}, missing {missing:?}"
        )
        .into());
    }
    Ok(members.len())
}

/// Files of the commit `HEAD` in `root`, read from the commit tree rather than
/// the index, so a staged but uncommitted file cannot widen the expectation.
pub fn tracked_files(root: &Path) -> Result<Vec<String>> {
    let root = root.canonicalize()?;
    let safe = format!(
        "safe.directory={}",
        root.to_str().ok_or("root is not UTF-8")?
    );
    let output = std::process::Command::new("git")
        .args(["-c", &safe, "-C"])
        .arg(&root)
        .args(["ls-tree", "-r", "-z", "--name-only", "HEAD"])
        .output()?;
    if !output.status.success() {
        return Err("git ls-files failed for the tagged checkout".into());
    }
    Ok(String::from_utf8(output.stdout)?
        .split('\0')
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect())
}
