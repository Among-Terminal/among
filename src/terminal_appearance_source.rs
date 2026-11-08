//! Read-only GNOME Terminal default-profile adapter.
//!
//! The adapter resolves only the default profile identity and the colour keys
//! this feature needs. It never invokes GNOME Terminal, scans unrelated
//! profiles, subscribes to changes, or writes a GSettings key.

use gtk::gdk;
use gtk::gio;
use gtk::gio::prelude::*;

use crate::terminal_appearance::{Rgba, SourceFailure, SourceSnapshot, TextBackgroundPair};

const PROFILES_LIST_SCHEMA: &str = "org.gnome.Terminal.ProfilesList";
const PROFILE_SCHEMA: &str = "org.gnome.Terminal.Legacy.Profile";
const PROFILES_PATH: &str = "/org/gnome/terminal/legacy/profiles:/";

/// A deliberately unavailable snapshot used before the startup read runs.
pub(crate) fn fallback_snapshot() -> SourceSnapshot {
    SourceSnapshot {
        text_background: Err(SourceFailure::Unreadable),
        palette: Err(SourceFailure::Unreadable),
    }
}

/// Take the one startup snapshot from the process schema source and backend.
pub(crate) fn snapshot() -> SourceSnapshot {
    match gio::SettingsSchemaSource::default() {
        Some(source) => snapshot_from(&source, None),
        None => fallback_snapshot(),
    }
}

/// Snapshot through an injectable schema source and backend for isolated tests.
pub(crate) fn snapshot_from(
    source: &gio::SettingsSchemaSource,
    backend: Option<&gio::SettingsBackend>,
) -> SourceSnapshot {
    let Some(list_schema) = source.lookup(PROFILES_LIST_SCHEMA, true) else {
        return missing_schema();
    };
    if !list_schema_ok(&list_schema) {
        return missing_schema();
    }
    let list = gio::Settings::new_full(&list_schema, backend, None);
    let default = list.string("default");
    let default = default.as_str();

    if default.is_empty() {
        return missing(SourceFailure::MissingDefault);
    }
    if !is_uuid(default) {
        return missing(SourceFailure::InvalidIdentity);
    }
    if !list
        .strv("list")
        .iter()
        .any(|candidate| candidate.as_str() == default)
    {
        return missing(SourceFailure::InvalidIdentity);
    }

    let path = profile_path(default);
    let Some(profile_schema) = source.lookup(PROFILE_SCHEMA, true) else {
        return missing_schema();
    };
    if !profile_schema_path_ok(&profile_schema) {
        return missing_schema();
    }
    let profile = gio::Settings::new_full(&profile_schema, backend, Some(path.as_str()));

    // Each dimension is guarded separately: a broken palette key must not hide
    // a valid text/background pair, and a broken text key must not hide a valid
    // palette.
    let palette = if has_key_of_type(&profile_schema, "palette", "as") {
        read_palette(&profile)
    } else {
        Err(SourceFailure::MissingSchema)
    };
    let text_background = if !text_schema_ok(&profile_schema) {
        Err(SourceFailure::MissingSchema)
    } else if profile.boolean("use-theme-colors") {
        Err(SourceFailure::ThemeDerived)
    } else {
        read_text_background(&profile)
    };

    SourceSnapshot {
        text_background,
        palette,
    }
}

fn list_schema_ok(schema: &gio::SettingsSchema) -> bool {
    schema.path().as_deref() == Some(PROFILES_PATH)
        && has_key_of_type(schema, "default", "s")
        && has_key_of_type(schema, "list", "as")
}

fn profile_schema_path_ok(schema: &gio::SettingsSchema) -> bool {
    schema.path().is_none()
}

fn text_schema_ok(schema: &gio::SettingsSchema) -> bool {
    has_key_of_type(schema, "use-theme-colors", "b")
        && has_key_of_type(schema, "foreground-color", "s")
        && has_key_of_type(schema, "background-color", "s")
}

fn has_key_of_type(schema: &gio::SettingsSchema, key: &str, kind: &str) -> bool {
    schema.has_key(key) && schema.key(key).value_type().as_str() == kind
}

fn missing_schema() -> SourceSnapshot {
    SourceSnapshot {
        text_background: Err(SourceFailure::MissingSchema),
        palette: Err(SourceFailure::MissingSchema),
    }
}

fn missing(reason: SourceFailure) -> SourceSnapshot {
    SourceSnapshot {
        text_background: Err(reason),
        palette: Err(reason),
    }
}

fn read_text_background(profile: &gio::Settings) -> Result<TextBackgroundPair, SourceFailure> {
    let foreground = profile.string("foreground-color");
    let background = profile.string("background-color");
    Ok(TextBackgroundPair {
        foreground: parse_colour(foreground.as_str())?,
        background: parse_colour(background.as_str())?,
    })
}

fn read_palette(profile: &gio::Settings) -> Result<[Rgba; 16], SourceFailure> {
    let entries = profile.strv("palette");
    if entries.len() != 16 {
        return Err(SourceFailure::InvalidColour);
    }
    let mut colours = [Rgba::opaque(0.0, 0.0, 0.0).map_err(|_| SourceFailure::InvalidColour)?; 16];
    for (slot, entry) in colours.iter_mut().zip(entries.iter()) {
        *slot = parse_colour(entry.as_str())?;
    }
    Ok(colours)
}

fn parse_colour(text: &str) -> Result<Rgba, SourceFailure> {
    let colour = gdk::RGBA::parse(text).map_err(|_| SourceFailure::InvalidColour)?;
    Rgba::new(colour.red(), colour.green(), colour.blue(), colour.alpha())
        .map_err(|_| SourceFailure::InvalidColour)
}

fn profile_path(uuid: &str) -> String {
    format!("{PROFILES_PATH}:{uuid}/")
}

fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        let expected_hyphen = matches!(index, 8 | 13 | 18 | 23);
        if expected_hyphen {
            if *byte != b'-' {
                return false;
            }
        } else if !byte.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    fn schema_xml(name: &str) -> &'static str {
        match name {
            "base" => include_str!(
                "../tests/fixtures/terminal-gsettings/base/org.gnome.Terminal.gschema.xml"
            ),
            "profileslist-missing-default" => include_str!(
                "../tests/fixtures/terminal-gsettings/profileslist-missing-default/org.gnome.Terminal.gschema.xml"
            ),
            "profileslist-wrong-path" => include_str!(
                "../tests/fixtures/terminal-gsettings/profileslist-wrong-path/org.gnome.Terminal.gschema.xml"
            ),
            "profile-palette-type" => include_str!(
                "../tests/fixtures/terminal-gsettings/profile-palette-type/org.gnome.Terminal.gschema.xml"
            ),
            "profile-missing-theme" => include_str!(
                "../tests/fixtures/terminal-gsettings/profile-missing-theme/org.gnome.Terminal.gschema.xml"
            ),
            other => panic!("unknown terminal GSettings fixture: {other}"),
        }
    }

    struct SchemaFixture {
        directory: PathBuf,
        source: gio::SettingsSchemaSource,
    }

    impl Drop for SchemaFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    fn schema_fixture(name: &str) -> SchemaFixture {
        let directory = std::env::temp_dir().join(format!(
            "among-terminal-schemas-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("private schema directory");
        std::fs::write(
            directory.join("org.gnome.Terminal.gschema.xml"),
            schema_xml(name),
        )
        .expect("write embedded schema XML");
        let status = Command::new("glib-compile-schemas")
            .arg("--strict")
            .arg("--targetdir")
            .arg(&directory)
            .arg(&directory)
            .status()
            .expect("glib-compile-schemas is installed");
        assert!(status.success(), "compile private schema fixture {name}");
        let source = gio::SettingsSchemaSource::from_directory(&directory, None, false)
            .expect("private schema source");
        SchemaFixture { directory, source }
    }

    fn require_memory_backend() {
        assert_eq!(
            std::env::var("GSETTINGS_BACKEND").as_deref(),
            Ok("memory"),
            "run this exact case with GSETTINGS_BACKEND=memory; do not touch personal dconf"
        );
    }

    fn set_identity(source: &gio::SettingsSchemaSource, uuid: &str) {
        let list_schema = source
            .lookup(PROFILES_LIST_SCHEMA, true)
            .expect("ProfilesList schema");
        let list = gio::Settings::new_full(&list_schema, None::<&gio::SettingsBackend>, None);
        list.set_string("default", uuid).expect("default uuid");
        list.set_strv("list", [uuid]).expect("profile list");
    }

    fn profile(source: &gio::SettingsSchemaSource, uuid: &str) -> gio::Settings {
        let schema = source.lookup(PROFILE_SCHEMA, true).expect("Profile schema");
        gio::Settings::new_full(
            &schema,
            None::<&gio::SettingsBackend>,
            Some(profile_path(uuid).as_str()),
        )
    }

    fn palette() -> [&'static str; 16] {
        [
            "#000000", "#111111", "#222222", "#333333", "#444444", "#555555", "#666666", "#777777",
            "#888888", "#999999", "#aaaaaa", "#bbbbbb", "#cccccc", "#dddddd", "#eeeeee", "#ffffff",
        ]
    }

    fn palette_15() -> [&'static str; 15] {
        [
            "#000000", "#111111", "#222222", "#333333", "#444444", "#555555", "#666666", "#777777",
            "#888888", "#999999", "#aaaaaa", "#bbbbbb", "#cccccc", "#dddddd", "#eeeeee",
        ]
    }

    #[test]
    #[ignore = "requires glib-compile-schemas, GSETTINGS_BACKEND=memory and a private schema source"]
    fn guarded_source_preserves_custom_pair_and_palette() {
        let fixture = schema_fixture("base");
        require_memory_backend();
        let uuid = "01234567-89ab-cdef-0123-456789abcdef";
        set_identity(&fixture.source, uuid);
        let profile = profile(&fixture.source, uuid);
        profile.set_string("foreground-color", "#123456").unwrap();
        profile.set_string("background-color", "#abcdef").unwrap();
        profile.set_boolean("use-theme-colors", false).unwrap();
        profile.set_strv("palette", palette()).unwrap();

        let snapshot = snapshot_from(&fixture.source, None);
        let pair = snapshot.text_background().expect("explicit source pair");
        assert_eq!(pair.foreground, parse_colour("#123456").unwrap());
        assert_eq!(pair.background, parse_colour("#abcdef").unwrap());
        let colours = snapshot.ansi_palette().expect("valid source palette");
        assert_eq!(colours[0], parse_colour("#000000").unwrap());
        assert_eq!(colours[15], parse_colour("#ffffff").unwrap());
    }

    #[test]
    #[ignore = "requires glib-compile-schemas, GSETTINGS_BACKEND=memory and a private schema source"]
    fn theme_text_failure_keeps_the_valid_palette() {
        let fixture = schema_fixture("base");
        require_memory_backend();
        let uuid = "01234567-89ab-cdef-0123-456789abcdef";
        set_identity(&fixture.source, uuid);
        let profile = profile(&fixture.source, uuid);
        profile.set_boolean("use-theme-colors", true).unwrap();
        profile.set_strv("palette", palette()).unwrap();

        let snapshot = snapshot_from(&fixture.source, None);
        assert_eq!(snapshot.text_background(), Err(SourceFailure::ThemeDerived));
        assert!(snapshot.ansi_palette().is_ok());
    }

    #[test]
    #[ignore = "requires glib-compile-schemas, GSETTINGS_BACKEND=memory and a private schema source"]
    fn invalid_palette_does_not_hide_a_valid_text_pair() {
        let fixture = schema_fixture("base");
        require_memory_backend();
        let uuid = "01234567-89ab-cdef-0123-456789abcdef";
        set_identity(&fixture.source, uuid);
        let profile = profile(&fixture.source, uuid);
        profile.set_string("foreground-color", "#123456").unwrap();
        profile.set_string("background-color", "#abcdef").unwrap();
        profile.set_boolean("use-theme-colors", false).unwrap();
        profile.set_strv("palette", palette_15()).unwrap();

        let snapshot = snapshot_from(&fixture.source, None);
        assert!(snapshot.text_background().is_ok());
        assert_eq!(snapshot.ansi_palette(), Err(SourceFailure::InvalidColour));
    }

    #[test]
    #[ignore = "requires glib-compile-schemas, GSETTINGS_BACKEND=memory and a private schema source"]
    fn wrong_palette_key_type_does_not_hide_a_valid_text_pair() {
        let fixture = schema_fixture("profile-palette-type");
        require_memory_backend();
        let uuid = "01234567-89ab-cdef-0123-456789abcdef";
        set_identity(&fixture.source, uuid);
        let profile = profile(&fixture.source, uuid);
        profile.set_string("foreground-color", "#123456").unwrap();
        profile.set_string("background-color", "#abcdef").unwrap();
        profile.set_boolean("use-theme-colors", false).unwrap();

        let snapshot = snapshot_from(&fixture.source, None);
        assert!(snapshot.text_background().is_ok());
        assert_eq!(snapshot.ansi_palette(), Err(SourceFailure::MissingSchema));
    }

    #[test]
    #[ignore = "requires glib-compile-schemas, GSETTINGS_BACKEND=memory and a private schema source"]
    fn missing_theme_key_does_not_hide_a_valid_palette() {
        let fixture = schema_fixture("profile-missing-theme");
        require_memory_backend();
        let uuid = "01234567-89ab-cdef-0123-456789abcdef";
        set_identity(&fixture.source, uuid);
        let profile = profile(&fixture.source, uuid);
        profile.set_strv("palette", palette()).unwrap();

        let snapshot = snapshot_from(&fixture.source, None);
        assert_eq!(
            snapshot.text_background(),
            Err(SourceFailure::MissingSchema)
        );
        assert!(snapshot.ansi_palette().is_ok());
    }

    #[test]
    #[ignore = "requires glib-compile-schemas, GSETTINGS_BACKEND=memory and a private schema source"]
    fn malformed_profile_list_path_or_identity_fails_both_dimensions() {
        for name in ["profileslist-missing-default", "profileslist-wrong-path"] {
            let fixture = schema_fixture(name);
            let snapshot = snapshot_from(&fixture.source, None);
            assert_eq!(
                snapshot.text_background(),
                Err(SourceFailure::MissingSchema),
                "{name}"
            );
            assert_eq!(
                snapshot.ansi_palette(),
                Err(SourceFailure::MissingSchema),
                "{name}"
            );
        }
    }

    #[test]
    fn fallback_snapshot_reports_both_dimensions_unavailable() {
        let fallback = fallback_snapshot();
        assert_eq!(fallback.text_background(), Err(SourceFailure::Unreadable));
        assert_eq!(fallback.ansi_palette(), Err(SourceFailure::Unreadable));
    }

    #[test]
    fn uuid_shape_validation_accepts_terminal_ids_and_rejects_malformed_values() {
        assert!(is_uuid("01234567-89ab-cdef-0123-456789abcdef"));
        assert!(!is_uuid(""));
        assert!(!is_uuid("01234567-89ab-cdef-0123-456789abcde"));
        assert!(!is_uuid("01234567-89ab-cdef-0123-456789abcdeg"));
        assert!(!is_uuid("0123456789abcdef0123456789abcdef0000"));
    }

    #[test]
    fn profile_path_uses_the_terminal_relocatable_root() {
        assert_eq!(
            profile_path("01234567-89ab-cdef-0123-456789abcdef"),
            "/org/gnome/terminal/legacy/profiles:/:01234567-89ab-cdef-0123-456789abcdef/"
        );
    }

    #[test]
    fn colour_parser_preserves_alpha_and_rejects_invalid_values() {
        let colour = parse_colour("#3465a4").expect("valid colour");
        assert!((colour.red() - 0.203_921_57).abs() < 1e-6);
        assert!((colour.green() - 0.396_078_44).abs() < 1e-6);
        assert!((colour.blue() - 0.643_137_3).abs() < 1e-6);
        assert_eq!(colour.alpha(), 1.0);
        assert_eq!(
            parse_colour("not a colour"),
            Err(SourceFailure::InvalidColour)
        );
    }
}
