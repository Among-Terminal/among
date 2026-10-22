//! catalogue entries embed the shared API manifest. artifact digests bind exact
//! package bytes; they do not authenticate the publisher independently.

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;

use serde::Deserialize;
use serde_json::Value;

use crate::plugin::data::{self, Problem};
use crate::plugin::manifest::{self, Manifest};

pub const MAX_CATALOGUE_BYTES: usize = 1024 * 1024;
pub const MAX_PLUGINS: usize = 32;
pub const MAX_VERSIONS: usize = 16;
pub const MAX_ARTIFACTS: usize = 8;
pub const MAX_PACKAGE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_EXPANDED_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_MEMBERS: usize = 2048;

#[derive(Clone, Debug, PartialEq)]
pub struct Catalogue {
    pub plugins: Vec<Plugin>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plugin {
    pub id: String,
    pub versions: Vec<Version>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Version {
    pub revision: String,
    pub manifest_sha256: String,
    pub release_id: u64,
    pub manifest: Manifest,
    pub artifacts: Vec<Artifact>,
}

#[derive(Clone, Debug)]
pub struct Platform {
    pub architecture: Architecture,
    pub libc: Option<(Libc, String)>,
}

impl Platform {
    pub fn current() -> Result<Self, Problem> {
        let architecture = match std::env::consts::ARCH {
            "x86_64" => Architecture::Amd64,
            "aarch64" => Architecture::Arm64,
            _ => return Err(Problem("no supported ready-built host architecture")),
        };
        #[cfg(target_env = "gnu")]
        let libc = {
            let version = unsafe { std::ffi::CStr::from_ptr(libc::gnu_get_libc_version()) }
                .to_str()
                .map_err(|_| Problem("cannot establish host libc version"))?;
            Some((Libc::Glibc, version.to_string()))
        };
        // do not invent a musl version from the compiler target or a filename.
        #[cfg(not(target_env = "gnu"))]
        let libc = None;
        Ok(Self { architecture, libc })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Architecture {
    Amd64,
    Arm64,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Libc {
    Glibc,
    Musl,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Runtime {
    Native {
        libc: Libc,
        minimum: String,
    },
    Script {
        executable: String,
        minimum: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    pub executable: String,
    pub minimum: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub executable: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub os: String,
    pub architecture: Architecture,
    pub runtime: Runtime,
    pub dependencies: Vec<Dependency>,
    pub format: String,
    pub asset_id: u64,
    pub size: u64,
    pub sha256: String,
    pub members: Vec<Member>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCatalogue {
    schema_version: u32,
    plugins: Vec<RawPlugin>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPlugin {
    id: String,
    versions: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawVersion {
    revision: String,
    manifest_sha256: String,
    release_id: u64,
    manifest: Value,
    artifacts: Vec<Artifact>,
}

impl Catalogue {
    pub fn parse(bytes: &[u8]) -> Result<Self, Problem> {
        let mut value = data::parse_unique(bytes, MAX_CATALOGUE_BYTES)?;
        let plugins = if value.get("plugins").is_some() {
            let raw: RawCatalogue =
                serde_json::from_value(value).map_err(|_| Problem("invalid catalogue fields"))?;
            if raw.schema_version != 1 {
                return Err(Problem("unsupported catalogue schema version"));
            }
            raw.plugins
        } else {
            let object = value
                .as_object_mut()
                .ok_or(Problem("invalid catalogue root"))?;
            if object.remove("schema_version") != Some(Value::from(1)) {
                return Err(Problem("unsupported catalogue schema version"));
            }
            let version = Version::parse(value)?;
            return Ok(Self {
                plugins: vec![Plugin {
                    id: version.manifest.id.clone(),
                    versions: vec![version],
                }],
            });
        };
        if plugins.is_empty() || plugins.len() > MAX_PLUGINS {
            return Err(Problem("catalogue plugin count exceeds its bounds"));
        }
        let mut ids = BTreeSet::new();
        let mut accepted = Vec::new();
        for plugin in plugins {
            if !manifest::local_id(&plugin.id) || !ids.insert(plugin.id.clone()) {
                return Err(Problem("invalid or duplicate catalogue plugin identity"));
            }
            if plugin.versions.is_empty() || plugin.versions.len() > MAX_VERSIONS {
                return Err(Problem("catalogue version count exceeds its bounds"));
            }
            let mut versions = Vec::new();
            let mut labels = BTreeSet::new();
            for raw in plugin.versions {
                let version = Version::parse(raw)?;
                if version.manifest.id != plugin.id
                    || !labels.insert(version.manifest.version.clone())
                {
                    return Err(Problem(
                        "catalogue version identity is inconsistent or duplicated",
                    ));
                }
                versions.push(version);
            }
            accepted.push(Plugin {
                id: plugin.id,
                versions,
            });
        }
        Ok(Self { plugins: accepted })
    }
}

impl Version {
    pub fn select(&self, platform: &Platform) -> Result<&Artifact, Problem> {
        let mut compatible = self
            .artifacts
            .iter()
            .filter(|artifact| artifact.compatibility(platform).is_ok());
        let selected = compatible.next().ok_or(Problem(
            "no declared artifact matches the host platform and dependencies",
        ))?;
        if compatible.next().is_some() {
            return Err(Problem(
                "multiple artifacts match; the selection is ambiguous",
            ));
        }
        Ok(selected)
    }

    fn parse(value: Value) -> Result<Self, Problem> {
        let raw: RawVersion = serde_json::from_value(value)
            .map_err(|_| Problem("invalid version fields or unsupported installation behaviour"))?;
        if !hex(&raw.revision, 40) || !hex(&raw.manifest_sha256, 64) || raw.release_id == 0 {
            return Err(Problem(
                "version needs exact revision, manifest digest and release identity",
            ));
        }
        let bytes =
            serde_json::to_vec(&raw.manifest).map_err(|_| Problem("invalid shared manifest"))?;
        let manifest = Manifest::parse(&bytes)?;
        if raw.artifacts.is_empty() || raw.artifacts.len() > MAX_ARTIFACTS {
            return Err(Problem("artifact count exceeds its bounds"));
        }
        let mut assets = BTreeSet::new();
        for artifact in &raw.artifacts {
            if !assets.insert(artifact.asset_id) {
                return Err(Problem("duplicate release asset identity"));
            }
            artifact.validate(&manifest, &raw.manifest_sha256)?;
        }
        Ok(Self {
            revision: raw.revision,
            manifest_sha256: raw.manifest_sha256,
            release_id: raw.release_id,
            manifest,
            artifacts: raw.artifacts,
        })
    }
}

impl Artifact {
    pub fn compatibility(&self, platform: &Platform) -> Result<(), Problem> {
        if self.os != "linux" || self.architecture != platform.architecture {
            return Err(Problem("artifact OS or architecture does not match"));
        }
        match &self.runtime {
            Runtime::Native { libc, minimum } => {
                let (host_libc, version) = platform
                    .libc
                    .as_ref()
                    .ok_or(Problem("host libc version is unavailable"))?;
                if host_libc != libc || !version_at_least(version, minimum) {
                    return Err(Problem("artifact libc or minimum version does not match"));
                }
            }
            Runtime::Script {
                executable,
                minimum,
            } => {
                if executable == "/usr/bin/env" {
                    return Err(Problem("env-based script runtimes are not modeled"));
                }
                installed_executable(executable, minimum.as_deref())?;
            }
        }
        for dependency in &self.dependencies {
            installed_executable(&dependency.executable, dependency.minimum.as_deref())?;
        }
        Ok(())
    }

    fn validate(&self, manifest: &Manifest, manifest_sha256: &str) -> Result<(), Problem> {
        if self.os != "linux"
            || self.format != "tar.gz"
            || self.asset_id == 0
            || self.size == 0
            || self.size > MAX_PACKAGE_BYTES
            || !hex(&self.sha256, 64)
        {
            return Err(Problem("invalid or unsupported ready-built artifact"));
        }
        match &self.runtime {
            Runtime::Native { minimum, .. } if !numeric_version(minimum) => {
                return Err(Problem("invalid libc minimum"))
            }
            Runtime::Script {
                executable,
                minimum,
            } if !system_executable(executable)
                || minimum
                    .as_ref()
                    .is_some_and(|minimum| !numeric_version(minimum)) =>
            {
                return Err(Problem("invalid declared script runtime"))
            }
            _ => {}
        }
        if self.dependencies.len() > 16 {
            return Err(Problem("too many external dependencies"));
        }
        let mut dependencies = BTreeSet::new();
        for dependency in &self.dependencies {
            if !system_executable(&dependency.executable)
                || !dependencies.insert(&dependency.executable)
                || dependency
                    .minimum
                    .as_ref()
                    .is_some_and(|minimum| !numeric_version(minimum))
            {
                return Err(Problem("invalid or duplicate external dependency"));
            }
        }
        if self.members.is_empty() || self.members.len() > MAX_MEMBERS {
            return Err(Problem("package member count exceeds its bounds"));
        }
        let mut paths = BTreeSet::new();
        let mut expanded = 0u64;
        for member in &self.members {
            if !data::relative_path(&member.path)
                || member
                    .path
                    .split('/')
                    .any(|component| component.starts_with(".among-"))
                || !hex(&member.sha256, 64)
                || !paths.insert(member.path.as_str())
                || member.size > MAX_PACKAGE_BYTES
            {
                return Err(Problem("invalid or duplicate package member"));
            }
            expanded = expanded
                .checked_add(member.size)
                .filter(|size| *size <= MAX_EXPANDED_BYTES)
                .ok_or(Problem("expanded package bytes exceed their bounds"))?;
        }
        for path in &paths {
            let mut prefix = path.split('/').collect::<Vec<_>>();
            prefix.pop();
            while !prefix.is_empty() {
                if paths.contains(prefix.join("/").as_str()) {
                    return Err(Problem("package file conflicts with a parent directory"));
                }
                prefix.pop();
            }
        }
        let packaged_manifest = self
            .members
            .iter()
            .find(|member| member.path == "among-plugin.json")
            .ok_or(Problem("package lacks the shared manifest"))?;
        if packaged_manifest.sha256 != manifest_sha256
            || packaged_manifest.executable
            || packaged_manifest.size > manifest::MAX_MANIFEST_BYTES as u64
        {
            return Err(Problem(
                "packaged manifest disagrees with selected metadata",
            ));
        }
        if !self.members.iter().any(|member| {
            member.path == manifest.entrypoint && member.executable && member.size > 0
        }) {
            return Err(Problem("package lacks the declared executable entrypoint"));
        }
        Ok(())
    }
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn numeric_version(value: &str) -> bool {
    let parts = value.split('.').collect::<Vec<_>>();
    (1..=3).contains(&parts.len())
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && part.parse::<u16>().is_ok()
        })
}

fn version_at_least(actual: &str, minimum: &str) -> bool {
    let parse = |value: &str| -> Option<[u16; 3]> {
        if !numeric_version(value) {
            return None;
        }
        let mut version = [0; 3];
        for (index, part) in value.split('.').enumerate() {
            version[index] = part.parse().ok()?;
        }
        Some(version)
    };
    match (parse(actual), parse(minimum)) {
        (Some(actual), Some(minimum)) => actual >= minimum,
        _ => false,
    }
}

fn installed_executable(path: &str, minimum: Option<&str>) -> Result<(), Problem> {
    if !system_executable(path)
        || !std::fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    {
        return Err(Problem("declared system executable is not installed"));
    }
    // a filesystem entry proves availability, not a version. do not run probes
    // or claim an unmeasured minimum is satisfied during inert inspection.
    if minimum.is_some() {
        return Err(Problem(
            "the declared executable minimum version cannot be established without a probe",
        ));
    }
    Ok(())
}

fn system_executable(value: &str) -> bool {
    value.strip_prefix("/usr/bin/").is_some_and(|name| {
        !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            && !matches!(name, "." | "..")
    })
}

#[cfg(test)]
pub(crate) mod tests;
