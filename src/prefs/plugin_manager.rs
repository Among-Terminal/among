//! rows retain identity and widget instances while observations/search change.
//! package-generation replacement invalidates old controls instead of retargeting them.

use super::*;
use crate::plugin::host::Overview;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

// gtk owns the widgets; the refresh index must not prolong a closed page's life.
struct Row {
    widget: glib::WeakRef<adw::ActionRow>,
    switch: glib::WeakRef<gtk::Switch>,
    updating: Rc<Cell<bool>>,
    generation: Option<String>,
}

impl Row {
    fn refresh(&self, info: &Overview, query: &str) {
        let (Some(widget), Some(switch)) = (self.widget.upgrade(), self.switch.upgrade()) else {
            return;
        };
        widget.set_title(&escape_display(&info.name));
        widget.set_subtitle(&escape_display(&format!(
            "{}\n{}",
            info.metadata(),
            describe_state(&info.state)
        )));
        widget.set_visible(info.search_text().contains(query));
        switch.update_property(&[gtk::accessible::Property::Label(&format!(
            "Enable {} ({})",
            info.name, info.key
        ))]);
        switch.set_sensitive(info.enabled || info.available);
        // even setting the same active value cancels GTK's keyboard-toggle
        // animation before its active-notify can commit the user's choice.
        if switch.is_active() != info.enabled {
            self.updating.set(true);
            switch.set_active(info.enabled);
            self.updating.set(false);
        }
    }
}

pub(super) fn page(ctx: &PreferencesContext) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Plugins");
    page.set_icon_name(Some("application-x-addon-symbolic"));
    add_storage_status(&page, ctx);
    let navigation = adw::PreferencesGroup::new();
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search plugins"));
    search.update_property(&[gtk::accessible::Property::Label(
        "Search plugins by name, identity or origin",
    )]);
    navigation.add(&search);
    page.add(&navigation);
    let group = adw::PreferencesGroup::new();
    group.set_title("Installed and configured");
    group.set_description(Some("Inspection does not start code. Plugins run as your user, not in an OS sandbox. Switches show desired enablement, separately from runtime state."));
    page.add(&group);
    let empty = adw::ActionRow::new();
    empty.set_title("No matching plugins");
    empty.set_subtitle(
        "Configured missing files remain listed. Newly discovered executables start disabled.",
    );
    group.add(&empty);
    let rows = Rc::new(RefCell::new(BTreeMap::new()));
    reconcile(ctx, &group, &empty, &rows, "");
    search.connect_search_changed({
        let ctx = ctx.clone();
        let group = group.downgrade();
        let empty = empty.downgrade();
        let rows = rows.clone();
        move |search| {
            if let (Some(group), Some(empty)) = (group.upgrade(), empty.upgrade()) {
                reconcile(&ctx, &group, &empty, &rows, &search.text().to_lowercase());
            }
        }
    });
    let group = group.downgrade();
    let empty = empty.downgrade();
    let search = search.downgrade();
    let ctx = ctx.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
        let (Some(group), Some(empty), Some(search)) =
            (group.upgrade(), empty.upgrade(), search.upgrade())
        else {
            return glib::ControlFlow::Break;
        };
        reconcile(&ctx, &group, &empty, &rows, &search.text().to_lowercase());
        glib::ControlFlow::Continue
    });
    page
}

fn reconcile(
    ctx: &PreferencesContext,
    group: &adw::PreferencesGroup,
    empty: &adw::ActionRow,
    rows: &RefCell<BTreeMap<String, Row>>,
    query: &str,
) {
    let mut inventory = (ctx.plugin_inventory)();
    inventory.sort_by(|first, second| {
        first
            .name
            .cmp(&second.name)
            .then_with(|| first.key.cmp(&second.key))
    });
    let live: BTreeSet<_> = inventory.iter().map(|info| info.key.as_str()).collect();
    let mut rows = rows.borrow_mut();
    rows.retain(|key, row| {
        if live.contains(key.as_str()) {
            true
        } else {
            if let Some(widget) = row.widget.upgrade() {
                group.remove(&widget);
            }
            false
        }
    });
    for info in &inventory {
        if rows
            .get(&info.key)
            .is_some_and(|row| row.generation != info.generation || row.widget.upgrade().is_none())
        {
            if let Some(row) = rows.remove(&info.key) {
                if let Some(widget) = row.widget.upgrade() {
                    group.remove(&widget);
                }
            }
        }
        let row = rows
            .entry(info.key.clone())
            .or_insert_with(|| make_row(ctx, group, info));
        row.refresh(info, query);
    }
    empty.set_visible(!rows.values().any(|row| {
        row.widget
            .upgrade()
            .is_some_and(|widget| widget.is_visible())
    }));
}

fn make_row(ctx: &PreferencesContext, group: &adw::PreferencesGroup, info: &Overview) -> Row {
    let row = adw::ActionRow::new();
    row.set_subtitle_lines(0);
    row.set_title_lines(1);
    if let Some((entry, _)) = (ctx.plugin_details)(&info.key) {
        plugin_settings::add_actions(
            &row,
            entry,
            ctx.save_plugin_settings.clone(),
            ctx.plugin_details.clone(),
            ctx.plugin_configuration.clone(),
            ctx.restart_plugin.clone(),
        );
    } else {
        let icon = gtk::Image::from_icon_name("application-x-addon-symbolic");
        icon.set_pixel_size(32);
        row.add_prefix(&icon);
        let details = gtk::Button::with_label("Details…");
        details.set_valign(gtk::Align::Center);
        details.update_property(&[gtk::accessible::Property::Label(&format!(
            "Details for {} ({})",
            info.name, info.key
        ))]);
        let key = info.key.clone();
        let inventory = ctx.plugin_inventory.clone();
        details.connect_clicked(move |button| {
            let Some(info) = inventory().into_iter().find(|info| info.key == key) else {
                return;
            };
            let dialog = adw::AlertDialog::builder()
                .heading(&info.name)
                .body(format!(
                    "Identity: {}\n{}\n{}",
                    info.key,
                    info.metadata(),
                    describe_state(&info.state)
                ))
                .build();
            dialog.add_response("back", "Back");
            dialog.set_close_response("back");
            dialog.present(
                button
                    .root()
                    .and_then(|root| root.downcast::<gtk::Window>().ok())
                    .as_ref(),
            );
        });
        row.add_suffix(&details);
    }
    let switch = gtk::Switch::new();
    switch.set_valign(gtk::Align::Center);
    switch.set_active(info.enabled);
    let updating = Rc::new(Cell::new(false));
    let signal = updating.clone();
    let enable = ctx.enable_plugin.clone();
    let disable = ctx.disable_plugin.clone();
    let key = info.key.clone();
    let listing = ctx.plugin_inventory.clone();
    let weak_row = row.downgrade();
    switch.connect_active_notify(move |switch| {
        if signal.get() {
            return;
        }
        if switch.is_active() {
            enable(
                &key,
                switch
                    .root()
                    .and_then(|root| root.downcast::<gtk::Window>().ok()),
            );
        } else {
            disable(&key);
        }
        if let (Some(row), Some(info)) = (
            weak_row.upgrade(),
            listing().into_iter().find(|info| info.key == key),
        ) {
            row.set_subtitle(&escape_display(&format!(
                "{}\n{}",
                info.metadata(),
                describe_state(&info.state)
            )));
        }
    });
    row.add_suffix(&switch);
    row.set_activatable_widget(Some(&switch));
    group.add(&row);
    Row {
        widget: row.downgrade(),
        switch: switch.downgrade(),
        updating,
        generation: info.generation.clone(),
    }
}
