use crate::{deb822, process::OwnedProcess, storage, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::time::Duration;

fn package_name(name: &str) -> bool {
    name.bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"+.-".contains(&byte)
        })
}
pub fn source_identity(
    package: &str,
    version: &str,
    source: Option<&str>,
) -> Result<(String, String)> {
    if !package_name(package) || version.is_empty() {
        return Err("invalid Debian binary package identity".into());
    }
    let source = source.unwrap_or(package);
    let (name, version) = if let Some((name, version)) = source.split_once(" (") {
        let version = version
            .strip_suffix(')')
            .filter(|version| !version.is_empty() && !version.contains(['(', ')']))
            .ok_or("invalid Debian source version")?;
        (name, version)
    } else {
        (source, version)
    };
    if !package_name(name) {
        return Err("invalid Debian source package identity".into());
    }
    Ok((name.into(), version.into()))
}

pub fn payload(
    stream: impl Read,
    prime: &Path,
    output: &Path,
    package: &str,
) -> Result<(Vec<Value>, Vec<Value>, BTreeSet<String>)> {
    if !package_name(package) {
        return Err("unsafe native package identity".into());
    }
    let prime = prime.canonicalize()?;
    let mut archive = tar::Archive::new(stream);
    let mut staged = BTreeMap::new();
    let mut notices = Vec::new();
    let mut references = BTreeSet::new();
    let mut members = BTreeSet::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let name = entry.path()?.into_owned();
        let raw = name
            .to_str()
            .ok_or("Debian member name is not UTF-8")?
            .strip_prefix("./")
            .unwrap_or(name.to_str().ok_or("Debian member name is not UTF-8")?)
            .to_string();
        if matches!(raw.as_str(), "" | ".") && entry.header().entry_type().is_dir() {
            continue;
        }
        // dpkg data archives include directory names with trailing slashes;
        // normalize those only for directory entries, not regular members.
        let relative = Path::new(if entry.header().entry_type().is_dir() {
            raw.trim_end_matches('/')
        } else {
            &raw
        });
        storage::relative_path(relative)?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        if !members.insert(raw.clone()) {
            return Err("duplicate regular Debian payload member".into());
        }
        let target = prime.join(relative);
        let present = target.is_file() && !target.symlink_metadata()?.file_type().is_symlink();
        if present && !target.canonicalize()?.starts_with(&prime) {
            return Err("staged native file escapes prime".into());
        }
        let basename = relative
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Debian payload basename absent")?;
        let upper = basename.to_ascii_uppercase();
        let notice = raw.starts_with("usr/share/doc/")
            && (basename == "copyright"
                || ["LICENSE", "COPYING", "NOTICE"]
                    .iter()
                    .any(|prefix| upper.starts_with(prefix)));
        let (digest, bytes) = if notice {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            (storage::sha256(bytes.as_slice())?, Some(bytes))
        } else {
            (storage::sha256(&mut entry)?, None)
        };
        if present {
            let observed = storage::sha256(File::open(&target)?)?;
            staged.insert(raw.clone(),json!({"path":raw,"sha256":observed,"original_sha256":digest,"modified":observed!=digest}));
        }
        if let Some(bytes) = bytes {
            let destination = format!("notices/{package}/{raw}");
            let path = storage::safe_path(output, Path::new(&destination))?;
            fs::create_dir_all(path.parent().ok_or("notice output parent absent")?)?;
            storage::atomic_write(output, Path::new(&destination), &bytes, 0o644)?;
            notices.push(json!({"path":destination,"sha256":digest,"staged":present}));
            let text = String::from_utf8_lossy(&bytes);
            let prefix = "/usr/share/common-licenses/";
            for (offset, _) in text.match_indices(prefix) {
                let tail = &text[offset + prefix.len()..];
                let name = tail
                    .chars()
                    .take_while(|character| {
                        character.is_ascii_alphanumeric() || ".+-".contains(*character)
                    })
                    .collect::<String>();
                let name = name.trim_end_matches('.');
                if !name.is_empty() {
                    references.insert(name.to_string());
                }
            }
        }
    }
    // A package that contributes nothing to the prime is not staged in the
    // artifact. Its notices must not be audited as if they shipped, or a
    // base-image package reintroduces references the payload never had.
    if staged.is_empty() {
        return Err("Debian input has no observable regular files in prime".into());
    }
    if notices.is_empty() {
        return Err("missing authentic native notice evidence".into());
    }
    Ok((staged.into_values().collect(), notices, references))
}

/// A prime that actually came from this recipe is identifiable by our own
/// payload. Requiring these is what stops an unrelated collection of `.deb`
/// files from being audited as if it were the shipped artifact.
///
/// The mistake this exists to catch was real: an inventory was once built from
/// a single hand-placed `gzip` deb against a gzip-only prime, which produced a
/// licensing failure (`GFDL-3`) that never applied to the Snap at all, because
/// `gzip` is neither staged nor present in the real payload.
fn require_own_payload(prime: &Path) -> Result<()> {
    for relative in [
        "bin/among",
        "usr/share/doc/among/common-licenses-provider.txt",
    ] {
        let path = storage::safe_path(prime, Path::new(relative))?;
        if !path.is_file() || fs::metadata(&path)?.len() == 0 {
            return Err(format!(
                "prime does not contain this project's own payload at {relative}; \
                 a prime built from unrelated packages cannot be audited as the artifact"
            )
            .into());
        }
    }
    Ok(())
}

pub fn inventory(prime: &Path, debs: &Path, output: &Path, scratch: &Path) -> Result<Value> {
    if !prime.is_dir() || !debs.is_dir() {
        return Err("prime and exact stage-package directories required".into());
    }
    require_own_payload(prime)?;
    let mut inputs = Vec::new();
    for entry in fs::read_dir(debs)? {
        let entry = entry?;
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "deb")
        {
            let path = storage::safe_path(debs, Path::new(&entry.file_name()))?;
            if !path.is_file() {
                return Err("staged Debian artifact must be a regular file".into());
            }
            inputs.push(path);
        }
    }
    inputs.sort();
    if inputs.is_empty() {
        return Err(
            "no exact staged Debian artifacts; host package metadata is not a substitute".into(),
        );
    }
    if output.symlink_metadata().is_ok() {
        return Err("native inventory output must be new".into());
    }
    fs::create_dir(output)?;
    // A refused inventory leaves nothing behind: extracted notices without an
    // inventory.json would look like an audit that was started and kept.
    let result = write_inventory(&inputs, prime, output, scratch);
    if result.is_err() {
        // The refusal is what the caller needs; a failed cleanup of the
        // directory this call just created cannot change it.
        let _ = fs::remove_dir_all(output);
    }
    result
}

fn write_inventory(
    inputs: &[std::path::PathBuf],
    prime: &Path,
    output: &Path,
    scratch: &Path,
) -> Result<Value> {
    let base = storage::private_directory(scratch, "among-native-inventory")?;
    let env = BTreeMap::from([
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("LC_ALL".into(), "C.UTF-8".into()),
    ]);
    let mut records = Vec::new();
    let mut packages = BTreeSet::new();
    for (index, deb) in inputs.iter().enumerate() {
        let metadata = base.join(format!("metadata-{index}"));
        let errors = File::create(base.join(format!("metadata-{index}.stderr")))?;
        let mut process = OwnedProcess::spawn_split(
            Path::new("/usr/bin/dpkg-deb"),
            &[
                "--field",
                deb.to_str().ok_or("Debian input path is not UTF-8")?,
                "Package",
                "Version",
                "Architecture",
                "Source",
            ],
            &env,
            &base,
            &File::create(&metadata)?,
            &errors,
        )?;
        if !process.wait(Duration::from_secs(60))?.success() {
            return Err("cannot inspect exact Debian input metadata".into());
        }
        let fields = deb822::fields(&fs::read_to_string(metadata)?)?;
        let package = deb822::required(&fields, "Package")?;
        let version = deb822::required(&fields, "Version")?;
        let (source, source_version) =
            source_identity(package, version, fields.get("source").map(String::as_str))?;
        if !packages.insert(package.to_string()) {
            return Err("duplicate binary package inputs".into());
        }
        let data = base.join(format!("payload-{index}.tar"));
        let errors = File::create(base.join(format!("payload-{index}.stderr")))?;
        let mut process = OwnedProcess::spawn_split(
            Path::new("/usr/bin/dpkg-deb"),
            &[
                "--fsys-tarfile",
                deb.to_str().ok_or("Debian input path is not UTF-8")?,
            ],
            &env,
            &base,
            &File::create(&data)?,
            &errors,
        )?;
        if !process.wait(Duration::from_secs(60))?.success() {
            return Err("cannot inspect exact Debian payload archive".into());
        }
        let (staged, notices, references) = payload(File::open(data)?, prime, output, package)?;
        records.push(json!({"package":package,"version":version,"architecture":deb822::required(&fields,"Architecture")?,"source_package":source,"source_version":source_version,"input":{"filename":deb.file_name().and_then(|name|name.to_str()).ok_or("Debian filename is not UTF-8")?,"sha256":storage::sha256(File::open(deb)?)?},"staged_files":staged,"notices":notices,"common_license_references":references}));
    }
    let document = json!({"schema":1,"scope":"exact Debian stage inputs and their observed prime files","source_delivery":"not established; matching source archives/patches/build instructions still required","packages":records});
    storage::atomic_write(
        output,
        Path::new("inventory.json"),
        &serde_json::to_vec_pretty(&document)?,
        0o644,
    )?;
    println!("Inventoried {} actual Debian inputs; source delivery/runtime compliance remains unverified",packages.len());
    Ok(document)
}
