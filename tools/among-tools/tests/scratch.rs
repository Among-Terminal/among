use among_tools::{scratch, Result};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn env(pairs: &[(&str, &str)]) -> BTreeMap<OsString, OsString> {
    pairs
        .iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
        .collect()
}

#[test]
fn explicit_root_wins_over_cache_and_home() -> Result<()> {
    let resolved = scratch::resolve(&env(&[
        ("AMONG_CHECK_ROOT", "/srv/evidence"),
        ("XDG_CACHE_HOME", "/home/demo/.cache"),
        ("HOME", "/home/demo"),
    ]))?;
    assert_eq!(resolved, Path::new("/srv/evidence"));
    Ok(())
}

#[test]
fn default_root_lives_in_the_user_cache_on_disk() -> Result<()> {
    assert_eq!(
        scratch::resolve(&env(&[
            ("XDG_CACHE_HOME", "/home/demo/cache"),
            ("HOME", "/home/demo")
        ]))?,
        Path::new("/home/demo/cache/among/checks")
    );
    assert_eq!(
        scratch::resolve(&env(&[("HOME", "/home/demo")]))?,
        Path::new("/home/demo/.cache/among/checks")
    );
    // A relative XDG_CACHE_HOME is invalid by the XDG Base Directory rules
    // and is ignored rather than resolved against the working directory.
    assert_eq!(
        scratch::resolve(&env(&[("XDG_CACHE_HOME", "cache"), ("HOME", "/home/demo")]))?,
        Path::new("/home/demo/.cache/among/checks")
    );
    Ok(())
}

#[test]
fn unusable_settings_are_refused_instead_of_guessed() {
    // An explicit root that is ignored would scatter evidence where nobody
    // looks; a missing HOME must not fall back to a temporary directory.
    for pairs in [
        vec![
            ("AMONG_CHECK_ROOT", "relative/evidence"),
            ("HOME", "/home/demo"),
        ],
        vec![("AMONG_CHECK_ROOT", ""), ("HOME", "/home/demo")],
        vec![("HOME", "relative-home")],
        vec![],
    ] {
        assert!(scratch::resolve(&env(&pairs)).is_err(), "{pairs:?}");
    }
}

#[test]
fn created_root_is_private_canonical_and_reused() -> Result<()> {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("among-scratch-create-{}", std::process::id()));
    let requested = base.join("cache/among/checks");
    let created = scratch::create(&requested)?;
    assert_eq!(created, requested.canonicalize()?);
    for directory in [
        base.join("cache"),
        base.join("cache/among"),
        requested.clone(),
    ] {
        assert_eq!(
            fs::metadata(&directory)?.permissions().mode() & 0o777,
            0o700,
            "{}",
            directory.display()
        );
    }
    fs::write(created.join("evidence.txt"), b"kept")?;
    assert_eq!(scratch::create(&requested)?, created);
    assert_eq!(fs::read(created.join("evidence.txt"))?, b"kept");
    fs::remove_dir_all(&base)?;
    Ok(())
}

/// The former agent's scratch path lived in tmpfs, so evidence vanished on
/// reboot. Any reappearance in sources, workflows, packaging or documentation
/// fails here. The needle is assembled at run time so this file and its
/// compiled test binary do not contain it.
#[test]
fn the_former_tmpfs_scratch_path_never_returns_to_the_public_tree() -> Result<()> {
    let needle = ["", "tmp", "opencode"].join("/");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let mut offenders = Vec::new();
    let mut scanned = 0usize;
    let mut pending = vec![root.clone()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let kind = entry.file_type()?;
            // Symlinks lead into private or generated trees outside the
            // public repository (process notes, video sources).
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                let private = name.starts_with('.') && name != ".github";
                let build_output = name == "target" || name.ends_with("-target");
                if !private && !build_output {
                    pending.push(entry.path());
                }
                continue;
            }
            scanned += 1;
            if fs::read(entry.path())?
                .windows(needle.len())
                .any(|window| window == needle.as_bytes())
            {
                offenders.push(entry.path());
            }
        }
    }
    // Guards against a walk that silently scans nothing, e.g. a wrong root.
    assert!(
        scanned > 100 && root.join(".github/workflows/checks.yml").is_file(),
        "scanned only {scanned} files below {}",
        root.display()
    );
    assert!(
        offenders.is_empty(),
        "former tmpfs scratch path found in {offenders:?}"
    );
    Ok(())
}
