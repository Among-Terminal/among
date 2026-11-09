//! Terminal appearance preferences page.

use super::*;

use crate::terminal_appearance::{
    AnsiPaletteId, TextBackgroundId, ANSI_PALETTE_CATALOGUE, TEXT_BACKGROUND_CATALOGUE,
};

/// The source and fallback state shown beside the two dropdowns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalView {
    pub text_background: TextBackgroundId,
    pub ansi_palette: AnsiPaletteId,
    pub text_background_note: String,
    pub palette_note: String,
}

#[derive(Clone)]
pub(crate) struct Context {
    pub view: TerminalView,
    pub text_background_changed: Rc<dyn Fn(TextBackgroundId) -> TerminalView>,
    pub ansi_palette_changed: Rc<dyn Fn(AnsiPaletteId) -> TerminalView>,
}

pub(crate) fn page(ctx: &PreferencesContext) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    page.set_name(Some("terminal"));
    page.set_title("Terminal");
    page.set_icon_name(Some("utilities-terminal-symbolic"));

    let appearance = adw::PreferencesGroup::new();
    appearance.set_title("Appearance");
    appearance.set_description(Some(
        "Choose default text and background separately from the 16 ANSI colours.",
    ));

    let (text_model, text_ids) = text_options();
    let text_row = adw::ComboRow::builder()
        .title("Text and background")
        .model(&text_model)
        .build();
    text_row.set_widget_name("terminal-text-background");
    text_row.set_subtitle(&ctx.terminal.view.text_background_note);
    text_row.set_selected(
        text_ids
            .iter()
            .position(|id| *id == ctx.terminal.view.text_background)
            .unwrap_or(0) as u32,
    );

    let (palette_model, palette_ids) = palette_options();
    let palette_row = adw::ComboRow::builder()
        .title("ANSI palette")
        .model(&palette_model)
        .build();
    palette_row.set_widget_name("terminal-ansi-palette");
    palette_row.set_subtitle(&ctx.terminal.view.palette_note);
    palette_row.set_selected(
        palette_ids
            .iter()
            .position(|id| *id == ctx.terminal.view.ansi_palette)
            .unwrap_or(0) as u32,
    );

    // Connect only after the initial selection, so opening the page cannot
    // look like a user edit and cannot create terminal.json.
    {
        let ids = text_ids;
        let change = ctx.terminal.text_background_changed.clone();
        let row = text_row.downgrade();
        let palette_row = palette_row.downgrade();
        text_row.connect_selected_notify(move |selected| {
            let index = selected.selected() as usize;
            let Some(id) = ids.get(index).copied() else {
                return;
            };
            let view = change(id);
            if let Some(row) = row.upgrade() {
                row.set_subtitle(&view.text_background_note);
            }
            if let Some(row) = palette_row.upgrade() {
                row.set_subtitle(&view.palette_note);
            }
        });
    }
    {
        let ids = palette_ids;
        let change = ctx.terminal.ansi_palette_changed.clone();
        let row = palette_row.downgrade();
        let text_row = text_row.downgrade();
        palette_row.connect_selected_notify(move |selected| {
            let index = selected.selected() as usize;
            let Some(id) = ids.get(index).copied() else {
                return;
            };
            let view = change(id);
            if let Some(row) = row.upgrade() {
                row.set_subtitle(&view.palette_note);
            }
            if let Some(row) = text_row.upgrade() {
                row.set_subtitle(&view.text_background_note);
            }
        });
    }

    appearance.add(&text_row);
    appearance.add(&palette_row);
    page.add(&appearance);
    page
}

fn text_options() -> (gtk::StringList, Vec<TextBackgroundId>) {
    let mut ids = vec![TextBackgroundId::GnomeTerminalDefault];
    let mut labels = vec!["GNOME Terminal default".to_string()];
    for preset in TEXT_BACKGROUND_CATALOGUE.iter() {
        ids.push(preset.id);
        labels.push(preset.label.to_string());
    }
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    (gtk::StringList::new(&labels), ids)
}

fn palette_options() -> (gtk::StringList, Vec<AnsiPaletteId>) {
    let mut ids = vec![AnsiPaletteId::GnomeTerminalDefault];
    let mut labels = vec!["GNOME Terminal default".to_string()];
    for preset in ANSI_PALETTE_CATALOGUE.iter() {
        ids.push(preset.id);
        labels.push(preset.label.to_string());
    }
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    (gtk::StringList::new(&labels), ids)
}

/// Keep a source row's custom state explicit without inventing a preset name.
pub(crate) fn custom_source_note() -> &'static str {
    "GNOME Terminal default (custom colours)"
}

/// Used by test-only PreferencesContext constructors.
#[cfg(test)]
pub(crate) fn test_context() -> Context {
    let view = TerminalView {
        text_background: TextBackgroundId::GnomeTerminalDefault,
        ansi_palette: AnsiPaletteId::GnomeTerminalDefault,
        text_background_note: String::new(),
        palette_note: String::new(),
    };
    let text_view = view.clone();
    let palette_view = view.clone();
    Context {
        view,
        text_background_changed: Rc::new(move |_| text_view.clone()),
        ansi_palette_changed: Rc::new(move |_| palette_view.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_appearance::{ansi_palette_preset, text_background_preset};

    #[test]
    #[ignore = "requires an isolated GTK display"]
    fn catalogues_add_the_source_row_before_each_independent_list() {
        adw::init().expect("isolated GTK display");
        let (text, text_ids) = text_options();
        assert_eq!(text.n_items(), 12);
        assert_eq!(text_ids[0], TextBackgroundId::GnomeTerminalDefault);
        assert_eq!(text.string(1).as_deref(), Some("Black on light yellow"));

        let (palette, palette_ids) = palette_options();
        assert_eq!(palette.n_items(), 7);
        assert_eq!(palette_ids[0], AnsiPaletteId::GnomeTerminalDefault);
        assert_eq!(palette.string(1).as_deref(), Some("GNOME"));

        assert!(text_background_preset(TextBackgroundId::GrayOnBlack).is_some());
        assert!(ansi_palette_preset(AnsiPaletteId::Tango).is_some());
    }
}
