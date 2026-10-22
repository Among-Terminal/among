use super::*;
use among::notifications::preferences::{self, Edit};

impl PreferencesStorage {
    pub fn general_changed(&mut self, edit: Edit) -> Result<(), StorageError> {
        if let Err(problem) = edit.validate() {
            let result = Err(invalid(Path::new(Document::General.filename()), problem));
            self.record(Document::General, &result);
            return result;
        }
        let same = |previous: &Edit| match (previous, &edit) {
            (Edit::Provider(a, _), Edit::Provider(b, _)) => a == b,
            _ => std::mem::discriminant(previous) == std::mem::discriminant(&edit),
        };
        self.pending_general.retain(|previous| !same(previous));
        if matches!(edit, Edit::CommandConfiguration(None)) {
            self.pending_general
                .retain(|previous| !matches!(previous, Edit::CommandPolicy(_)));
        }
        self.pending_general.push(edit);
        // a pending enable may depend on a configuration accepted in the same
        // session; prepare it first even after repeated configuration edits.
        self.pending_general
            .sort_by_key(|edit| !matches!(edit, Edit::CommandConfiguration(_)));
        let result = self.path(Document::General).and_then(|path| {
            update(&path, Document::General, |document| {
                for edit in &self.pending_general {
                    preferences::merge(document, edit)?;
                }
                Ok(())
            })
        });
        if result.is_ok() {
            self.pending_general.clear();
        }
        self.record(Document::General, &result);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use among::notifications::{
        commands::Configuration, preferences::Preferences, Eligibility, Policy,
    };

    #[test]
    fn legacy_owner_controlled_directory_does_not_make_general_session_only() {
        let root = std::env::temp_dir().join(format!(
            "among-general-legacy-directory-{}",
            std::process::id()
        ));
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(&root)
            .unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        let legacy = br#"{"plugins":[{"name":"retained","enabled":false}],"future":42}"#;
        std::fs::write(root.join("plugins.json"), legacy).unwrap();
        let mut storage = PreferencesStorage::new(Some(root.clone()));
        let result = storage.general_changed(Edit::Visual(Policy {
            enabled: false,
            eligibility: Eligibility::Always,
        }));
        assert!(result.is_ok(), "safe legacy parent refused: {result:?}");
        assert!(
            !Preferences::parse(&storage.load(Document::General).unwrap())
                .unwrap()
                .visual
                .enabled
        );
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            std::fs::metadata(root.join("general.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(std::fs::read(root.join("plugins.json")).unwrap(), legacy);
    }

    #[test]
    fn inert_defaults_unknown_preservation_restart_and_failed_write_retry() {
        let path =
            std::env::temp_dir().join(format!("among-general-storage-test-{}", std::process::id()));
        let mut storage = PreferencesStorage::new(Some(path.clone()));
        assert_eq!(
            Preferences::parse(&storage.load(Document::General).unwrap()).unwrap(),
            Preferences::default()
        );
        assert!(!path.exists(), "inspection wrote defaults");
        let policy = Policy {
            enabled: false,
            eligibility: Eligibility::Always,
        };
        storage.general_changed(Edit::Visual(policy)).unwrap();
        let file = path.join("general.json");
        let mut document = load(&file, Document::General).unwrap();
        document["future"] = json!({"keep":42});
        document["notifications"]["audio"] =
            json!({"enabled":true,"selection":"future-audio","extra":7});
        document["notifications"]["visual"]["future"] = json!(7);
        std::fs::write(&file, serde_json::to_vec(&document).unwrap()).unwrap();
        let command =
            Configuration::new(Path::new("/usr/bin/true"), vec!["; $HOME".into()]).unwrap();
        storage
            .general_changed(Edit::CommandConfiguration(Some(command.clone())))
            .unwrap();
        storage
            .general_changed(Edit::CommandPolicy(Policy {
                enabled: true,
                eligibility: Eligibility::Always,
            }))
            .unwrap();
        let restart = Preferences::parse(
            &PreferencesStorage::new(Some(path.clone()))
                .load(Document::General)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(restart.visual, policy);
        assert_eq!(restart.configuration, Some(command));
        assert!(restart.command.enabled);
        let saved = load(&file, Document::General).unwrap();
        assert_eq!(saved["future"], document["future"]);
        assert_eq!(
            saved["notifications"]["audio"],
            document["notifications"]["audio"]
        );
        assert_eq!(saved["notifications"]["visual"]["future"], 7);
        std::fs::write(&file, b"malformed; do not repair").unwrap();
        assert!(storage
            .general_changed(Edit::Visual(Policy::VISUAL_DEFAULT))
            .is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"malformed; do not repair");
        std::fs::write(&file, serde_json::to_vec(&saved).unwrap()).unwrap();
        storage
            .general_changed(Edit::CommandPolicy(Policy::DELIVERY_DEFAULT))
            .unwrap();
        assert_eq!(
            Preferences::parse(&load(&file, Document::General).unwrap())
                .unwrap()
                .visual,
            Policy::VISUAL_DEFAULT
        );
        assert!(storage.pending_general.is_empty());
        assert_eq!(
            std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn future_duplicate_unsafe_and_uncertain_documents_are_not_silently_repaired() {
        let root =
            std::env::temp_dir().join(format!("among-general-invalid-test-{}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let path = root.join("general.json");
        for bytes in [
            r#"{"version":2}"#,
            r#"{"version":1,"version":1}"#,
            r#"{"version":1,"notifications":{"command":{"enabled":true}}}"#,
            r#"{"version":1,"notifications":{"visual":{"eligibility":"future"}}}"#,
        ] {
            std::fs::write(&path, bytes).unwrap();
            let mut storage = PreferencesStorage::new(Some(root.clone()));
            assert!(storage.load(Document::General).is_none());
            assert!(storage
                .general_changed(Edit::Visual(Policy::VISUAL_DEFAULT))
                .is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
        }
        std::fs::write(&path, br#"{"version":1}"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let result = update_with_confirmation(
            &path,
            Document::General,
            |document| {
                preferences::merge(
                    document,
                    &Edit::Visual(Policy {
                        enabled: false,
                        eligibility: Eligibility::Always,
                    }),
                )
            },
            |_| Ok(()),
            |_| {
                Err(io::Error::other(
                    "controlled directory confirmation failure",
                ))
            },
        );
        let error = result.unwrap_err();
        assert!(error.replaced, "{error}");
        assert!(
            !Preferences::parse(&load(&path, Document::General).unwrap())
                .unwrap()
                .visual
                .enabled
        );
        let original = std::fs::read(&path).unwrap();
        let target = root.join("target.json");
        std::fs::rename(&path, &target).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(PreferencesStorage::new(Some(root))
            .general_changed(Edit::Visual(Policy::VISUAL_DEFAULT))
            .is_err());
        assert_eq!(std::fs::read(target).unwrap(), original);
    }
}
