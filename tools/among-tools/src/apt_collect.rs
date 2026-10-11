use crate::{platform, process::OwnedProcess, storage, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    path::Path,
    time::Duration,
};

pub fn index_identity(name: &str) -> Result<Option<Value>> {
    if !name.contains("_dists_")
        || !["_Sources", "_Sources.lz4", "_Packages", "_Packages.lz4"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
    {
        return Ok(None);
    }
    let (prefix, rest) = name
        .split_once("_dists_")
        .ok_or("APT index release prefix absent")?;
    let unpacked = rest.strip_suffix(".lz4").unwrap_or(rest);
    let parts = unpacked.splitn(3, '_').collect::<Vec<_>>();
    if prefix.is_empty() || parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
        return Err("malformed APT index filename".into());
    }
    let (suite, component, kind) = (parts[0], parts[1], parts[2]);
    let role = if kind == "source_Sources" {
        "source"
    } else if kind.starts_with("binary-") && kind.ends_with("_Packages") {
        "binary"
    } else {
        return Err("unsupported APT index filename role".into());
    };
    let file = name.strip_suffix(".lz4").unwrap_or(name);
    storage::relative_path(Path::new(file))?;
    let release = format!("{prefix}_dists_{suite}_InRelease");
    storage::relative_path(Path::new(&release))?;
    Ok(Some(
        json!({"file":file,"release":release,"release_member":format!("{component}/{}",kind.replace('_',"/")),"role":role}),
    ))
}
pub fn enable_sources(text: &str) -> Result<String> {
    let mut output = String::new();
    for line in text.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        let content = content.strip_suffix('\r').unwrap_or(content);
        if let Some(types) = content.strip_prefix("Types:") {
            let types = types.split_whitespace().collect::<Vec<_>>();
            if types.contains(&"deb") && !types.contains(&"deb-src") {
                output.push_str("Types: ");
                output.push_str(&types.join(" "));
                output.push_str(" deb-src");
                if line.ends_with('\n') {
                    output.push('\n');
                }
                continue;
            }
        }
        output.push_str(line);
    }
    Ok(output)
}
pub fn run(output: &Path) -> Result<Value> {
    if !Path::new("/.dockerenv").is_file() || !nix::unistd::geteuid().is_root() {
        return Err(
            "APT collection requires a disposable root audit container, never the host".into(),
        );
    }
    let actual = platform::discover()?;
    if actual.os != "ubuntu" || actual.version != "24.04" {
        return Err("APT collection requires the Ubuntu 24.04 audit target".into());
    }
    let keyring = storage::safe_path(
        Path::new("/usr/share/keyrings"),
        Path::new("ubuntu-archive-keyring.gpg"),
    )?;
    if !keyring.is_file() {
        return Err("Ubuntu archive keyring absent".into());
    }
    let sources = Path::new("/etc/apt/sources.list.d");
    let mut edits = Vec::new();
    for entry in fs::read_dir(sources)? {
        let entry = entry?;
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "sources")
        {
            let path = storage::safe_path(sources, Path::new(&entry.file_name()))?;
            let text = fs::read_to_string(&path)?;
            edits.push((entry.file_name(), enable_sources(&text)?));
        }
    }
    if edits.is_empty() {
        return Err("Deb822 audit repository configuration is absent".into());
    }
    let lists = Path::new("/root/provenance-apt-lists");
    if lists.symlink_metadata().is_ok()
        || Path::new("/etc/apt/apt.conf.d/99provenance")
            .symlink_metadata()
            .is_ok()
    {
        return Err("APT audit state must be fresh".into());
    }
    fs::create_dir(output)?;
    fs::create_dir(lists)?;
    fs::create_dir(lists.join("partial"))?;
    fs::create_dir(lists.join("auxfiles"))?;
    storage::atomic_write(
        Path::new("/etc/apt/apt.conf.d"),
        Path::new("99provenance"),
        b"APT::Sandbox::User \"root\";\nDir::State::lists \"/root/provenance-apt-lists\";\n",
        0o644,
    )?;
    for (name, text) in edits {
        storage::atomic_write(sources, Path::new(&name), text.as_bytes(), 0o644)?;
    }
    let env = BTreeMap::from([
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("LC_ALL".into(), "C.UTF-8".into()),
        ("DEBIAN_FRONTEND".into(), "noninteractive".into()),
    ]);
    let base = output.canonicalize()?;
    let log = File::create(base.join("apt-update.log"))?;
    let mut process = OwnedProcess::spawn(
        Path::new("/usr/bin/apt-get"),
        &["update", "-o", "APT::Update::Error-Mode=any"],
        &env,
        &base,
        &log,
    )?;
    if !process.wait(Duration::from_secs(300))?.success() {
        return Err(
            "APT metadata update failed; do not treat partial indices as authenticated input"
                .into(),
        );
    }
    storage::atomic_write(
        &base,
        Path::new("archive-keyring.gpg"),
        &fs::read(keyring)?,
        0o644,
    )?;
    let mut entries = fs::read_dir(lists)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort();
    let mut indices = Vec::new();
    for name in entries {
        let name = name.to_str().ok_or("APT list name not UTF-8")?;
        if name.ends_with("_InRelease") {
            let path = storage::safe_path(lists, Path::new(name))?;
            storage::atomic_write(&base, Path::new(name), &fs::read(path)?, 0o644)?;
        } else if let Some(record) = index_identity(name)? {
            let path = storage::safe_path(lists, Path::new(name))?;
            let file = record["file"]
                .as_str()
                .ok_or("APT output filename absent")?;
            let stdout = File::create(base.join(file))?;
            let errors = File::create(base.join(format!("{file}.stderr")))?;
            let mut process = OwnedProcess::spawn_split(
                Path::new("/usr/lib/apt/apt-helper"),
                &["cat-file", path.to_str().ok_or("APT list path not UTF-8")?],
                &env,
                &base,
                &stdout,
                &errors,
            )?;
            if !process.wait(Duration::from_secs(60))?.success() {
                return Err("APT index decompression failed".into());
            }
            indices.push(record);
        }
    }
    if indices.is_empty() {
        return Err("APT collection produced no binary/source indices".into());
    }
    let date = base.join("collection-date.txt");
    let errors = File::create(base.join("collection-date.stderr"))?;
    let mut process = OwnedProcess::spawn_split(
        Path::new("/usr/bin/date"),
        &["--utc", "+%Y-%m-%dT%H:%M:%S+00:00"],
        &env,
        &base,
        &File::create(&date)?,
        &errors,
    )?;
    if !process.wait(Duration::from_secs(10))?.success() {
        return Err("APT collection timestamp unavailable".into());
    }
    let document =
        json!({"schema":1,"collected_at":fs::read_to_string(date)?.trim(),"indices":indices});
    let mut bytes = serde_json::to_vec_pretty(&document)?;
    bytes.push(b'\n');
    storage::atomic_write(&base, Path::new("inventory.json"), &bytes, 0o644)?;
    println!("Preserved {} indices and signed metadata; cryptographic authentication remains a separate check",indices.len());
    Ok(document)
}
