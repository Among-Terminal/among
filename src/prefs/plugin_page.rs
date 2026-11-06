//! the plugin page, opened from the info control as a subpage of the
//! Preferences window. it tells an ordinary user what the plugin is, whether it
//! is switched on and what it may read; package facts such as digests appear
//! only in development mode, and a collapsed Technical details holds the rest.

use super::plugin_manager::{Env, Op};
use super::plugin_row::{Slot, SlotWidgets};
use super::trust::{self, Shield};
use super::*;
use crate::plugin::host::Overview;
use crate::plugin::install::listing::{Link, Standing};
use crate::plugin::install::mutation::Action;
use crate::plugin::inventory::{Entry, LOCAL_MISSING_PROVENANCE, LOCAL_SOURCE_LABEL};

/// The package facts shown under Technical details, as title and value. The
/// short set is for every run; digests, release and commit identities are for
/// whoever builds plugins and so appear only in development mode.
pub(super) fn technical_rows(
    entry: &Entry,
    info: Option<&Overview>,
    development: bool,
) -> Vec<(&'static str, String)> {
    let mut rows = vec![("Identity", entry.key())];
    match entry.receipt.release() {
        Some(release) => rows.push((
            "GitHub repository",
            format!("{}/{}", release.owner, release.repository),
        )),
        None => rows.push(("Source", LOCAL_SOURCE_LABEL.to_owned())),
    }
    if let Some(info) = info {
        rows.push(("API compatibility", info.declared.clone()));
    }
    if let Some(problem) = entry
        .inspected
        .as_ref()
        .ok()
        .and_then(|inspected| inspected.icon_problem)
    {
        rows.push(("Artwork", problem.to_string()));
    }
    if !development {
        return rows;
    }
    if let Some(release) = entry.receipt.release() {
        rows.push(("Repository id", release.repository_id.to_string()));
        match release.source.as_ref() {
            Some(source) => {
                rows.push(("Publisher id", source.publisher_id.to_string()));
                rows.push(("Release tag", source.release_binding.tag.clone()));
                rows.push(("Release commit", source.release_binding.commit.clone()));
                rows.push(("Catalogue revision", source.catalogue_revision.clone()));
                rows.push(("Catalogue SHA-256", source.catalogue_sha256.clone()));
            }
            None => rows.push((
                "Publisher id",
                "Unconfirmed in this pre-installer receipt; an update requires renewed review"
                    .to_owned(),
            )),
        }
        rows.push((
            "Release and asset",
            format!("{} / {}", release.release_id, release.asset_id),
        ));
    }
    if let Some(local) = entry.receipt.local() {
        rows.push(("Provenance", LOCAL_MISSING_PROVENANCE.to_owned()));
        rows.push(("Local package file", local.asset.clone()));
        rows.push(("Local catalogue SHA-256", local.catalogue_sha256.clone()));
    }
    rows.push(("Version commit", entry.receipt.revision.clone()));
    rows.push(("Package SHA-256", entry.receipt.package_sha256.clone()));
    rows.push(("Manifest SHA-256", entry.receipt.manifest_sha256.clone()));
    rows.push(("Generation", entry.receipt.generation.clone()));
    if let Ok(inspected) = &entry.inspected {
        rows.push(("Entrypoint", inspected.executable.display().to_string()));
    }
    if let Some(details) = info.and_then(|info| info.protection_details.as_ref()) {
        rows.push(("Protection at last start", details.clone()));
    }
    rows
}

fn technical_row(value: &str, title: &str) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(title);
    row.set_subtitle(&escape_display(if value.is_empty() {
        "None declared"
    } else {
        value
    }));
    row.set_subtitle_lines(0);
    row.set_subtitle_selectable(true);
    row
}

/// What a page shows that changes while it is open.
struct Live {
    banner: adw::Banner,
    head: adw::ActionRow,
    slot: SlotWidgets,
    switch: adw::SwitchRow,
    updating: Rc<std::cell::Cell<bool>>,
    protection: adw::ActionRow,
    allow: gtk::Button,
    update_note: adw::ActionRow,
    status: adw::ActionRow,
    description: String,
    name: String,
}

fn window_of(widget: &impl IsA<gtk::Widget>) -> Option<adw::PreferencesWindow> {
    widget
        .root()
        .and_then(|root| root.downcast::<adw::PreferencesWindow>().ok())
}

fn current(env: &Env, key: &str) -> Option<(Overview, Entry)> {
    let info = (env.ctx.plugin_inventory)()
        .into_iter()
        .find(|info| info.key == key)?;
    let (entry, _) = (env.ctx.plugin_details)(key)?;
    Some((info, entry))
}

fn standing_of(env: &Env, key: &str) -> Option<Standing> {
    env.state
        .listing
        .borrow()
        .installed
        .iter()
        .find(|plugin| plugin.key == key)
        .map(|plugin| plugin.standing.clone())
}

fn link_of(env: &Env, key: &str) -> Option<Link> {
    env.state
        .listing
        .borrow()
        .installed
        .iter()
        .find(|plugin| plugin.key == key)
        .map(|plugin| plugin.link.clone())
}

fn update_text(standing: Option<&Standing>) -> String {
    match standing {
        Some(Standing::Update(selection)) => {
            format!("Version {} is available.", selection.manifest().version)
        }
        Some(Standing::Replace(selection)) => format!(
            "Pre-release {} can replace this version.",
            selection.manifest().version
        ),
        Some(Standing::Current) => "This is the newest version the saved list offers.".into(),
        Some(Standing::NotOffered) => {
            "The saved list of its source does not offer this plugin.".into()
        }
        Some(Standing::Withheld(problem)) => format!("No update is offered: {problem}"),
        Some(Standing::Unknown) | None => "No saved list of its source is available.".into(),
    }
}

/// Opens the plugin page for an installed plugin as a subpage of `window`.
pub(super) fn open(env: &Rc<Env>, key: &str, window: &adw::PreferencesWindow) {
    let Some((info, entry)) = current(env, key) else {
        // a plugin without package files has no page: say what is known.
        let Some(info) = (env.ctx.plugin_inventory)()
            .into_iter()
            .find(|info| info.key == key)
        else {
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
        dialog.present(Some(window));
        return;
    };
    let nav = adw::NavigationPage::new(&gtk::Box::new(gtk::Orientation::Vertical, 0), &info.name);
    let shown = Rc::new(RefCell::new((
        entry.receipt.clone(),
        build(env, key, &nav, &info, &entry),
    )));
    window.push_subpage(&nav);
    let env = env.clone();
    let key = key.to_owned();
    let window = window.downgrade();
    let nav = nav.downgrade();
    glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        let (Some(window), Some(nav)) = (window.upgrade(), nav.upgrade()) else {
            return glib::ControlFlow::Break;
        };
        // going back removes the page from the window; nothing more to keep up to date.
        if nav.parent().is_none() {
            return glib::ControlFlow::Break;
        }
        let Some((info, entry)) = current(&env, &key) else {
            // removal committed: the page of a plugin that is gone closes and
            // focus returns to the list.
            while nav.parent().is_some() && window.pop_subpage() {}
            let focus = env.focus.borrow().clone();
            if let Some(focus) = focus {
                focus(&key);
            }
            return glib::ControlFlow::Break;
        };
        let mut shown = shown.borrow_mut();
        if shown.0 != entry.receipt {
            // another generation was selected: rebuild from what is installed now.
            shown.0 = entry.receipt.clone();
            shown.1 = build(&env, &key, &nav, &info, &entry);
        }
        update(&env, &key, &shown.1, &info);
        glib::ControlFlow::Continue
    });
}

/// Opens the settings form of an installed plugin as a further subpage.
pub(super) fn open_settings(env: &Rc<Env>, key: &str, window: &adw::PreferencesWindow) {
    let Some((entry, stored)) = (env.ctx.plugin_details)(key) else {
        return;
    };
    let Some(inspected) = entry.inspected.as_ref().ok() else {
        return;
    };
    let Some(schema) = inspected.manifest.settings.clone() else {
        return;
    };
    let name = inspected.manifest.name.clone();
    let page = plugin_settings::settings_subpage(
        entry,
        stored,
        schema,
        name,
        plugin_settings::Form {
            save: env.ctx.save_plugin_settings.clone(),
            load: env.ctx.plugin_details.clone(),
            status: env.ctx.plugin_configuration.clone(),
            restart: env.ctx.restart_plugin.clone(),
        },
    );
    window.push_subpage(&page);
}

fn build(
    env: &Rc<Env>,
    key: &str,
    nav: &adw::NavigationPage,
    info: &Overview,
    entry: &Entry,
) -> Live {
    let development = crate::plugin::development::enabled();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let banner = adw::Banner::new("");
    banner.set_button_label(Some("Dismiss"));
    banner.connect_button_clicked({
        let env = env.clone();
        let key = key.to_owned();
        move |_| env.clear_note(&key)
    });
    toolbar.add_top_bar(&banner);
    let page = adw::PreferencesPage::new();

    let head_group = adw::PreferencesGroup::new();
    let head = adw::ActionRow::new();
    head.set_title_lines(1);
    head.set_subtitle_lines(0);
    head.add_prefix(&plugin_settings::entry_icon(entry, 48));
    let slot = SlotWidgets::new();
    head.add_suffix(&slot.stack);
    head_group.add(&head);
    page.add(&head_group);
    slot.install.set_visible(false);
    slot.update.connect_clicked({
        let env = env.clone();
        let key = key.to_owned();
        move |button| super::plugin_manager::update(&env, &key, window_of(button))
    });
    slot.uninstall.connect_clicked({
        let env = env.clone();
        let key = key.to_owned();
        move |button| {
            super::plugin_manager::uninstall(
                &env,
                &key,
                button
                    .root()
                    .and_then(|root| root.downcast::<gtk::Window>().ok()),
            )
        }
    });

    let origin = trust::Origin::of_installed(entry);
    let features = entry
        .inspected
        .as_ref()
        .map(|inspected| inspected.manifest.required_features.clone())
        .unwrap_or_default();
    let shield = Shield::of(origin, info.weaker_protection);
    let cards = adw::PreferencesGroup::new();
    let source = adw::ActionRow::new();
    source.set_title("Source");
    source.set_subtitle_lines(0);
    source.add_prefix(&gtk::Image::from_icon_name(
        Shield::of(origin, false).icon_name(),
    ));
    let mut source_text = format!(
        "{}\n{}",
        Shield::of(origin, false).title(),
        Shield::of(origin, false).detail()
    );
    match link_of(env, key) {
        Some(Link::SourceRemoved {
            owner, repository, ..
        }) => source_text.push_str(&format!(
            "\nPublished from {owner}/{repository}, which is no longer a saved source. The plugin keeps working; it only cannot be updated."
        )),
        Some(Link::Source(source)) if !trust::Origin::of_installed(entry).eq(&trust::Origin::Official) => {
            source_text.push_str(&format!("\nFrom {}/{}.", source.owner, source.repository))
        }
        _ => {}
    }
    source.set_subtitle(&escape_display(&source_text));
    cards.add(&source);
    let protection = adw::ActionRow::new();
    protection.set_title("Protection");
    protection.set_subtitle_lines(0);
    protection.add_prefix(&gtk::Image::from_icon_name(shield.icon_name()));
    let allow = gtk::Button::with_label("Allow reduced protection…");
    allow.set_valign(gtk::Align::Center);
    allow.connect_clicked({
        let env = env.clone();
        let key = key.to_owned();
        move |button| super::plugin_manager::allow_reduced(&env, &key, button)
    });
    protection.add_suffix(&allow);
    cards.add(&protection);
    let access = adw::ActionRow::new();
    access.set_title("Can read");
    access.set_subtitle_lines(0);
    access.add_prefix(&gtk::Image::from_icon_name("dialog-information-symbolic"));
    access.set_subtitle(&escape_display(&capitalized(&trust::access_summary(
        &features,
    ))));
    cards.add(&access);
    page.add(&cards);

    let controls = adw::PreferencesGroup::new();
    let status = adw::ActionRow::new();
    status.set_title("Status");
    status.set_subtitle_lines(0);
    controls.add(&status);
    let switch = adw::SwitchRow::new();
    switch.set_title("Enabled");
    controls.add(&switch);
    let updating = Rc::new(std::cell::Cell::new(false));
    switch.connect_active_notify({
        let env = env.clone();
        let key = key.to_owned();
        let signal = updating.clone();
        let generation = info.generation.clone();
        move |switch| {
            if signal.get() {
                return;
            }
            if (env.ctx.plugin_inventory)()
                .into_iter()
                .find(|info| info.key == key)
                .is_none_or(|info| info.generation != generation)
            {
                return;
            }
            env.clear_note(&key);
            if switch.is_active() {
                (env.ctx.enable_plugin)(
                    &key,
                    switch
                        .root()
                        .and_then(|root| root.downcast::<gtk::Window>().ok()),
                );
            } else {
                (env.ctx.disable_plugin)(&key);
            }
        }
    });
    if entry
        .inspected
        .as_ref()
        .is_ok_and(|inspected| inspected.manifest.settings.is_some())
    {
        let settings = adw::ActionRow::new();
        settings.set_title("Settings");
        settings.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        settings.set_activatable(true);
        settings.connect_activated({
            let env = env.clone();
            let key = key.to_owned();
            move |row| {
                if let Some(window) = window_of(row) {
                    open_settings(&env, &key, &window);
                }
            }
        });
        controls.add(&settings);
    }
    page.add(&controls);

    let details = adw::PreferencesGroup::new();
    let expander = adw::ExpanderRow::new();
    expander.set_title("Technical details");
    expander.set_expanded(false);
    let update_note = technical_row("", "Updates");
    expander.add_row(&update_note);
    for (title, value) in technical_rows(entry, Some(info), development) {
        expander.add_row(&technical_row(&value, title));
    }
    if development && entry.previous.is_some() {
        let recover = adw::ActionRow::new();
        recover.set_title("Recover previous");
        recover.set_subtitle("Select the retained previous verified generation.");
        let button = gtk::Button::with_label("Recover previous…");
        button.set_valign(gtk::Align::Center);
        button.connect_clicked({
            let env = env.clone();
            let key = key.to_owned();
            move |button| {
                super::plugin_manager::package_action(
                    &env,
                    &key,
                    Action::RestorePrevious,
                    button
                        .root()
                        .and_then(|root| root.downcast::<gtk::Window>().ok()),
                )
            }
        });
        recover.add_suffix(&button);
        recover.set_activatable_widget(Some(&button));
        expander.add_row(&recover);
    }
    details.add(&expander);
    page.add(&details);

    toolbar.set_content(Some(&page));
    nav.set_child(Some(&toolbar));
    nav.set_title(&info.name);
    head.set_title(&version_title(&info.name, info.version.as_deref()));
    Live {
        banner,
        head,
        slot,
        switch,
        updating,
        protection,
        allow,
        update_note,
        status,
        description: info.description.clone(),
        name: info.name.clone(),
    }
}

fn capitalized(text: &str) -> String {
    let mut characters = text.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

fn version_title(name: &str, version: Option<&str>) -> String {
    match version {
        Some(version) => format!(
            "{} <span alpha=\"60%\" size=\"small\">{}</span>",
            escape_display(name),
            escape_display(version)
        ),
        None => escape_display(name),
    }
}

/// Brings what can change while the page is open in line with the host.
fn update(env: &Rc<Env>, key: &str, live: &Live, info: &Overview) {
    let op = env.state.op(key);
    match &op {
        Some(Op::Failed(text)) | Some(Op::Done(text)) => {
            live.banner.set_title(&escape_display(text));
            live.banner.set_revealed(true);
        }
        _ => live.banner.set_revealed(false),
    }
    let line = match &op {
        Some(Op::Running { phase, .. }) => phase.clone(),
        _ => live.description.clone(),
    };
    live.head.set_subtitle(&escape_display(&line));
    let standing = standing_of(env, key);
    let slot = match (&op, &standing) {
        (Some(Op::Running { can_stop, .. }), _) => Slot::Busy {
            can_stop: *can_stop,
        },
        (_, Some(Standing::Update(_))) => Slot::Update,
        (_, Some(Standing::Replace(_))) => Slot::Replace,
        _ => Slot::Uninstall,
    };
    live.slot.show(slot, &live.name);
    live.status.set_subtitle(&escape_display(
        &super::plugin_row::status_line(info).unwrap_or_else(|| match info.state {
            PluginState::Running => "Running".into(),
            _ => "Switched off".into(),
        }),
    ));
    live.update_note
        .set_subtitle(&escape_display(&update_text(standing.as_ref())));
    live.protection.set_subtitle(&escape_display(
        &match (&info.protection, info.consent_offer) {
            (Some(line), Some(offer)) => format!(
            "{line}\nThe kernel can give this plugin {}. Allowing it applies to this plugin only.",
            offer.label()
        ),
            (Some(line), None) => line.clone(),
            (None, _) => "Checked when the plugin starts.".into(),
        },
    ));
    live.allow.set_visible(info.consent_offer.is_some());
    live.switch.set_sensitive(info.enabled || info.available);
    if live.switch.is_active() != info.enabled {
        live.updating.set(true);
        live.switch.set_active(info.enabled);
        live.updating.set(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::install::TestOwnedRoot;
    use crate::plugin::inventory;

    fn titles(rows: &[(&'static str, String)]) -> Vec<&'static str> {
        rows.iter().map(|(title, _)| *title).collect()
    }

    #[test]
    fn ordinary_details_omit_digests_and_release_identities() {
        let root = TestOwnedRoot::new();
        inventory::tests::fixture(&root.0);
        let entry = inventory::read(&root.0).unwrap().remove(0);
        let rows = titles(&technical_rows(&entry, None, false));
        assert_eq!(rows, ["Identity", "GitHub repository"]);
    }

    #[test]
    fn development_details_show_digests_release_and_commit() {
        let root = TestOwnedRoot::new();
        inventory::tests::fixture(&root.0);
        let entry = inventory::read(&root.0).unwrap().remove(0);
        let rows = technical_rows(&entry, None, true);
        let names = titles(&rows);
        for expected in [
            "Package SHA-256",
            "Manifest SHA-256",
            "Version commit",
            "Generation",
            "Release and asset",
            "Entrypoint",
        ] {
            assert!(names.contains(&expected), "{expected} in {names:?}");
        }
        let package = rows
            .iter()
            .find(|(title, _)| *title == "Package SHA-256")
            .unwrap();
        assert_eq!(package.1, entry.receipt.package_sha256);
    }

    #[test]
    fn a_local_install_is_described_without_inventing_a_repository() {
        let root = TestOwnedRoot::new();
        inventory::tests::development_fixture(&root.0);
        let entry = inventory::read(&root.0).unwrap().remove(0);
        let ordinary = technical_rows(&entry, None, false);
        assert!(ordinary.iter().any(|(title, _)| *title == "Source"));
        assert!(!ordinary
            .iter()
            .any(|(title, _)| *title == "GitHub repository"));
        let development = technical_rows(&entry, None, true);
        assert!(development.iter().any(|(title, _)| *title == "Provenance"));
        assert!(!development
            .iter()
            .any(|(title, _)| *title == "Publisher id"));
    }
}
