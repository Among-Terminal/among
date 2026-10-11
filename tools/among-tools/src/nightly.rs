use crate::{
    process::{allowed_environment, OwnedProcess},
    release_policy, storage, Result,
};
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct PrivateScratch(std::path::PathBuf);
impl Drop for PrivateScratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn source_identity(source: &str) -> Result<()> {
    if source.len() != 40
        || !source
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err("full nightly source commit required".into());
    }
    Ok(())
}
pub fn selection(
    source: &str,
    marker: Option<&Value>,
    now: &str,
    prepared: Option<&Value>,
) -> Result<Value> {
    source_identity(source)?;
    let mut selected = json!({"source_sha":source,"skip":marker.is_some_and(|record|record["source_sha"]==source)});
    if selected["skip"] == true {
        return Ok(selected);
    }
    let tag = format!("nightly-{source}");
    if let Some(record) = prepared {
        if record["mode"] != "nightly" || record["source_sha"] != source || record["tag"] != tag {
            return Err("immutable nightly retry identity conflict".into());
        }
        let version = record["version"]
            .as_str()
            .ok_or("invalid prepared nightly version")?;
        let bytes = version.as_bytes();
        if bytes.len() != 22
            || bytes[0] != b'n'
            || !bytes[1..9].iter().all(u8::is_ascii_digit)
            || bytes[9] != b'-'
            || version.get(10..) != source.get(..12)
        {
            return Err("invalid prepared nightly version".into());
        }
        selected["version"] = version.into();
        selected["tag"] = tag.into();
        selected["resume"] = true.into();
        return Ok(selected);
    }
    let date = now
        .get(..10)
        .ok_or("nightly timestamp must begin with ISO date")?;
    if date.as_bytes()[4] != b'-'
        || date.as_bytes()[7] != b'-'
        || !date
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return Err("nightly timestamp must begin with ISO date".into());
    }
    selected["version"] = format!("n{}-{}", date.replace('-', ""), &source[..12]).into();
    selected["tag"] = tag.into();
    selected["resume"] = false.into();
    Ok(selected)
}

pub fn release_response(
    success: bool,
    stdout: &[u8],
    stderr: &str,
) -> Result<Option<(Value, bool)>> {
    if !success {
        if stderr.contains("HTTP 404") {
            return Ok(None);
        }
        return Err(
            "cannot read nightly provenance; connection/auth failure is not missing state".into(),
        );
    }
    let data = storage::parse_json(stdout)?;
    let body = storage::parse_json(
        data["body"]
            .as_str()
            .ok_or("nightly release body absent")?
            .as_bytes(),
    )?;
    let record = &body["record"];
    let source = record["source_sha"]
        .as_str()
        .ok_or("nightly provenance source absent")?;
    source_identity(source)?;
    if record["mode"] != "nightly" || record["tag"] != format!("nightly-{source}") {
        return Err("invalid nightly provenance/tag association".into());
    }
    let draft = data["draft"]
        .as_bool()
        .ok_or("nightly draft state must be boolean")?;
    Ok(Some((
        record.clone(),
        body["state"] == "complete" && !draft,
    )))
}

pub fn select(
    source: &str,
    now: &str,
    mut read: impl FnMut(&str) -> Result<Option<(Value, bool)>>,
) -> Result<Value> {
    source_identity(source)?;
    let rolling = read("nightly")?;
    let marker = rolling
        .as_ref()
        .filter(|(_, complete)| *complete)
        .map(|(record, _)| record);
    if marker.is_some_and(|record| record["source_sha"] == source) {
        return selection(source, marker, now, None);
    }
    let prepared = read(&format!("nightly-{source}"))?;
    selection(
        source,
        marker,
        now,
        prepared.as_ref().map(|(record, _)| record),
    )
}

pub fn run(
    repo: &Path,
    repository: &str,
    output: &Path,
    now: Option<&str>,
    scratch: &Path,
) -> Result<()> {
    let parts = repository.split('/').collect::<Vec<_>>();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
        })
    {
        return Err("invalid nightly repository".into());
    }
    let source = release_policy::git(
        repo,
        &["rev-parse", "--verify", "refs/remotes/origin/main^{commit}"],
    )?;
    let base = scratch.join(format!(
        "among-nightly-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    fs::DirBuilder::new().mode(0o700).create(&base)?;
    let _private = PrivateScratch(base.clone());
    let input = std::env::vars_os().collect();
    // Git resolution never sees these credentials. Only gh's read-only API
    // invocation needs the narrowly scoped GitHub token; logs are not echoed.
    let env = allowed_environment(&input, &["PATH", "HOME", "GH_TOKEN", "GITHUB_TOKEN"]);
    let path = env
        .get(&std::ffi::OsString::from("PATH"))
        .ok_or("nightly PATH absent")?;
    let gh = std::env::split_paths(path)
        .map(|directory| directory.join("gh"))
        .find(|path| path.is_file())
        .ok_or("GitHub CLI unavailable")?;
    let timestamp;
    let now = if let Some(now) = now {
        now
    } else {
        let log = File::create(base.join("date.log"))?;
        let errors = File::create(base.join("date.stderr"))?;
        let mut process = OwnedProcess::spawn_split(
            Path::new("/usr/bin/date"),
            &["--utc", "+%Y-%m-%dT%H:%M:%SZ"],
            &std::collections::BTreeMap::new(),
            &base,
            &log,
            &errors,
        )?;
        if !process.wait(Duration::from_secs(10))?.success() {
            return Err("nightly UTC timestamp unavailable".into());
        }
        timestamp = fs::read_to_string(base.join("date.log"))?;
        timestamp.trim()
    };
    let selected = select(&source, now, |tag| {
        let stdout = base.join(format!("{tag}.json"));
        let stderr = base.join(format!("{tag}.stderr"));
        let mut process = OwnedProcess::spawn_split(
            &gh,
            &["api", &format!("repos/{repository}/releases/tags/{tag}")],
            &env,
            repo,
            &File::create(&stdout)?,
            &File::create(&stderr)?,
        )?;
        let success = process.wait(Duration::from_secs(60))?.success();
        release_response(success, &fs::read(stdout)?, &fs::read_to_string(stderr)?)
    })?;
    let mut bytes = serde_json::to_vec_pretty(&selected)?;
    bytes.push(b'\n');
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    println!(
        "{}",
        if selected["skip"] == true {
            "unchanged"
        } else if selected["resume"] == true {
            "resume"
        } else {
            "build"
        }
    );
    // API error bodies may contain sensitive diagnostics. Scratch cleanup also
    // runs on failure; no public receipt includes endpoint error text.
    fs::remove_dir_all(base)?;
    Ok(())
}
