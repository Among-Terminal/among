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
/// Days since 1970-01-01 of a proleptic Gregorian date, and back. These are
/// Howard Hinnant's `days_from_civil`/`civil_from_days`, exact for every year
/// a timestamp here can carry.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = of_era * 365 + of_era / 4 - of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * of_era + of_era / 4 - of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The UTC calendar date, as `YYYYMMDD`, of an ISO 8601 timestamp
/// `YYYY-MM-DDTHH:MM[:SS[.fraction]]` followed by `Z` or `±HH:MM`.
///
/// A timestamp without a timezone names no instant, so it is refused rather
/// than assumed to be UTC: near midnight that assumption picks the wrong day.
fn utc_date(now: &str) -> Result<String> {
    const INVALID: &str = "nightly timestamp must be ISO 8601 with an explicit timezone";
    let number = |text: &str| -> Result<i64> {
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(INVALID.into());
        }
        Ok(text.parse()?)
    };
    let (date, rest) = now.split_once('T').ok_or(INVALID)?;
    let [year, month, day] =
        <[&str; 3]>::try_from(date.split('-').collect::<Vec<_>>()).map_err(|_| INVALID)?;
    if year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return Err(INVALID.into());
    }
    let digits = format!("{year}{month}{day}");
    if !release_policy::calendar_date(digits.as_bytes()) {
        return Err(INVALID.into());
    }
    let (clock, offset_minutes) = if let Some(clock) = rest.strip_suffix('Z') {
        (clock, 0)
    } else {
        let sign_at = rest.rfind(['+', '-']).ok_or(INVALID)?;
        let (clock, offset) = rest.split_at(sign_at);
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let (hours, minutes) = offset[1..].split_once(':').ok_or(INVALID)?;
        if hours.len() != 2 || minutes.len() != 2 {
            return Err(INVALID.into());
        }
        let (hours, minutes) = (number(hours)?, number(minutes)?);
        if hours > 23 || minutes > 59 {
            return Err(INVALID.into());
        }
        (clock, sign * (hours * 60 + minutes))
    };
    let mut parts = clock.splitn(3, ':');
    let hour = parts.next().ok_or(INVALID)?;
    let minute = parts.next().ok_or(INVALID)?;
    if hour.len() != 2 || minute.len() != 2 {
        return Err(INVALID.into());
    }
    let (hour, minute) = (number(hour)?, number(minute)?);
    if hour > 23 || minute > 59 {
        return Err(INVALID.into());
    }
    if let Some(seconds) = parts.next() {
        let (whole, fraction) = seconds.split_once('.').unwrap_or((seconds, "0"));
        if whole.len() != 2 || number(whole)? > 59 {
            return Err(INVALID.into());
        }
        number(fraction)?;
    }
    let local =
        days_from_civil(number(year)?, number(month)?, number(day)?) * 1440 + hour * 60 + minute;
    let (year, month, day) = civil_from_days((local - offset_minutes).div_euclid(1440));
    Ok(format!("{year:04}{month:02}{day:02}"))
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
    selected["version"] = format!("n{}-{}", utc_date(now)?, &source[..12]).into();
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
