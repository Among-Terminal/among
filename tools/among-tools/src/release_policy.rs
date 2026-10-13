use crate::{deb, process::OwnedProcess, storage, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}
pub fn validate_record(record: &Value) -> Result<()> {
    let object = record.as_object().ok_or("provenance must be an object")?;
    let schema = record["schema"]
        .as_u64()
        .ok_or("unsupported provenance schema")?;
    let key = match schema {
        1 => "architecture",
        2 => "architectures",
        _ => return Err("unsupported provenance schema".into()),
    };
    // `mode` is optional and meaningful only on a publication record: a stable
    // release omits it, a nightly declares it. Any other extra field is refused.
    let mut expected = BTreeSet::from(["schema", "tag", "version", "source_sha", key, "assets"]);
    if object.contains_key("mode") {
        expected.insert("mode");
    }
    if object.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected {
        return Err("unsupported provenance fields".into());
    }
    // A present `mode` must be the exact string "nightly". A non-string value
    // is refused here rather than silently treated as absent, because the
    // caller dispatches on the same field: a `mode` of 5 or null would
    // otherwise pass nightly validation and then be routed down the stable
    // path, publishing a nightly build to the stable channel.
    if let Some(mode) = object.get("mode") {
        if mode.as_str() != Some("nightly") {
            return Err("unsupported publication mode".into());
        }
    }
    let tag = record["tag"].as_str().ok_or("stable tag missing")?;
    let source = record["source_sha"]
        .as_str()
        .filter(|value| hex(value, 40))
        .ok_or("invalid immutable source provenance")?;
    // A nightly is identified by its source, not by a semantic version, so the
    // stable tag rule cannot apply to it. The mode is carried by the caller and
    // removed from the record before this function sees it; an absent mode is a
    // stable release.
    if record.get("mode").is_none() {
        if record["version"] != deb::stable_version(tag)? {
            return Err("stable version mismatch".into());
        }
    } else {
        // `nightly-<full sha>` plus `n<YYYYMMDD>-<12 hex of the same sha>`.
        // The suffix is what makes a published edge build traceable back to the
        // source it claims, so it is required rather than merely formatted.
        let version = record["version"].as_str().unwrap_or_default();
        version
            .strip_prefix('n')
            .filter(|rest| {
                rest.len() == 8 + 1 + 12
                    && calendar_date(&rest.as_bytes()[..8])
                    && rest.as_bytes()[8] == b'-'
                    && rest.as_bytes()[9..].iter().all(|byte| {
                        byte.is_ascii_digit()
                            || byte.is_ascii_lowercase()
                                && !byte.is_ascii_uppercase()
                                && (byte.is_ascii_hexdigit())
                    })
            })
            .ok_or("invalid nightly identity")?;
        if tag != format!("nightly-{source}") {
            return Err("nightly tag must name its exact source".into());
        }
        if !version.ends_with(&source[..12]) {
            return Err("nightly version is not source-traceable".into());
        }
    }
    if schema == 1 && record["architecture"] != "amd64" {
        return Err("initial provenance supports only amd64".into());
    }
    let assets = record["assets"]
        .as_object()
        .filter(|object| !object.is_empty())
        .ok_or("release must include checked assets")?;
    for (name, digest) in assets {
        if name.is_empty()
            || !name.as_bytes()[0].is_ascii_alphanumeric()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
            return Err("unsafe asset name".into());
        }
        if !digest.as_str().is_some_and(|value| hex(value, 64)) {
            return Err("invalid asset digest".into());
        }
    }
    if schema == 2 {
        let groups = record["architectures"]
            .as_object()
            .ok_or("architecture groups must be an object")?;
        if groups.keys().map(String::as_str).collect::<BTreeSet<_>>()
            != ["amd64", "arm64"].into_iter().collect()
        {
            return Err("both release architectures are required".into());
        }
        let mut names = BTreeSet::new();
        let mut snaps = BTreeSet::new();
        for group in groups.values() {
            let group = group
                .as_object()
                .ok_or("architecture roles must be an object")?;
            if group.keys().map(String::as_str).collect::<BTreeSet<_>>()
                != ["snap", "source", "association"].into_iter().collect()
            {
                return Err("each architecture needs Snap, source and association".into());
            }
            for (role, suffix) in [
                ("snap", ".snap"),
                ("source", ".tar"),
                ("association", ".json"),
            ] {
                let name = group[role]
                    .as_str()
                    .ok_or("artifact role must be a string")?;
                if !assets.contains_key(name) || !name.ends_with(suffix) {
                    return Err("architecture references unchecked artifact or wrong role".into());
                }
                if !names.insert(name) {
                    return Err("architecture artifacts must be distinct".into());
                }
                if role == "snap" {
                    snaps.insert(name);
                }
            }
        }
        if assets
            .keys()
            .filter(|name| name.ends_with(".snap"))
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            != snaps
        {
            return Err("unexpected Snap artifact".into());
        }
    }
    Ok(())
}

pub fn git(repo: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8(git_bytes(repo, args)?)?
        .trim()
        .to_string())
}

pub fn git_bytes(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let repository = deb::repository_directory(repo)?;
    let safe = format!(
        "safe.directory={}",
        repository.to_str().ok_or("Git path is not UTF-8")?
    );
    let mut argv = vec!["-c", safe.as_str()];
    argv.extend_from_slice(args);
    let base = storage::private_directory(&std::env::temp_dir(), "among-policy-git")?;
    let result = (|| -> Result<Vec<u8>> {
        let file = File::create(base.join("git.log"))?;
        let errors = File::create(base.join("git.stderr"))?;
        let env = [
            ("PATH", "/usr/bin:/bin"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_TERMINAL_PROMPT", "0"),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
        let mut child = OwnedProcess::spawn_split(
            Path::new("/usr/bin/git"),
            &argv,
            &env,
            repo,
            &file,
            &errors,
        )?;
        if !child.wait(Duration::from_secs(30))?.success() {
            return Err("required Git reference or ancestry is unavailable".into());
        }
        Ok(fs::read(base.join("git.log"))?)
    })();
    fs::remove_dir_all(&base)?;
    result
}

/// Whether eight ASCII bytes `YYYYMMDD` name a day of the Gregorian calendar.
///
/// The date is how a reader places a nightly in time; eight digits that name
/// no day (month 13, 31 April, 29 February of a common year) are refused.
pub(crate) fn calendar_date(digits: &[u8]) -> bool {
    if digits.len() != 8 || !digits.iter().all(u8::is_ascii_digit) {
        return false;
    }
    let number = |range: std::ops::Range<usize>| {
        digits[range]
            .iter()
            .fold(0u32, |value, digit| value * 10 + u32::from(digit - b'0'))
    };
    let (year, month, day) = (number(0..4), number(4..6), number(6..8));
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&day)
}

/// Sort key of a stable tag: each component compared as a number of any
/// length (shorter digit strings are smaller, equal lengths compare as text).
pub fn order(tag: &str) -> Result<Vec<(usize, String)>> {
    // Strict decimal components permit arbitrary length, like the legacy
    // policy; numeric ordering must not silently overflow machine integers.
    Ok(deb::stable_version(tag)?
        .split('.')
        .map(|part| (part.len(), part.to_string()))
        .collect())
}

pub fn stable(
    repo: &Path,
    tag: &str,
    main: &str,
    assets: &[PathBuf],
    previous: Option<&Value>,
    architectures: Option<&Value>,
) -> Result<Value> {
    let version = deb::stable_version(tag)?;
    let source = git(
        repo,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/tags/{tag}^{{commit}}"),
        ],
    )?;
    if !hex(&source, 40) {
        return Err("release requires a full immutable SHA-1 commit".into());
    }
    git(repo, &["merge-base", "--is-ancestor", "--", &source, main])?;
    for manifest in ["Cargo.toml", "examples/plugins/Cargo.toml"] {
        let data = git(repo, &["show", &format!("{source}:{manifest}")])?;
        let document: toml::Value = toml::from_str(&data)?;
        if document
            .get("package")
            .and_then(|value| value.get("version"))
            .and_then(toml::Value::as_str)
            != Some(version)
        {
            return Err(format!("tag does not match {manifest}").into());
        }
    }
    let mut digests = BTreeMap::new();
    for asset in assets {
        if !asset.symlink_metadata()?.file_type().is_file() {
            return Err("assets must be regular non-symlink files".into());
        }
        let name = asset
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("asset filename is not UTF-8")?;
        if digests
            .insert(name.to_string(), storage::sha256(File::open(asset)?)?)
            .is_some()
        {
            return Err("duplicate asset filename".into());
        }
    }
    let mut record = json!({"schema":1,"tag":tag,"version":version,"source_sha":source,"architecture":"amd64","assets":digests});
    if let Some(groups) = architectures {
        record
            .as_object_mut()
            .ok_or("provenance object absent")?
            .remove("architecture");
        record["schema"] = 2.into();
        record["architectures"] = groups.clone();
    }
    validate_record(&record)?;
    if let Some(previous) = previous {
        validate_record(previous)?;
        let before = previous["tag"].as_str().ok_or("previous tag absent")?;
        match order(tag)?.cmp(&order(before)?) {
            std::cmp::Ordering::Equal if record != *previous => {
                return Err("immutable release conflicts with published provenance".into())
            }
            std::cmp::Ordering::Less => return Err("stable version must increase".into()),
            _ => {}
        }
    }
    Ok(record)
}
