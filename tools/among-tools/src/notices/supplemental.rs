use crate::{storage, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const INPUT: &str = "THIRD_PARTY/inputs";
const LIMIT: u64 = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    schema: u32,
    packages: Vec<Receipt>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Receipt {
    name: String,
    version: String,
    source: String,
    license: String,
    repository: String,
    upstream: String,
    archive_sha256: String,
    vcs_commit: String,
    vcs_path: String,
    vcs_sha256: String,
    package_manifest: String,
    files: Vec<Evidence>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    path: String,
    upstream_path: String,
    sha256: String,
    kind: Kind,
}

#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Kind {
    License,
    Source,
}

pub(super) struct Inputs {
    root: PathBuf,
    receipts: BTreeMap<(String, String), Receipt>,
    checksums: BTreeMap<(String, String, String), String>,
}

fn read(root: &Path, relative: &str) -> Result<Vec<u8>> {
    let path = storage::safe_path(root, Path::new(relative))?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > LIMIT {
        return Err("supplemental evidence must be a bounded regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() as u64 > LIMIT {
        return Err("empty or oversized supplemental evidence".into());
    }
    Ok(bytes)
}

fn digest(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl Inputs {
    pub(super) fn load(root: &Path) -> Result<Self> {
        let input = storage::safe_path(root, Path::new(INPUT))?;
        let mut result = Self {
            root: input,
            receipts: BTreeMap::new(),
            checksums: BTreeMap::new(),
        };
        if !result.root.join("index.json").try_exists()? {
            return Ok(result);
        }
        let index: Index =
            serde_json::from_value(storage::parse_json(&read(&result.root, "index.json")?)?)?;
        if index.schema != 1 || index.packages.is_empty() || index.packages.len() > 64 {
            return Err("unsupported supplemental notice index".into());
        }
        for manifest in super::MANIFESTS {
            let lock = Path::new(manifest).with_file_name("Cargo.lock");
            let lock: toml::Value = toml::from_str(&String::from_utf8(read(
                root,
                lock.to_str().ok_or("non-UTF-8 lock path")?,
            )?)?)?;
            for package in lock
                .get("package")
                .and_then(toml::Value::as_array)
                .ok_or("lock packages absent")?
            {
                let Some(source) = package.get("source").and_then(toml::Value::as_str) else {
                    continue;
                };
                let string = |key: &str| -> Result<String> {
                    Ok(package
                        .get(key)
                        .and_then(toml::Value::as_str)
                        .ok_or("locked package identity/checksum absent")?
                        .into())
                };
                let key = (string("name")?, string("version")?, source.into());
                // git packages have no registry archive checksum; they cannot use this input.
                if let Some(checksum) = package.get("checksum").and_then(toml::Value::as_str) {
                    if result
                        .checksums
                        .insert(key, checksum.into())
                        .is_some_and(|old| old != checksum)
                    {
                        return Err("conflicting locked archive checksums".into());
                    }
                }
            }
        }
        for receipt in index.packages {
            let key = (receipt.name.clone(), receipt.version.clone());
            if result.receipts.insert(key, receipt).is_some() {
                return Err("duplicate supplemental package evidence".into());
            }
        }
        Ok(result)
    }

    pub(super) fn apply(
        &mut self,
        package: &Value,
        source: &Path,
        outputs: &mut BTreeMap<String, Vec<u8>>,
        texts: &mut Vec<Value>,
    ) -> Result<Option<Value>> {
        let name = package["name"].as_str().ok_or("package name absent")?;
        let version = package["version"]
            .as_str()
            .ok_or("package version absent")?;
        let Some(receipt) = self.receipts.remove(&(name.into(), version.into())) else {
            return Ok(None);
        };
        if package["source"] != receipt.source
            || package["license"] != receipt.license
            || package["repository"] != receipt.repository
            || receipt.source != "registry+https://github.com/rust-lang/crates.io-index"
            || self
                .checksums
                .get(&(name.into(), version.into(), receipt.source.clone()))
                != Some(&receipt.archive_sha256)
            || !digest(&receipt.archive_sha256, 64)
            || !digest(&receipt.vcs_sha256, 64)
            || !digest(&receipt.vcs_commit, 40)
            || receipt.repository.trim_end_matches(".git") != receipt.upstream
        {
            return Err(format!("supplemental package/lock identity mismatch: {name}").into());
        }
        let origin = receipt
            .upstream
            .strip_prefix("https://github.com/")
            .ok_or("unsupported supplemental upstream origin")?;
        storage::relative_path(Path::new(origin))?;
        if origin.split('/').count() != 2
            || !origin
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
        {
            return Err("unsafe supplemental upstream origin".into());
        }
        if !receipt.vcs_path.is_empty() {
            storage::relative_path(Path::new(&receipt.vcs_path))?;
        }
        let vcs_bytes = read(source, ".cargo_vcs_info.json")?;
        let vcs = storage::parse_json(&vcs_bytes)?;
        if storage::sha256(vcs_bytes.as_slice())? != receipt.vcs_sha256
            || vcs["git"]["sha1"] != receipt.vcs_commit
            || vcs["path_in_vcs"] != receipt.vcs_path
        {
            return Err("published crate VCS evidence mismatch".into());
        }
        // registry extractions and cargo-vendor use different receipts. Neither may
        // substitute a version/SPDX declaration for the exact locked archive identity.
        if source.join(".cargo-checksum.json").try_exists()? {
            let checksum = storage::parse_json(&read(source, ".cargo-checksum.json")?)?;
            if checksum["package"] != receipt.archive_sha256 {
                return Err("vendored archive checksum mismatch".into());
            }
        } else {
            let namespace = source.parent().ok_or("registry namespace absent")?;
            let src = namespace.parent().ok_or("registry src absent")?;
            if src.file_name() != Some(std::ffi::OsStr::new("src")) {
                return Err("published crate archive evidence unavailable".into());
            }
            let registry = src.parent().ok_or("registry root absent")?;
            let archive = Path::new("cache")
                .join(namespace.file_name().ok_or("registry namespace absent")?)
                .join(format!("{name}-{version}.crate"));
            let path = storage::safe_path(registry, &archive)?;
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
                .open(path)?;
            if !file.metadata()?.is_file() || storage::sha256(file)? != receipt.archive_sha256 {
                return Err("published crate archive checksum mismatch".into());
            }
        }
        if receipt.files.is_empty() || receipt.files.len() > 16 {
            return Err("supplemental source/license evidence absent or excessive".into());
        }
        let mut evidence = Vec::new();
        let mut paths = BTreeSet::new();
        let mut manifest_bound = false;
        let mut licensed = false;
        for file in receipt.files {
            storage::relative_path(Path::new(&file.upstream_path))?;
            if !paths.insert(file.upstream_path.clone()) || !digest(&file.sha256, 64) {
                return Err("duplicate or invalid supplemental evidence identity".into());
            }
            let bytes = read(&self.root, &file.path)?;
            if storage::sha256(bytes.as_slice())? != file.sha256 {
                return Err("supplemental evidence SHA-256 mismatch".into());
            }
            if file.path == receipt.package_manifest {
                let expected = if receipt.vcs_path.is_empty() {
                    "Cargo.toml".into()
                } else {
                    format!("{}/Cargo.toml", receipt.vcs_path)
                };
                if file.kind != Kind::Source
                    || file.upstream_path != expected
                    || read(source, "Cargo.toml.orig")? != bytes
                {
                    return Err("published/upstream package manifest mismatch".into());
                }
                manifest_bound = true;
            }
            let output = if file.kind == Kind::License {
                licensed = true;
                format!(
                    "texts/{name}-{version}/{}",
                    Path::new(&file.upstream_path)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .ok_or("license filename absent")?
                )
            } else {
                format!("provenance/{name}-{version}/{}", file.upstream_path)
            };
            if outputs.get(&output).is_some_and(|old| old != &bytes) {
                return Err("conflicting supplemental output evidence".into());
            }
            outputs.insert(output.clone(), bytes);
            if file.kind == Kind::License {
                texts.push(json!({"path":output,"sha256":file.sha256}));
            }
            evidence.push(json!({"path":output,"sha256":file.sha256,"url":format!("{}/blob/{}/{}",receipt.upstream,receipt.vcs_commit,file.upstream_path)}));
        }
        if !manifest_bound || !licensed {
            return Err(
                "supplemental evidence requires the exact package source and license grant".into(),
            );
        }
        Ok(Some(
            json!({"archive_sha256":receipt.archive_sha256,"repository":receipt.repository,"vcs_commit":receipt.vcs_commit,"vcs_path":receipt.vcs_path,"vcs_sha256":receipt.vcs_sha256,"evidence":evidence}),
        ))
    }

    pub(super) fn finish(self) -> Result<()> {
        if !self.receipts.is_empty() {
            return Err("unused or mismatched supplemental package evidence".into());
        }
        Ok(())
    }
}
