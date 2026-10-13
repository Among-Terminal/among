//! The single place that decides where checks keep their work directories
//! and evidence. Evidence has to outlive the run that produced it (it is
//! inspected afterwards and uploaded on failure), so the default lives in the
//! user's cache directory on disk rather than in a temporary file system that
//! a reboot erases.

use crate::Result;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

/// Environment variable naming the check root explicitly. Harnesses pass it
/// to every child that needs the root, because they rewrite `HOME` and
/// `XDG_*` for those children and a recomputed default would differ.
pub const VARIABLE: &str = "AMONG_CHECK_ROOT";

/// Chooses the check root from an environment without touching the file
/// system: `AMONG_CHECK_ROOT`, else `$XDG_CACHE_HOME/among/checks`, else
/// `$HOME/.cache/among/checks`.
pub fn resolve(env: &BTreeMap<OsString, OsString>) -> Result<PathBuf> {
    let get = |key: &str| env.get(OsStr::new(key)).map(PathBuf::from);
    if let Some(explicit) = get(VARIABLE) {
        // An explicit setting that were silently ignored would put evidence
        // where nobody looks for it.
        if !explicit.is_absolute() {
            return Err(format!("{VARIABLE} must be an absolute path").into());
        }
        return Ok(explicit);
    }
    // The XDG Base Directory specification declares relative values invalid
    // and asks implementations to ignore them.
    if let Some(cache) = get("XDG_CACHE_HOME").filter(|path| path.is_absolute()) {
        return Ok(cache.join("among/checks"));
    }
    match get("HOME") {
        Some(home) if home.is_absolute() => Ok(home.join(".cache/among/checks")),
        _ => Err(format!(
            "set {VARIABLE}, XDG_CACHE_HOME or HOME to an absolute path for check evidence"
        )
        .into()),
    }
}

/// Resolves the check root for this process, creates it if missing and
/// returns its canonical path. Newly created components are private (0700);
/// an existing root is used as found, because shared CI mounts and roots
/// handed from a privileged orchestrator to a check user are legitimately
/// owned or moded differently.
pub fn root() -> Result<PathBuf> {
    create(&resolve(&std::env::vars_os().collect())?)
}

/// Creates `path` like `root` does and returns its canonical form.
pub fn create(path: &Path) -> Result<PathBuf> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    Ok(path.canonicalize()?)
}
