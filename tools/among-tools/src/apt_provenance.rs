use crate::{deb822, native_sources, process::OwnedProcess, storage, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const SIGNER: &str = "F6ECB3762474EDA9D21B7022871920D1991BC93C";
fn text<'a>(row: &'a Value, key: &str) -> Result<&'a str> {
    row[key]
        .as_str()
        .ok_or_else(|| format!("missing provenance field: {key}").into())
}
fn timestamp(date: &str) -> Result<i64> {
    let base = storage::private_directory(&std::env::temp_dir(), "among-apt-date")?;
    let result = (|| -> Result<i64> {
        let stdout = File::create(base.join("stdout"))?;
        let stderr = File::create(base.join("stderr"))?;
        let env = BTreeMap::from([
            ("LC_ALL".into(), "C.UTF-8".into()),
            ("TZ".into(), "UTC".into()),
        ]);
        let mut process = OwnedProcess::spawn_split(
            Path::new("/usr/bin/date"),
            &["--date", date, "+%s"],
            &env,
            &base,
            &stdout,
            &stderr,
        )?;
        if !process.wait(Duration::from_secs(10))?.success() {
            return Err("invalid APT repository timestamp".into());
        }
        Ok(fs::read_to_string(base.join("stdout"))?.trim().parse()?)
    })();
    fs::remove_dir_all(base)?;
    result
}

pub fn signature_status(success: bool, status: &str) -> bool {
    success
        && status
            .lines()
            .filter_map(|line| line.strip_prefix("[GNUPG:] VALIDSIG "))
            .any(|line| line.split_whitespace().next() == Some(SIGNER))
}

pub fn authenticate_with(
    root: &Path,
    native: &Value,
    sources: &Path,
    now: i64,
    mut verify: impl FnMut(&Path, &Path) -> Result<bool>,
) -> Result<Value> {
    let inventory =
        storage::parse_json(&fs::read(native_sources::regular(root, "inventory.json")?)?)?;
    let indices = inventory["indices"]
        .as_array()
        .filter(|indices| !indices.is_empty())
        .ok_or("empty APT metadata")?;
    if inventory["schema"] != 1 {
        return Err("unsupported APT metadata".into());
    }
    let date = text(&inventory, "collected_at")?;
    let time = date
        .split_once('T')
        .ok_or("metadata collection time must have an explicit timezone")?
        .1;
    if !time.ends_with('Z') && !time.contains('+') && !time.contains('-') {
        return Err("metadata collection time must have an explicit timezone".into());
    }
    let collected = timestamp(date)?;
    if collected > now.checked_add(300).ok_or("invalid current time")? {
        return Err("invalid future metadata collection time".into());
    }
    let keyring = native_sources::regular(root, "archive-keyring.gpg")?.canonicalize()?;
    let mut releases: BTreeMap<String, BTreeMap<String, (String, u64)>> = BTreeMap::new();
    let mut binaries: BTreeSet<(String, String, String, String, String)> = BTreeSet::new();
    let mut archives = BTreeSet::new();
    for index in indices {
        let role = text(index, "role")?;
        if !matches!(role, "binary" | "source") {
            return Err("unsupported APT index role".into());
        }
        let release_name = text(index, "release")?;
        if !releases.contains_key(release_name) {
            let release = native_sources::regular(root, release_name)?;
            if !verify(&release, &keyring)? {
                return Err("release is not signed by the pinned Ubuntu archive key".into());
            }
            let fields = deb822::fields(&fs::read_to_string(release)?)?;
            if deb822::required(&fields, "Origin")? != "Ubuntu"
                || deb822::required(&fields, "Codename")? != "noble"
            {
                return Err("unexpected repository origin/codename".into());
            }
            if timestamp(deb822::required(&fields, "Date")?)?
                > collected
                    .checked_add(86400)
                    .ok_or("invalid collection time")?
            {
                return Err("repository metadata is from the future".into());
            }
            if let Some(until) = fields.get("valid-until").filter(|value| !value.is_empty()) {
                if timestamp(until)? < collected {
                    return Err("repository metadata expired before collection".into());
                }
            }
            let mut members = BTreeMap::new();
            for line in deb822::required(&fields, "SHA256")?
                .lines()
                .filter(|line| !line.trim().is_empty())
            {
                let (digest, size, name) = deb822::checksum(line)?;
                if members.insert(name.into(), (digest.into(), size)).is_some() {
                    return Err("duplicate authenticated release member".into());
                }
            }
            releases.insert(release_name.into(), members);
        }
        let path = native_sources::regular(root, text(index, "file")?)?;
        let expected = releases
            .get(release_name)
            .and_then(|release| release.get(index["release_member"].as_str()?));
        let observed = (storage::sha256(File::open(&path)?)?, path.metadata()?.len());
        if expected != Some(&observed) {
            return Err("index digest/size not covered by authenticated release".into());
        }
        for paragraph in fs::read_to_string(path)?
            .replace("\r\n", "\n")
            .split("\n\n")
            .filter(|paragraph| !paragraph.trim().is_empty())
        {
            let fields = deb822::fields(paragraph)?;
            let Some(package) = fields.get("package") else {
                continue;
            };
            let version = deb822::required(&fields, "Version")?;
            if role == "binary" {
                let filename = Path::new(deb822::required(&fields, "Filename")?)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or("binary index filename absent")?;
                binaries.insert((
                    package.clone(),
                    version.into(),
                    deb822::required(&fields, "Architecture")?.to_string(),
                    filename.to_string(),
                    deb822::required(&fields, "SHA256")?.to_string(),
                ));
            } else if let Some(checksums) = fields.get("checksums-sha256") {
                for line in checksums.lines().filter(|line| !line.trim().is_empty()) {
                    let (digest, size, filename) = deb822::checksum(line)?;
                    archives.insert((
                        package.clone(),
                        version.to_string(),
                        filename.to_string(),
                        digest.to_string(),
                        size,
                    ));
                }
            }
        }
    }
    let packages = native["packages"]
        .as_array()
        .ok_or("native input inventory absent")?;
    for row in packages {
        let entry = (
            text(row, "package")?.into(),
            text(row, "version")?.into(),
            text(row, "architecture")?.into(),
            text(&row["input"], "filename")?.into(),
            text(&row["input"], "sha256")?.into(),
        );
        if !binaries.contains(&entry) {
            return Err("binary input not authenticated".into());
        }
    }
    let source_inventory = storage::parse_json(&fs::read(native_sources::regular(
        sources,
        "inventory.json",
    )?)?)?;
    let records = source_inventory["sources"]
        .as_array()
        .ok_or("source inventory absent")?;
    for row in records {
        for file in row["files"]
            .as_array()
            .ok_or("source file inventory absent")?
        {
            let filename = Path::new(text(file, "path")?)
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("source filename absent")?;
            let entry = (
                text(row, "source_package")?.to_string(),
                text(row, "source_version")?.to_string(),
                filename.to_string(),
                text(file, "sha256")?.to_string(),
                file["bytes"].as_u64().ok_or("source file size absent")?,
            );
            if !archives.contains(&entry) {
                return Err("source input not authenticated".into());
            }
        }
    }
    native_sources::check(native, sources)?;
    Ok(
        json!({"schema":1,"signer":SIGNER,"binary_inputs":packages.len(),"source_versions":records.len(),"indices":indices.len(),"signed_releases":releases.len(),"trust":"Ubuntu pinned archive signing key -> InRelease -> index -> exact input digests","scope":"repository authentication, not legal sign-off or an independent .dsc signer audit"}),
    )
}

pub fn authenticate(root: &Path, native: &Value, sources: &Path, scratch: &Path) -> Result<Value> {
    let now = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())?;
    let base = storage::private_directory(scratch, "among-apt-signature")?;
    let mut counter = 0;
    authenticate_with(root, native, sources, now, |release, keyring| {
        counter += 1;
        let stdout = base.join(format!("gpgv-{counter}.status"));
        let errors = File::create(base.join(format!("gpgv-{counter}.stderr")))?;
        let env = BTreeMap::from([
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("GNUPGHOME".into(), base.as_os_str().to_owned()),
        ]);
        let mut process = OwnedProcess::spawn_split(
            Path::new("/usr/bin/gpgv"),
            &[
                "--status-fd",
                "1",
                "--keyring",
                keyring.to_str().ok_or("keyring path is not UTF-8")?,
                release.to_str().ok_or("release path is not UTF-8")?,
            ],
            &env,
            &base,
            &File::create(&stdout)?,
            &errors,
        )?;
        let success = process.wait(Duration::from_secs(60))?.success();
        Ok(signature_status(success, &fs::read_to_string(stdout)?))
    })
}
