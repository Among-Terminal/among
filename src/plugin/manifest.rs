//! managed identity comes from the installation record, never a manifest's
//! display name or a running executable's claim. inspection executes no code.

use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::data::{self, bounded_text, relative_path, Problem};
use super::schema::SettingsSchema;

pub const MAX_MANIFEST_BYTES: usize = data::MAX_STATIC_BYTES;
const MAX_FEATURES: usize = 32;
const MAX_CHANNELS: usize = 32;
const MAX_ICON_BYTES: usize = 1024 * 1024;
const MAX_ICON_DIMENSION: u32 = 2048;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ManagedIdentity {
    namespace: Namespace,
    plugin: String,
}

/// Where an identity's data, settings and generations live. A development
/// install from a local catalogue has no repository, so it cannot share a
/// namespace with any repository plugin of the same local ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Namespace {
    Repository(u64),
    Development,
}

/// Generation directory and key prefix of local development installs. It is
/// not a decimal number, so it never collides with a repository directory.
pub const DEVELOPMENT_NAMESPACE: &str = "development";

impl ManagedIdentity {
    pub fn new(repository: u64, plugin: String) -> Result<Self, Problem> {
        if repository == 0 || !local_id(&plugin) {
            return Err(Problem("invalid authoritative plugin identity"));
        }
        Ok(Self {
            namespace: Namespace::Repository(repository),
            plugin,
        })
    }

    pub fn development(plugin: String) -> Result<Self, Problem> {
        if !local_id(&plugin) {
            return Err(Problem("invalid development plugin identity"));
        }
        Ok(Self {
            namespace: Namespace::Development,
            plugin,
        })
    }

    pub fn local_id(&self) -> &str {
        &self.plugin
    }
    pub fn key(&self) -> String {
        // '/' cannot occur in a legacy executable filename, so these keys never
        // inherit a legacy switch by a coincidental display-name match.
        match self.namespace {
            Namespace::Repository(id) => format!("github:{id}/{}", self.plugin),
            Namespace::Development => format!("{DEVELOPMENT_NAMESPACE}/{}", self.plugin),
        }
    }
}

pub fn local_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Api {
    pub major: u32,
    pub minor: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryChannel {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Icon {
    Theme(String),
    File(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub version: String,
    pub name: String,
    pub description: String,
    pub entrypoint: String,
    pub icon: Option<Icon>,
    pub icon_problem: Option<Problem>,
    pub api: Api,
    pub required_features: Vec<String>,
    pub optional_features: Vec<String>,
    pub settings: Option<SettingsSchema>,
    pub delivery_channels: Vec<DeliveryChannel>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    schema_version: u32,
    id: String,
    version: String,
    name: String,
    description: String,
    entrypoint: String,
    icon: Option<Value>,
    api: Api,
    #[serde(default)]
    required_features: Vec<String>,
    #[serde(default)]
    optional_features: Vec<String>,
    settings: Option<Value>,
    #[serde(default)]
    delivery_channels: Vec<DeliveryChannel>,
}

impl Manifest {
    pub fn parse(bytes: &[u8]) -> Result<Self, Problem> {
        let raw: RawManifest =
            serde_json::from_value(data::parse_unique(bytes, MAX_MANIFEST_BYTES)?)
                .map_err(|_| Problem("invalid manifest fields or unsupported schema vocabulary"))?;
        if raw.schema_version != 1 {
            return Err(Problem("unsupported manifest schema version"));
        }
        if !local_id(&raw.id)
            || !metadata_text(&raw.version, 128, false)
            || !metadata_text(&raw.name, 128, false)
            || !bounded_text(&raw.description, 2048, true)
            || !relative_path(&raw.entrypoint)
        {
            return Err(Problem("invalid manifest identity, text or entrypoint"));
        }
        if raw.required_features.len() + raw.optional_features.len() > MAX_FEATURES {
            return Err(Problem("too many manifest features"));
        }
        let mut features = BTreeSet::new();
        for feature in raw.required_features.iter().chain(&raw.optional_features) {
            if !feature_name(feature) || !features.insert(feature) {
                return Err(Problem("invalid or duplicate manifest feature"));
            }
        }
        if raw.delivery_channels.len() > MAX_CHANNELS {
            return Err(Problem("too many provider channels"));
        }
        let mut channels = BTreeSet::new();
        for channel in &raw.delivery_channels {
            if !local_id(&channel.id)
                || !channels.insert(&channel.id)
                || !metadata_text(&channel.name, 128, false)
                || !bounded_text(&channel.description, 1024, true)
            {
                return Err(Problem("invalid or duplicate provider channel"));
            }
        }
        let mut icon_problem = None;
        let icon = match raw.icon.as_ref().and_then(Value::as_str) {
            Some(name) if name.starts_with("theme:") => {
                let theme = &name[6..];
                let valid = !theme.is_empty()
                    && theme.len() <= 128
                    && theme
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
                if !valid {
                    icon_problem = Some(Problem("invalid optional theme icon"));
                }
                valid.then(|| Icon::Theme(theme.into()))
            }
            Some(path) if path.is_empty() || path.len() > 256 => {
                icon_problem = Some(Problem("invalid optional icon metadata"));
                None
            }
            Some(path) => {
                if !relative_path(path) {
                    return Err(Problem("unsafe package icon path"));
                }
                Some(Icon::File(path.into()))
            }
            None => {
                if raw.icon.is_some() {
                    icon_problem = Some(Problem("invalid optional icon metadata"));
                }
                None
            }
        };
        let settings = raw.settings.map(SettingsSchema::parse).transpose()?;
        Ok(Self {
            id: raw.id,
            version: raw.version,
            name: raw.name,
            description: raw.description,
            entrypoint: raw.entrypoint,
            icon,
            icon_problem,
            api: raw.api,
            required_features: raw.required_features,
            optional_features: raw.optional_features,
            settings,
            delivery_channels: raw.delivery_channels,
        })
    }

    pub fn compatibility(&self, major: u32, minor: u32, features: &[&str]) -> Result<(), Problem> {
        if self.api.major != major || self.api.minor > minor || major != 1 || minor < 2 {
            return Err(Problem(
                "managed plugins require compatible host API 1.2 or newer",
            ));
        }
        if self
            .required_features
            .iter()
            .any(|feature| !features.contains(&feature.as_str()))
        {
            return Err(Problem("a required plugin feature is unavailable"));
        }
        if self
            .settings
            .as_ref()
            .is_some_and(|schema| !schema.supported())
        {
            return Err(Problem("a required settings feature is unsupported"));
        }
        Ok(())
    }
}

fn feature_name(feature: &str) -> bool {
    !feature.is_empty()
        && feature.len() <= 64
        && feature.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
}

fn metadata_text(value: &str, limit: usize, multiline: bool) -> bool {
    !value.trim().is_empty() && bounded_text(value, limit, multiline)
}

#[derive(Clone, Debug)]
pub struct Inspected {
    pub manifest: Manifest,
    pub executable: PathBuf,
    pub raster_icon: Option<Vec<u8>>,
    pub icon_problem: Option<Problem>,
}

pub fn inspect(
    directory: &std::fs::File,
    root: &Path,
    identity: ManagedIdentity,
) -> Result<Inspected, Problem> {
    let bytes = data::read_bounded(
        &data::contained_file(directory, "among-plugin.json")?,
        MAX_MANIFEST_BYTES,
    )?;
    let manifest = Manifest::parse(&bytes)?;
    if manifest.id != identity.local_id() {
        return Err(Problem(
            "manifest identity disagrees with its installation record",
        ));
    }
    let executable = data::contained_file(directory, &manifest.entrypoint)?;
    if executable
        .metadata()
        .map_err(|_| Problem("cannot inspect plugin entrypoint"))?
        .mode()
        & 0o111
        == 0
    {
        return Err(Problem("plugin entrypoint is not executable"));
    }
    let (raster_icon, icon_problem) = if let Some(Icon::File(path)) = &manifest.icon {
        match data::contained_file(directory, path)
            .and_then(|file| data::read_bounded(&file, MAX_ICON_BYTES))
            .and_then(validate_png)
        {
            Ok(bytes) => (Some(bytes), None),
            Err(problem) => (None, Some(problem)),
        }
    } else {
        (None, manifest.icon_problem)
    };
    Ok(Inspected {
        executable: root.join(&manifest.entrypoint),
        manifest,
        raster_icon,
        icon_problem,
    })
}

fn validate_png(bytes: Vec<u8>) -> Result<Vec<u8>, Problem> {
    use gdk_pixbuf::prelude::*;
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return Err(Problem("optional artwork is not a supported PNG icon"));
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    if width == 0 || height == 0 || width > MAX_ICON_DIMENSION || height > MAX_ICON_DIMENSION {
        return Err(Problem("optional artwork exceeds 2048 by 2048 pixels"));
    }
    // pixel limits alone do not bound compressed text/ICC profiles or animation
    // frames. permit only the static PNG chunks needed by this icon contract.
    let mut offset = 8usize;
    let mut chunks = 0;
    let mut ended = false;
    while offset < bytes.len() {
        if offset + 12 > bytes.len() || chunks >= 4096 {
            return Err(Problem("invalid or excessive PNG chunks"));
        }
        let length = u32::from_be_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]) as usize;
        let next = offset
            .checked_add(12)
            .and_then(|offset| offset.checked_add(length))
            .filter(|next| *next <= bytes.len())
            .ok_or(Problem("truncated PNG icon"))?;
        let kind = &bytes[offset + 4..offset + 8];
        let permitted = match kind {
            b"IHDR" => offset == 8 && length == 13,
            b"PLTE" => length <= 768,
            b"tRNS" => length <= 256,
            b"IDAT" => true,
            b"gAMA" | b"sBIT" => length <= 4,
            b"cHRM" => length == 32,
            b"sRGB" => length == 1,
            b"pHYs" => length == 9,
            b"bKGD" => length <= 6,
            b"IEND" => {
                ended = true;
                length == 0 && next == bytes.len()
            }
            _ => false,
        };
        if !permitted {
            return Err(Problem(
                "optional icon contains unsupported PNG metadata or animation",
            ));
        }
        offset = next;
        chunks += 1;
    }
    if !ended {
        return Err(Problem("incomplete PNG icon"));
    }
    let loader = gdk_pixbuf::PixbufLoader::with_type("png")
        .map_err(|_| Problem("PNG icon decoder is unavailable"))?;
    let written = loader.write(&bytes);
    let closed = loader.close();
    if written.is_err() || closed.is_err() {
        return Err(Problem("optional PNG icon cannot be decoded"));
    }
    if !loader
        .pixbuf()
        .is_some_and(|image| image.width() == width as i32 && image.height() == height as i32)
    {
        return Err(Problem("optional PNG icon has inconsistent dimensions"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn inspect(root: &Path, identity: ManagedIdentity) -> Result<Inspected, Problem> {
        super::inspect(&data::owned_directory(root)?, root, identity)
    }

    fn sample() -> Value {
        json!({"schema_version":1,"id":"sample","version":"2026.10","name":"Sample","description":"Static data","entrypoint":"bin/plugin","api":{"major":1,"minor":2},"required_features":["status"],"optional_features":["future.optional"]})
    }

    fn root() -> PathBuf {
        let base = std::env::var_os("AMONG_CHECK_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("opencode"));
        let path = base.join(format!(
            "manifest-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(path.join("bin")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(path.join("bin"), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(path.join("among-plugin.json"), sample().to_string()).unwrap();
        std::fs::set_permissions(
            path.join("among-plugin.json"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        std::fs::write(
            path.join("bin/plugin"),
            format!(
                "#!/bin/sh\nprintf unexpected > '{}'\nexit 99\n",
                path.join("executed").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            path.join("bin/plugin"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        path
    }

    #[test]
    fn versions_features_channels_and_origins_have_independent_authority() {
        let parsed = Manifest::parse(sample().to_string().as_bytes()).unwrap();
        assert!(parsed.compatibility(1, 2, &["status"]).is_ok());
        assert!(parsed.compatibility(1, 1, &["status"]).is_err());
        assert!(parsed.compatibility(2, 2, &["status"]).is_err());
        assert!(parsed.compatibility(1, 2, &[]).is_err());
        assert_ne!(
            ManagedIdentity::new(1, "sample".into()).unwrap().key(),
            ManagedIdentity::new(2, "sample".into()).unwrap().key()
        );
        assert!(ManagedIdentity::new(0, "sample".into()).is_err());
        assert!(ManagedIdentity::new(1, "../sample".into()).is_err());
        // A local development install never shares data, settings or switches
        // with a repository plugin that happens to use the same local ID.
        let development = ManagedIdentity::development("sample".into()).unwrap();
        assert_eq!(development.key(), "development/sample");
        assert_ne!(
            development,
            ManagedIdentity::new(1, "sample".into()).unwrap()
        );
        assert!(ManagedIdentity::development("../sample".into()).is_err());
        let mut value = sample();
        value["delivery_channels"] = json!([{"id":"one","name":"One","description":"Delivery"},{"id":"one","name":"Two","description":"Duplicate"}]);
        assert!(Manifest::parse(value.to_string().as_bytes()).is_err());
    }

    #[test]
    fn documented_manifest_example_is_parsed_by_the_production_model() {
        let manifest =
            Manifest::parse(include_bytes!("../../examples/managed-manifest.json")).unwrap();
        assert!(manifest
            .compatibility(1, 2, &["status", "settings"])
            .is_ok());
        assert!(manifest
            .compatibility(1, 1, &["status", "settings"])
            .is_err());
        let schema = manifest.settings.unwrap();
        let effective = schema.effective(None).unwrap();
        assert_eq!(effective["interval"], 5);
        assert_eq!(effective["label"], "Status");
        assert!(!effective.contains_key("credential"));
        assert_eq!(schema.fields.len(), 6);
    }

    #[test]
    fn static_inspection_checks_containment_and_does_not_execute_a_probe() {
        let path = root();
        let before = std::fs::read(path.join("among-plugin.json")).unwrap();
        let inspected =
            inspect(&path, ManagedIdentity::new(123, "sample".into()).unwrap()).unwrap();
        assert_eq!(inspected.manifest.id, "sample");
        assert_eq!(inspected.manifest.name, "Sample");
        assert_eq!(inspected.executable, path.join("bin/plugin"));
        assert_eq!(
            std::fs::read(path.join("among-plugin.json")).unwrap(),
            before
        );
        assert!(!path.join("executed").exists());
        assert!(inspect(&path, ManagedIdentity::new(123, "other".into()).unwrap()).is_err());
        std::fs::remove_file(path.join("bin/plugin")).unwrap();
        std::os::unix::fs::symlink("/bin/sh", path.join("bin/plugin")).unwrap();
        assert!(inspect(&path, ManagedIdentity::new(123, "sample".into()).unwrap()).is_err());
    }

    #[test]
    fn parent_links_hardlinks_special_files_and_non_executables_are_not_resources() {
        let path = root();
        let identity = ManagedIdentity::new(123, "sample".into()).unwrap();
        std::fs::set_permissions(
            path.join("bin/plugin"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert!(inspect(&path, identity.clone()).is_err());
        std::fs::remove_file(path.join("bin/plugin")).unwrap();
        std::fs::write(path.join("outside"), b"outside").unwrap();
        std::fs::set_permissions(path.join("outside"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        std::fs::hard_link(path.join("outside"), path.join("bin/plugin")).unwrap();
        assert!(inspect(&path, identity.clone()).is_err());
        std::fs::remove_file(path.join("bin/plugin")).unwrap();
        let fifo =
            std::ffi::CString::new(path.join("bin/plugin").as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let start = std::time::Instant::now();
        assert!(inspect(&path, identity.clone()).is_err());
        assert!(start.elapsed() < std::time::Duration::from_millis(500));
        std::fs::remove_file(path.join("bin/plugin")).unwrap();
        std::fs::remove_dir(path.join("bin")).unwrap();
        std::os::unix::fs::symlink("/bin", path.join("bin")).unwrap();
        assert!(inspect(&path, identity).is_err());
        assert_eq!(std::fs::read(path.join("outside")).unwrap(), b"outside");
    }

    #[test]
    fn malformed_schema_duplicate_keys_feature_sets_and_resource_escapes_are_refused() {
        for (key, value) in [
            ("schema_version", json!(2)),
            ("entrypoint", json!("../plugin")),
            ("icon", json!("/outside")),
            ("id", json!("Upper")),
            ("required_features", json!(["status", "status"])),
        ] {
            let mut manifest = sample();
            manifest[key] = value;
            assert!(
                Manifest::parse(manifest.to_string().as_bytes()).is_err(),
                "{key}"
            );
        }
        let text = sample().to_string().replacen(
            "\"schema_version\":1",
            "\"schema_version\":1,\"schema_version\":1",
            1,
        );
        assert!(Manifest::parse(text.as_bytes()).is_err());
        assert!(Manifest::parse(&vec![b' '; MAX_MANIFEST_BYTES + 1]).is_err());
        let mut value = sample();
        value["validator"] = json!("run-me");
        assert!(Manifest::parse(value.to_string().as_bytes()).is_err());
    }

    #[test]
    fn optional_corrupt_icons_fall_back_but_never_expand_pixel_budgets() {
        let path = root();
        let mut value = sample();
        value["icon"] = json!("icon.png");
        std::fs::write(path.join("among-plugin.json"), value.to_string()).unwrap();
        std::fs::write(path.join("icon.png"), b"not an image").unwrap();
        std::fs::set_permissions(
            path.join("icon.png"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let inspected = inspect(&path, ManagedIdentity::new(1, "sample".into()).unwrap()).unwrap();
        assert!(inspected.raster_icon.is_none());
        assert!(inspected.icon_problem.is_some());
        for invalid in [json!(""), json!(23), json!("theme:not/valid")] {
            value["icon"] = invalid;
            std::fs::write(path.join("among-plugin.json"), value.to_string()).unwrap();
            let inspected =
                inspect(&path, ManagedIdentity::new(1, "sample".into()).unwrap()).unwrap();
            assert!(inspected.manifest.icon.is_none());
            assert!(inspected.icon_problem.is_some());
        }
        let mut fake = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        fake.extend(2049u32.to_be_bytes());
        fake.extend(1u32.to_be_bytes());
        assert!(validate_png(fake).unwrap_err().to_string().contains("2048"));
    }

    #[test]
    fn real_static_png_decodes_and_compressed_metadata_is_not_accepted() {
        let image = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, true, 8, 2, 3).unwrap();
        image.fill(0x336699ff);
        let bytes = image.save_to_bufferv("png", &[]).unwrap();
        assert_eq!(validate_png(bytes.clone()).unwrap(), bytes);
        let mut with_text = bytes[..bytes.len() - 12].to_vec();
        with_text.extend(0u32.to_be_bytes());
        with_text.extend(b"zTXt");
        with_text.extend(0u32.to_be_bytes());
        with_text.extend(&bytes[bytes.len() - 12..]);
        assert!(validate_png(with_text).is_err());
    }
}
