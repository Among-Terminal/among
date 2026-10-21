//! Preference documents stay independent of GTK and preserve manual additions.

use serde_json::{json, Value};
use std::io;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug)]
pub enum Document {
    Plugins,
    ManagedPlugins,
    Shortcuts,
    Window,
}

pub struct PreferencesStorage {
    directory: Option<PathBuf>,
    pending_plugins: std::collections::BTreeMap<String, bool>,
    pending_shortcuts: std::collections::BTreeMap<String, Value>,
    pub problems: std::collections::BTreeMap<String, String>,
}

impl PreferencesStorage {
    pub fn new(directory: Option<PathBuf>) -> Self {
        Self {
            directory,
            pending_plugins: Default::default(),
            pending_shortcuts: Default::default(),
            problems: Default::default(),
        }
    }

    pub fn load(&mut self, kind: Document) -> Option<Value> {
        let result = self.path(kind).and_then(|path| load(&path, kind));
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                self.problems
                    .insert(kind.filename().into(), error.to_string());
                None
            }
        }
    }

    fn path(&self, kind: Document) -> Result<PathBuf, StorageError> {
        self.directory
            .as_ref()
            .map(|dir| dir.join(kind.filename()))
            .ok_or_else(|| {
                invalid(
                    Path::new(kind.filename()),
                    "HOME and XDG_CONFIG_HOME unavailable",
                )
            })
    }

    pub fn plugin_changed(&mut self, name: &str, enabled: bool) -> Result<(), StorageError> {
        if name.starts_with("github:") && name.contains('/') {
            let result = self.path(Document::ManagedPlugins).and_then(|path| {
                update(&path, Document::ManagedPlugins, |value| {
                    let entry = &mut value["plugins"][name];
                    if entry.is_null() {
                        *entry = json!({});
                    }
                    entry["enabled"] = json!(enabled);
                    Ok(())
                })
            });
            self.record(Document::ManagedPlugins, &result);
            return result;
        }
        self.pending_plugins.insert(name.into(), enabled);
        let result = self.path(Document::Plugins).and_then(|path| {
            update(&path, Document::Plugins, |value| {
                merge_plugins(value, &self.pending_plugins)
            })
        });
        if result.is_ok() {
            self.pending_plugins.clear();
        }
        self.record(Document::Plugins, &result);
        result
    }

    pub fn shortcuts_changed(
        &mut self,
        changes: std::collections::BTreeMap<String, Value>,
    ) -> Result<(), StorageError> {
        self.pending_shortcuts.extend(changes);
        if self.pending_shortcuts.is_empty() {
            return Ok(());
        }
        let result = self.path(Document::Shortcuts).and_then(|path| {
            update(&path, Document::Shortcuts, |value| {
                let overrides = value
                    .get_mut("overrides")
                    .and_then(Value::as_object_mut)
                    .ok_or("overrides must be an object")?;
                overrides.extend(self.pending_shortcuts.clone());
                Ok(())
            })
        });
        if result.is_ok() {
            self.pending_shortcuts.clear();
        }
        self.record(Document::Shortcuts, &result);
        result
    }

    pub fn managed_settings_changed(
        &mut self,
        key: &str,
        schema: &crate::plugin::schema::SettingsSchema,
        changes: &serde_json::Map<String, Value>,
    ) -> Result<Value, StorageError> {
        let result = self.path(Document::ManagedPlugins).and_then(|path| {
            update(&path, Document::ManagedPlugins, |value| {
                let stored = value["plugins"][key].get("settings");
                let merged = schema
                    .merge_edit(stored, changes)
                    .map_err(|problem| problem.to_string())?;
                let entry = &mut value["plugins"][key];
                if entry.is_null() {
                    *entry = json!({});
                }
                entry["settings"] = merged;
                Ok(())
            })?;
            load(&path, Document::ManagedPlugins)?["plugins"][key]
                .get("settings")
                .cloned()
                .ok_or_else(|| invalid(&path, "saved settings could not be reloaded"))
        });
        self.record(Document::ManagedPlugins, &result);
        result
    }

    pub fn sidebar_width_changed(&mut self, width: f64) -> Result<(), StorageError> {
        let result = self.path(Document::Window).and_then(|path| {
            if !width.is_finite() || width <= 0.0 {
                return Err(invalid(
                    &path,
                    "sidebar_width must be a positive finite number",
                ));
            }
            update(&path, Document::Window, |value| {
                value["sidebar_width"] = json!(width);
                Ok(())
            })
        });
        self.record(Document::Window, &result);
        result
    }

    fn record<T>(&mut self, kind: Document, result: &Result<T, StorageError>) {
        match result {
            Ok(_) => {
                self.problems.remove(kind.filename());
            }
            Err(error) => {
                let prefix = if error.replaced {
                    "Durability uncertain"
                } else {
                    "Changes are session-only"
                };
                self.problems
                    .insert(kind.filename().into(), format!("{prefix}: {error}"));
            }
        }
    }

    pub fn status(&self) -> String {
        self.problems
            .values()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Document {
    pub fn filename(self) -> &'static str {
        match self {
            Self::Plugins => "plugins.json",
            Self::ManagedPlugins => "managed-plugins.json",
            Self::Shortcuts => "shortcuts.json",
            Self::Window => "window.json",
        }
    }

    fn empty(self) -> Value {
        match self {
            Self::Plugins => json!({"plugins": []}),
            Self::ManagedPlugins => json!({"version":1,"plugins":{}}),
            Self::Shortcuts => json!({"version": 1, "overrides": {}}),
            Self::Window => json!({"version": 1}),
        }
    }
}

#[derive(Debug)]
pub struct StorageError {
    pub path: PathBuf,
    pub message: String,
    pub replaced: bool,
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.message)
    }
}

impl std::error::Error for StorageError {}

fn invalid(path: &Path, message: impl ToString) -> StorageError {
    StorageError {
        path: path.into(),
        message: message.to_string(),
        replaced: false,
    }
}

pub fn directory_from(xdg: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    xdg.filter(|path| !path.as_os_str().is_empty())
        .or_else(|| home.map(|path| path.join(".config")))
        .map(|path| path.join("among"))
}

pub fn directory() -> Option<PathBuf> {
    directory_from(
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

fn read_bytes(path: &Path, kind: Document) -> Result<Option<Vec<u8>>, StorageError> {
    if matches!(kind, Document::ManagedPlugins) {
        if matches!(std::fs::symlink_metadata(path),Err(error) if error.kind()==io::ErrorKind::NotFound)
        {
            return Ok(None);
        }
        let parent = path
            .parent()
            .ok_or_else(|| invalid(path, "configuration has no parent"))?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| invalid(path, "invalid configuration filename"))?;
        return crate::plugin::data::owned_directory(parent)
            .and_then(|parent| crate::plugin::data::contained_file(&parent, name))
            .and_then(|file| {
                crate::plugin::data::read_bounded(&file, crate::plugin::data::MAX_STATIC_BYTES)
            })
            .map(Some)
            .map_err(|problem| invalid(path, problem));
    }
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(invalid(path, error)),
    }
}

fn parse(path: &Path, kind: Document, bytes: Option<&[u8]>) -> Result<Value, StorageError> {
    let Some(bytes) = bytes else {
        return Ok(kind.empty());
    };
    let value: Value = if matches!(kind, Document::ManagedPlugins) {
        crate::plugin::data::parse_unique(bytes, crate::plugin::data::MAX_STATIC_BYTES)
            .map_err(|problem| invalid(path, problem))?
    } else {
        serde_json::from_slice(bytes).map_err(|error| invalid(path, error))?
    };
    if !value.is_object() {
        return Err(invalid(path, "configuration must be an object"));
    }
    match kind {
        Document::ManagedPlugins => {
            if value.get("version").and_then(Value::as_u64) != Some(1)
                || !value.get("plugins").is_some_and(Value::is_object)
            {
                return Err(invalid(path, "unsupported managed preferences document"));
            }
            if value["plugins"]
                .as_object()
                .is_some_and(|entries| entries.values().any(|entry| !entry.is_object()))
            {
                return Err(invalid(path, "managed plugin preferences must be objects"));
            }
        }
        Document::Plugins if value.get("plugins").is_some_and(|v| !v.is_array()) => {
            return Err(invalid(path, "plugins must be an array"))
        }
        Document::Shortcuts => {
            if value.get("version").and_then(Value::as_u64) != Some(1) {
                return Err(invalid(path, "unsupported shortcut document version"));
            }
            if !value.get("overrides").is_some_and(Value::is_object) {
                return Err(invalid(path, "overrides must be an object"));
            }
        }
        Document::Window => {
            if value.get("version").and_then(Value::as_u64) != Some(1) {
                return Err(invalid(path, "unsupported window document version"));
            }
            if let Some(width) = value.get("sidebar_width") {
                if !width
                    .as_f64()
                    .is_some_and(|width| width.is_finite() && width > 0.0)
                {
                    return Err(invalid(
                        path,
                        "sidebar_width must be a positive finite number",
                    ));
                }
            }
        }
        _ => {}
    }
    Ok(value)
}

pub fn load(path: &Path, kind: Document) -> Result<Value, StorageError> {
    parse(path, kind, read_bytes(path, kind)?.as_deref())
}

fn destination_mode(path: &Path) -> Result<u32, StorageError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(metadata.permissions().mode() & 0o600),
        Ok(_) => Err(invalid(
            path,
            "refusing symlink or non-regular configuration destination",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0o600),
        Err(error) => Err(invalid(path, error)),
    }
}

/// Re-read each attempt: a UI edit must not erase independent manual edits.
pub fn update(
    path: &Path,
    kind: Document,
    edit: impl Fn(&mut Value) -> Result<(), String>,
) -> Result<(), StorageError> {
    update_with_hook(path, kind, edit, |_| Ok(()))
}

fn update_with_hook(
    path: &Path,
    kind: Document,
    edit: impl Fn(&mut Value) -> Result<(), String>,
    mut before_rename: impl FnMut(&Path) -> io::Result<()>,
) -> Result<(), StorageError> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid(path, "configuration has no parent"))?;
    for _ in 0..3 {
        let mode = destination_mode(path)?;
        let original = read_bytes(path, kind)?;
        let mut value = parse(path, kind, original.as_deref())?;
        edit(&mut value).map_err(|message| invalid(path, message))?;
        if matches!(kind, Document::ManagedPlugins) {
            crate::plugin::data::serialized_size(&value, crate::plugin::data::MAX_STATIC_BYTES)
                .map_err(|problem| invalid(path, problem))?;
        }
        let bytes = serde_json::to_vec_pretty(&value).map_err(|error| invalid(path, error))?;
        // unknown data can expand under pretty formatting. protect the reader's
        // actual byte budget before any replacement is prepared or published.
        if matches!(kind, Document::ManagedPlugins)
            && bytes.len() > crate::plugin::data::MAX_STATIC_BYTES
        {
            return Err(invalid(
                path,
                "encoded preferences exceed the managed document byte limit",
            ));
        }
        if matches!(kind, Document::ManagedPlugins) {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .map_err(|error| invalid(path, error))?;
            crate::plugin::data::owned_directory(parent)
                .map_err(|problem| invalid(path, problem))?;
        } else {
            std::fs::create_dir_all(parent).map_err(|error| invalid(path, error))?;
        }
        let temporary = parent.join(format!(
            ".among-settings-{}-{}",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temporary)
            .map_err(|error| invalid(path, error))?;
        let prepared = (|| {
            file.write_all(&bytes)?;
            file.sync_all()?;
            before_rename(&temporary)
        })();
        if let Err(error) = prepared {
            let _ = std::fs::remove_file(&temporary);
            return Err(invalid(path, error));
        }
        let current = destination_mode(path).and_then(|_| read_bytes(path, kind));
        match current {
            Ok(current) if current != original => {
                let _ = std::fs::remove_file(&temporary);
                continue;
            }
            Err(error) => {
                let _ = std::fs::remove_file(&temporary);
                return Err(error);
            }
            _ => {}
        }
        if let Err(error) = std::fs::rename(&temporary, path) {
            let _ = std::fs::remove_file(&temporary);
            return Err(invalid(path, error));
        }
        return std::fs::File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|error| StorageError {
                path: path.into(),
                message: format!("replacement written, durability uncertain: {error}"),
                replaced: true,
            });
    }
    Err(invalid(
        path,
        "configuration changed concurrently; retry the preference edit",
    ))
}

pub fn merge_plugins(
    value: &mut Value,
    changes: &std::collections::BTreeMap<String, bool>,
) -> Result<(), String> {
    let object = value
        .as_object_mut()
        .ok_or("configuration must be an object")?;
    let entries = object
        .entry("plugins")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or("plugins must be an array")?;
    for (name, enabled) in changes {
        let mut found = false;
        for entry in entries
            .iter_mut()
            .filter(|entry| entry.get("name").and_then(Value::as_str) == Some(name.as_str()))
        {
            if let Some(entry) = entry.as_object_mut() {
                entry.insert("enabled".into(), json!(enabled));
                found = true;
            }
        }
        if !found {
            entries.push(json!({"name":name,"enabled":enabled}));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "among-settings-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn paths_and_absent_documents_use_safe_defaults() {
        assert_eq!(
            directory_from(Some("/xdg".into()), Some("/home/test".into())),
            Some("/xdg/among".into())
        );
        assert_eq!(
            directory_from(None, Some("/home/test".into())),
            Some("/home/test/.config/among".into())
        );
        assert_eq!(directory_from(None, None), None);
        let temp = Temp::new();
        assert_eq!(
            load(&temp.0.join("plugins.json"), Document::Plugins).unwrap(),
            json!({"plugins":[]})
        );
        assert_eq!(
            load(&temp.0.join("shortcuts.json"), Document::Shortcuts).unwrap(),
            json!({"version":1,"overrides":{}})
        );
    }

    #[test]
    fn managed_output_expansion_cannot_replace_a_readable_preference_document() {
        let temp = Temp::new();
        std::fs::set_permissions(&temp.0, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = temp.0.join("managed-plugins.json");
        let original =
            serde_json::to_vec(&json!({"version":1,"plugins":{},"future":vec![0;60_000]})).unwrap();
        assert!(original.len() < crate::plugin::data::MAX_STATIC_BYTES);
        std::fs::write(&path, &original).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let prepared = std::cell::Cell::new(false);
        let result = update_with_hook(
            &path,
            Document::ManagedPlugins,
            |document| {
                document["plugins"]["github:1/sample"] = json!({"enabled":true});
                Ok(())
            },
            |_| {
                prepared.set(true);
                Ok(())
            },
        );
        assert!(
            result.is_err(),
            "the actual pretty encoding exceeds the reader's byte budget"
        );
        assert!(!result.unwrap_err().replaced);
        assert!(
            !prepared.get(),
            "oversized output must fail before preparing replacement bytes"
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(
            load(&path, Document::ManagedPlugins).unwrap()["future"]
                .as_array()
                .unwrap()
                .len(),
            60_000
        );
        assert_eq!(std::fs::read_dir(&temp.0).unwrap().count(), 1);
    }

    #[test]
    fn managed_actual_output_limit_accepts_the_boundary_and_refuses_the_next_byte() {
        let limit = crate::plugin::data::MAX_STATIC_BYTES;
        for size in [limit - 1, limit, limit + 1] {
            let temp = Temp::new();
            std::fs::set_permissions(&temp.0, std::fs::Permissions::from_mode(0o700)).unwrap();
            let path = temp.0.join("managed-plugins.json");
            let mut edited =
                json!({"version":1,"plugins":{"github:1/sample":{"enabled":true}},"future":""});
            let overhead = serde_json::to_vec_pretty(&edited).unwrap().len();
            edited["future"] = json!("x".repeat(size - overhead));
            assert_eq!(serde_json::to_vec_pretty(&edited).unwrap().len(), size);
            let mut initial = edited.clone();
            initial["plugins"]["github:1/sample"]["enabled"] = json!(false);
            let original = serde_json::to_vec(&initial).unwrap();
            assert!(original.len() < limit);
            std::fs::write(&path, &original).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            let result = update(&path, Document::ManagedPlugins, |document| {
                document["plugins"]["github:1/sample"]["enabled"] = json!(true);
                Ok(())
            });
            if size > limit {
                assert!(!result.unwrap_err().replaced);
                assert_eq!(std::fs::read(&path).unwrap(), original);
            } else {
                result.unwrap();
                assert_eq!(std::fs::metadata(&path).unwrap().len(), size as u64);
                assert_eq!(load(&path, Document::ManagedPlugins).unwrap(), edited);
            }
        }
    }

    #[test]
    fn managed_edits_preserve_origins_unknown_data_and_legacy_bytes() {
        let temp = Temp::new();
        std::fs::set_permissions(&temp.0, std::fs::Permissions::from_mode(0o700)).unwrap();
        let legacy = temp.0.join("plugins.json");
        std::fs::write(&legacy, b"legacy bytes never inspected by managed settings").unwrap();
        let mut storage = PreferencesStorage::new(Some(temp.0.clone()));
        let schema = crate::plugin::schema::SettingsSchema::parse(
            json!({"version":1,"apply":"live","fields":[
            {"key":"label","label":"Label","kind":"string","max_length":8,"default":"base"}]}),
        )
        .unwrap();
        let first = "github:1/sample";
        let second = "github:2/sample";
        storage.plugin_changed(first, false).unwrap();
        storage.plugin_changed(second, true).unwrap();
        let path = temp.0.join("managed-plugins.json");
        update(&path, Document::ManagedPlugins, |document| {
            document["plugins"][first]["settings"] =
                json!({"version":1,"values":{"future":"preserve"},"extra":true});
            Ok(())
        })
        .unwrap();
        storage
            .managed_settings_changed(
                first,
                &schema,
                json!({"label":"edited"}).as_object().unwrap(),
            )
            .unwrap();
        let saved = load(&path, Document::ManagedPlugins).unwrap();
        assert_eq!(
            saved["plugins"][first]["settings"]["values"]["future"],
            "preserve"
        );
        assert_eq!(saved["plugins"][first]["settings"]["extra"], true);
        assert_eq!(saved["plugins"][second]["enabled"], true);
        assert!(saved["plugins"][second].get("settings").is_none());
        let before = std::fs::read(&path).unwrap();
        let error = storage
            .managed_settings_changed(
                first,
                &schema,
                json!({"label":"sensitive-too-long-value"})
                    .as_object()
                    .unwrap(),
            )
            .unwrap_err();
        assert!(!error.to_string().contains("sensitive-too-long-value"));
        assert_eq!(before, std::fs::read(&path).unwrap());
        assert_eq!(
            std::fs::read(legacy).unwrap(),
            b"legacy bytes never inspected by managed settings"
        );
    }

    #[test]
    fn managed_duplicate_oversized_future_and_symlink_documents_are_never_repaired() {
        let temp = Temp::new();
        std::fs::set_permissions(&temp.0, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = temp.0.join("managed-plugins.json");
        for body in [
            r#"{"version":1,"version":1,"plugins":{}}"#.to_string(),
            r#"{"version":2,"plugins":{}}"#.into(),
            r#"{"version":1,"plugins":{"x":true}}"#.into(),
            " ".repeat(crate::plugin::data::MAX_STATIC_BYTES + 1),
        ] {
            std::fs::write(&path, &body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            let mut storage = PreferencesStorage::new(Some(temp.0.clone()));
            assert!(storage.load(Document::ManagedPlugins).is_none());
            assert!(storage.plugin_changed("github:1/sample", false).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        }
        let target = temp.0.join("target.json");
        std::fs::rename(&path, &target).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(load(&path, Document::ManagedPlugins).is_err());
    }

    #[test]
    fn malformed_shapes_and_future_versions_are_diagnostic_not_repaired() {
        let temp = Temp::new();
        let path = temp.0.join("settings.json");
        for (kind, body) in [
            (Document::Plugins, "broken"),
            (Document::Plugins, "[]"),
            (Document::Plugins, "{\"plugins\":1}"),
            (Document::Shortcuts, "{\"version\":2,\"overrides\":{}}"),
            (Document::Shortcuts, "{\"version\":1,\"overrides\":[]}"),
        ] {
            std::fs::write(&path, body).unwrap();
            let error = load(&path, kind).unwrap_err();
            assert_eq!(error.path, path);
            assert!(!error.replaced);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        }
        std::fs::write(&path, "{\"extra\":42,\"plugins\":[]}").unwrap();
        assert_eq!(load(&path, Document::Plugins).unwrap()["extra"], 42);
    }

    #[test]
    fn atomic_merge_preserves_manual_data_and_missing_plugins() {
        let temp = Temp::new();
        let path = temp.0.join("plugins.json");
        std::fs::write(&path,r#"{"extra":7,"plugins":[{"name":"missing","enabled":true},{"name":"clock","enabled":false,"custom":42}]}"#).unwrap();
        let changes = std::collections::BTreeMap::from([("clock".to_string(), true)]);
        update(&path, Document::Plugins, |value| {
            merge_plugins(value, &changes)
        })
        .unwrap();
        let value = load(&path, Document::Plugins).unwrap();
        assert_eq!(value["extra"], 7);
        assert_eq!(
            value["plugins"][0],
            json!({"name":"missing","enabled":true})
        );
        assert_eq!(
            value["plugins"][1],
            json!({"name":"clock","enabled":true,"custom":42})
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
            0
        );
    }

    #[test]
    fn interrupted_prepare_and_symlink_leave_original_intact() {
        let temp = Temp::new();
        let path = temp.0.join("plugins.json");
        let body = r#"{"plugins":[],"keep":true}"#;
        std::fs::write(&path, body).unwrap();
        let error = update_with_hook(
            &path,
            Document::Plugins,
            |value| {
                value["keep"] = json!(false);
                Ok(())
            },
            |_| Err(io::Error::other("injected interruption before rename")),
        )
        .unwrap_err();
        assert!(!error.replaced);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        assert_eq!(std::fs::read_dir(&temp.0).unwrap().count(), 1);
        let link = temp.0.join("linked.json");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(update(&link, Document::Plugins, |_| Ok(())).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
    }

    #[test]
    fn concurrent_edit_is_merged_on_retry_or_reported() {
        let temp = Temp::new();
        let path = temp.0.join("plugins.json");
        std::fs::write(&path, r#"{"plugins":[]}"#).unwrap();
        let mut first = true;
        update_with_hook(
            &path,
            Document::Plugins,
            |value| {
                value["own"] = json!(1);
                Ok(())
            },
            |_| {
                if first {
                    first = false;
                    std::fs::write(&path, r#"{"plugins":[],"external":42}"#)?;
                }
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            load(&path, Document::Plugins).unwrap(),
            json!({"plugins":[],"external":42,"own":1})
        );
        let mut count = 0;
        let error = update_with_hook(
            &path,
            Document::Plugins,
            |_| Ok(()),
            |_| {
                count += 1;
                std::fs::write(&path, format!("{{\"plugins\":[],\"external\":{count}}}"))
            },
        )
        .unwrap_err();
        assert!(error.message.contains("concurrently"));
        assert_eq!(count, 3);
    }

    #[test]
    fn documented_fixtures_load_without_modification() {
        let temp = Temp::new();
        let doc = include_str!("../docs/preferences.md");
        let fixtures: Vec<&str> = doc
            .split("```json\n")
            .skip(1)
            .map(|body| body.split("```").next().unwrap().trim())
            .collect();
        assert_eq!(fixtures.len(), 3);
        for (kind, body) in [Document::Plugins, Document::Shortcuts, Document::Window]
            .into_iter()
            .zip(fixtures)
        {
            let path = temp.0.join(kind.filename());
            std::fs::write(&path, body).unwrap();
            assert!(load(&path, kind).is_ok());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        }
    }

    #[test]
    fn failed_pending_edits_retry_without_erasing_unrelated_data() {
        let temp = Temp::new();
        let dir = temp.0.join("config");
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("plugins.json");
        std::fs::write(&path, "broken").unwrap();
        let mut store = PreferencesStorage::new(Some(dir.clone()));
        assert!(store.plugin_changed("clock", true).is_err());
        assert!(store.status().contains("session-only"));
        std::fs::write(&path, r#"{"plugins":[],"external":42}"#).unwrap();
        store.plugin_changed("pane-pwd", false).unwrap();
        assert!(store.status().is_empty());
        let value = load(&path, Document::Plugins).unwrap();
        assert_eq!(value["external"], 42);
        assert_eq!(
            value["plugins"],
            json!([{"name":"clock","enabled":true},{"name":"pane-pwd","enabled":false}])
        );
        let shortcut = dir.join("shortcuts.json");
        std::fs::write(&shortcut, r#"{"version":2,"overrides":{}}"#).unwrap();
        assert!(store
            .shortcuts_changed(std::collections::BTreeMap::from([(
                "split-pane".into(),
                Value::Null
            )]))
            .is_err());
        assert!(store.status().contains("unsupported"));
        std::fs::write(
            &shortcut,
            r#"{"version":1,"overrides":{"future-action":"F8"},"external":true}"#,
        )
        .unwrap();
        store.shortcuts_changed(Default::default()).unwrap();
        let value = load(&shortcut, Document::Shortcuts).unwrap();
        assert!(value["overrides"]["split-pane"].is_null());
        assert_eq!(value["overrides"]["future-action"], "F8");
        assert_eq!(value["external"], true);
    }

    #[test]
    fn window_width_defaults_and_saved_edits_preserve_unrelated_fields() {
        let temp = Temp::new();
        let mut store = PreferencesStorage::new(Some(temp.0.clone()));
        assert_eq!(store.load(Document::Window), Some(json!({"version":1})));
        let path = temp.0.join("window.json");
        assert!(
            !path.exists(),
            "loading defaults must not create a preference"
        );
        std::fs::write(&path, r#"{"version":1,"sidebar_width":240,"external":42}"#).unwrap();
        store.sidebar_width_changed(417.5).unwrap();
        let mut reopened = PreferencesStorage::new(Some(temp.0.clone()));
        assert_eq!(
            reopened.load(Document::Window),
            Some(json!({
                "version":1,"sidebar_width":417.5,"external":42
            }))
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
            0
        );
    }

    #[test]
    fn invalid_window_preferences_are_not_replaced_by_resize() {
        let temp = Temp::new();
        let path = temp.0.join("window.json");
        for body in [
            "broken",
            "[]",
            r#"{"version":2}"#,
            r#"{"sidebar_width":300}"#,
            r#"{"version":1,"sidebar_width":0}"#,
            r#"{"version":1,"sidebar_width":-2}"#,
            r#"{"version":1,"sidebar_width":"wide"}"#,
            r#"{"version":1,"sidebar_width":null}"#,
        ] {
            std::fs::write(&path, body).unwrap();
            let mut store = PreferencesStorage::new(Some(temp.0.clone()));
            assert!(store.load(Document::Window).is_none(), "{body}");
            assert!(!store.status().is_empty());
            assert!(store.sidebar_width_changed(320.0).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        }
    }

    #[test]
    fn failed_window_write_keeps_bytes_and_recovers_on_the_next_resize() {
        let temp = Temp::new();
        let path = temp.0.join("window.json");
        let body = r#"{"version":1,"sidebar_width":240,"external":42}"#;
        std::fs::write(&path, body).unwrap();
        let link = temp.0.join("linked-window.json");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(update(&link, Document::Window, |value| {
            value["sidebar_width"] = json!(500);
            Ok(())
        })
        .is_err());
        let error = update_with_hook(
            &path,
            Document::Window,
            |value| {
                value["sidebar_width"] = json!(500);
                Ok(())
            },
            |_| Err(io::Error::other("interrupted window save")),
        )
        .unwrap_err();
        assert!(!error.replaced);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        let mut store = PreferencesStorage::new(Some(temp.0.clone()));
        for width in [f64::NAN, f64::INFINITY, -1.0, 0.0] {
            assert!(store.sidebar_width_changed(width).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        }
        store.sidebar_width_changed(500.0).unwrap();
        assert!(store.status().is_empty());
        assert_eq!(
            load(&path, Document::Window).unwrap(),
            json!({
                "version":1,"sidebar_width":500.0,"external":42
            })
        );
    }
}
