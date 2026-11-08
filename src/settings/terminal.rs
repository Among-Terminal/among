use serde_json::{json, Value};

use crate::terminal_appearance::{AnsiPaletteId, StoredAppearance, TextBackgroundId};

use super::TerminalEdit;

pub(super) fn validate(value: &Value) -> Result<(), String> {
    if value.get("version").and_then(Value::as_u64) != Some(1) {
        return Err("unsupported terminal appearance document version".into());
    }
    if let Some(id) = value.get("text_background") {
        let id = id.as_str().ok_or("text_background must be a string")?;
        if id.parse::<TextBackgroundId>().is_err() {
            return Err(format!("unknown text_background preset: {id}"));
        }
    }
    if let Some(id) = value.get("ansi_palette") {
        let id = id.as_str().ok_or("ansi_palette must be a string")?;
        if id.parse::<AnsiPaletteId>().is_err() {
            return Err(format!("unknown ansi_palette preset: {id}"));
        }
    }
    Ok(())
}

/// Read a validated document. Missing fields become the GNOME default source.
pub(super) fn stored(value: &Value) -> StoredAppearance {
    let mut stored = StoredAppearance::default();
    if let Some(id) = value.get("text_background").and_then(Value::as_str) {
        if let Ok(id) = id.parse() {
            stored.text_background = id;
        }
    }
    if let Some(id) = value.get("ansi_palette").and_then(Value::as_str) {
        if let Ok(id) = id.parse() {
            stored.ansi_palette = id;
        }
    }
    stored
}

pub(super) fn edit_dimension(value: &mut Value, edit: TerminalEdit) {
    match edit {
        TerminalEdit::TextBackground(id) => {
            value["text_background"] = json!(id.as_str());
        }
        TerminalEdit::AnsiPalette(id) => {
            value["ansi_palette"] = json!(id.as_str());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_fields_are_the_gnome_default_source() {
        let stored = stored(&json!({"version": 1}));
        assert_eq!(stored, StoredAppearance::default());
        assert_eq!(
            stored.text_background,
            TextBackgroundId::GnomeTerminalDefault
        );
        assert_eq!(stored.ansi_palette, AnsiPaletteId::GnomeTerminalDefault);
    }

    #[test]
    fn known_ids_round_trip_and_unknown_ids_are_rejected() {
        let document = json!({
            "version": 1,
            "text_background": "solarized-dark",
            "ansi_palette": "linux-console",
            "foreign": {"keep": true}
        });
        assert_eq!(validate(&document), Ok(()));
        let parsed = stored(&document);
        assert_eq!(parsed.text_background, TextBackgroundId::SolarizedDark);
        assert_eq!(parsed.ansi_palette, AnsiPaletteId::LinuxConsole);

        let mut edited = document;
        edit_dimension(
            &mut edited,
            TerminalEdit::TextBackground(TextBackgroundId::GrayOnBlack),
        );
        edit_dimension(&mut edited, TerminalEdit::AnsiPalette(AnsiPaletteId::Tango));
        assert_eq!(edited["text_background"], "gray-on-black");
        assert_eq!(edited["ansi_palette"], "tango");
        assert_eq!(edited["foreign"], json!({"keep": true}));
    }

    #[test]
    fn future_version_wrong_type_and_unknown_id_are_errors() {
        assert!(validate(&json!({"version": 2})).is_err());
        assert!(validate(&json!({"version": 1, "text_background": 4})).is_err());
        assert!(validate(&json!({"version": 1, "ansi_palette": "not-a-palette"})).is_err());
    }
}
