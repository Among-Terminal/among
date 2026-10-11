//! Preference documents stay independent of GTK and preserve manual additions.

use serde_json::{json, Value};
use std::io;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug)]
pub enum Document {
    Plugins,
    Shortcuts,
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

    fn record(&mut self, kind: Document, result: &Result<(), StorageError>) {
        match result {
            Ok(()) => {
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
            Self::Shortcuts => "shortcuts.json",
        }
    }

    fn empty(self) -> Value {
        match self {
            Self::Plugins => json!({"plugins": []}),
            Self::Shortcuts => json!({"version": 1, "overrides": {}}),
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

fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>, StorageError> {
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
    let value: Value = serde_json::from_slice(bytes).map_err(|error| invalid(path, error))?;
    if !value.is_object() {
        return Err(invalid(path, "configuration must be an object"));
    }
    match kind {
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
        _ => {}
    }
    Ok(value)
}

pub fn load(path: &Path, kind: Document) -> Result<Value, StorageError> {
    parse(path, kind, read_bytes(path)?.as_deref())
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
        let original = read_bytes(path)?;
        let mut value = parse(path, kind, original.as_deref())?;
        edit(&mut value).map_err(|message| invalid(path, message))?;
        let bytes = serde_json::to_vec_pretty(&value).map_err(|error| invalid(path, error))?;
        std::fs::create_dir_all(parent).map_err(|error| invalid(path, error))?;
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
        let current = destination_mode(path).and_then(|_| read_bytes(path));
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
        assert_eq!(fixtures.len(), 2);
        for (kind, body) in [Document::Plugins, Document::Shortcuts]
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
}
