//! inspection and explicit package installation are separate actions. neither
//! grants host access nor supplies a new desired-enabled choice.

use super::*;
use crate::plugin::data::Problem;
use crate::plugin::install::source::{Inspection, InstallJob, Job, Repository, Selection};

type Start = dyn Fn(Repository) -> Result<Job, Problem>;
type Install = dyn Fn(Selection) -> Result<InstallJob, Problem>;

pub(super) fn present(
    parent: &gtk::Window,
    changed: Rc<dyn Fn()>,
    details: Rc<super::PluginDetailsQuery>,
) {
    present_complete(
        parent,
        Rc::new(Job::start),
        installer(),
        changed,
        crate::plugin::install::registry::root(),
        Rc::new(Job::refresh),
        details,
    );
}

#[cfg(test)]
pub(crate) fn present_for_test(parent: &gtk::Window, start: Rc<Start>) {
    present_with(parent, start, installer(), Rc::new(|| {}));
}

#[cfg(test)]
pub(crate) fn present_installer_for_test(
    parent: &gtk::Window,
    start: Rc<Start>,
    install: Rc<Install>,
    changed: Rc<dyn Fn()>,
) {
    present_with(parent, start, install, changed);
}

fn installer() -> Rc<Install> {
    Rc::new(|selection| {
        InstallJob::start(
            selection,
            crate::plugin::inventory::root()
                .ok_or(Problem("managed data directory is unavailable"))?,
        )
    })
}

#[cfg(test)]
fn present_with(
    parent: &gtk::Window,
    start: Rc<Start>,
    install: Rc<Install>,
    changed: Rc<dyn Fn()>,
) {
    present_complete(
        parent,
        start,
        install,
        changed,
        crate::plugin::install::registry::root(),
        Rc::new(Job::refresh),
        Rc::new(|_| None),
    );
}

#[cfg(test)]
pub(crate) fn present_sources_for_test(
    parent: &gtk::Window,
    start: Rc<Start>,
    refresh: Rc<super::repository_sources::Refresh>,
    install: Rc<Install>,
    changed: Rc<dyn Fn()>,
    root: std::path::PathBuf,
) {
    present_complete(
        parent,
        start,
        install,
        changed,
        Some(root),
        refresh,
        Rc::new(|_| None),
    );
}

#[cfg(test)]
pub(crate) fn present_updates_for_test(
    parent: &gtk::Window,
    start: Rc<Start>,
    install: Rc<Install>,
    changed: Rc<dyn Fn()>,
    details: Rc<super::PluginDetailsQuery>,
) {
    present_complete(
        parent,
        start,
        install,
        changed,
        crate::plugin::install::registry::root(),
        Rc::new(Job::refresh),
        details,
    );
}

fn present_complete(
    parent: &gtk::Window,
    start: Rc<Start>,
    install: Rc<Install>,
    changed: Rc<dyn Fn()>,
    source_root: Option<std::path::PathBuf>,
    refresh: Rc<super::repository_sources::Refresh>,
    details: Rc<super::PluginDetailsQuery>,
) {
    let window = adw::PreferencesWindow::builder()
        .transient_for(parent)
        .title("Inspect public GitHub repository")
        .search_enabled(false)
        .default_width(640)
        .default_height(480)
        .build();
    let page = adw::PreferencesPage::new();
    page.set_title("Repository");
    let group = adw::PreferencesGroup::new();
    group.set_title("Preview plugins from GitHub");
    group.set_description(Some(
        "Enter a public repository to read its plugin catalogue. Inspection does not install or enable plugins. Installation is a separate explicit action below.",
    ));
    let entry = adw::EntryRow::builder()
        .title("GitHub owner/repository")
        .build();
    entry.set_tooltip_text(Some(
        "owner/repository or https://github.com/owner/repository",
    ));
    entry
        .upcast_ref::<gtk::Widget>()
        .update_property(&[gtk::accessible::Property::Description(
        "Enter owner/repository or a public HTTPS GitHub repository URL. Press Enter to inspect.",
    )]);
    group.add(&entry);
    let inspect = gtk::Button::with_label("Inspect");
    inspect.add_css_class("suggested-action");
    inspect.set_valign(gtk::Align::Center);
    inspect.set_sensitive(false);
    entry.add_suffix(&inspect);
    let cancel = gtk::Button::with_label("Cancel request");
    cancel.set_valign(gtk::Align::Center);
    cancel.set_visible(false);
    let status = adw::ActionRow::builder()
        .title("Ready to inspect")
        .subtitle("No request has been sent.")
        .build();
    status.set_subtitle_lines(0);
    status.add_suffix(&cancel);
    let spinner = gtk::Spinner::new();
    spinner.set_visible(false);
    status.add_prefix(&spinner);
    let feedback = adw::PreferencesGroup::new();
    feedback.add(&status);
    let preview = adw::PreferencesGroup::new();
    preview.set_title("Repository metadata");
    preview.set_description(Some("Read-only preview. Plugins run as your user, not in an OS sandbox. Hashes do not authenticate a publisher."));
    preview.set_visible(false);
    let result = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(12)
        .bottom_margin(12)
        .left_margin(12)
        .right_margin(12)
        .build();
    result.update_property(&[gtk::accessible::Property::Label(
        "Repository inspection result",
    )]);
    let scroll = gtk::ScrolledWindow::builder()
        .child(&result)
        .min_content_height(220)
        .max_content_height(440)
        .propagate_natural_height(true)
        .build();
    scroll.add_css_class("card");
    preview.add(&scroll);
    let packages = adw::PreferencesGroup::new();
    packages.set_title("Select one ready-built version");
    packages.set_description(Some("New installations stay disabled. Applying an update is separate explicit consent to restart only that plugin; publisher/access changes still require runtime review."));
    packages.set_visible(false);
    page.add(&group);
    let package_rows = Rc::new(RefCell::new(Vec::new()));
    let show: Rc<dyn Fn(Inspection)> = Rc::new({
        let preview = preview.downgrade();
        let result = result.downgrade();
        let packages = packages.downgrade();
        let window = window.downgrade();
        let package_rows = package_rows.clone();
        move |inspection| {
            let (Some(preview), Some(result), Some(packages), Some(window)) = (
                preview.upgrade(),
                result.upgrade(),
                packages.upgrade(),
                window.upgrade(),
            ) else {
                return;
            };
            result.buffer().set_text(
                &inspection
                    .summary(|| Ok(()))
                    .unwrap_or_else(|problem| problem.to_string()),
            );
            preview.set_visible(true);
            for row in package_rows.borrow_mut().drain(..) {
                packages.remove(&row);
            }
            *package_rows.borrow_mut() = installation_rows(
                &packages,
                &inspection,
                install.clone(),
                changed.clone(),
                &window,
                details.clone(),
            );
            packages.set_visible(true);
        }
    });
    let sources =
        super::repository_sources::Panel::new(&page, &preview, source_root, refresh, show.clone());
    page.add(&feedback);
    page.add(&preview);
    page.add(&packages);
    window.add(&page);
    let job = Rc::new(RefCell::<Option<Job>>::new(None));
    entry.connect_entry_activated({
        let inspect = inspect.downgrade();
        move |_| {
            let inspect = inspect.clone();
            // let the entry finish its key event before the request disables it.
            glib::idle_add_local_once(move || {
                if let Some(inspect) = inspect
                    .upgrade()
                    .filter(|button| button.is_mapped() && button.is_sensitive())
                {
                    inspect.emit_clicked();
                }
            });
        }
    });
    entry.connect_changed({
        let job = job.clone();
        let inspect = inspect.downgrade();
        let status = status.downgrade();
        let preview = preview.downgrade();
        let packages = packages.downgrade();
        let sources = sources.clone();
        move |entry| {
            if job.borrow().is_some() {
                return;
            }
            entry.remove_css_class("error");
            sources.clear();
            if let Some(inspect) = inspect.upgrade() {
                inspect.set_sensitive(!entry.text().trim().is_empty());
            }
            if let Some(preview) = preview.upgrade() {
                preview.set_visible(false);
            }
            if let Some(packages) = packages.upgrade() {
                packages.set_visible(false);
            }
            if let Some(status) = status.upgrade() {
                status.set_title("Ready to inspect");
                status.set_subtitle("Press Inspect or Enter to request repository metadata.");
            }
        }
    });
    inspect.connect_clicked({
        let job = job.clone();
        let entry = entry.downgrade();
        let status = status.downgrade();
        let result = result.downgrade();
        let cancel = cancel.downgrade();
        let spinner = spinner.downgrade();
        let preview = preview.downgrade();
        let packages = packages.downgrade();
        let sources = sources.clone();
        move |inspect| {
            let (
                Some(entry),
                Some(status),
                Some(result),
                Some(cancel),
                Some(spinner),
                Some(preview),
            ) = (
                entry.upgrade(),
                status.upgrade(),
                result.upgrade(),
                cancel.upgrade(),
                spinner.upgrade(),
                preview.upgrade(),
            )
            else {
                return;
            };
            if job.borrow().is_some() {
                return;
            }
            result.buffer().set_text("");
            sources.clear();
            preview.set_visible(false);
            if let Some(packages) = packages.upgrade() {
                packages.set_visible(false);
            }
            entry.remove_css_class("error");
            let repository = match Repository::parse(&entry.text()) {
                Ok(repository) => repository,
                Err(problem) => {
                    entry.add_css_class("error");
                    status.set_title("Check the repository address");
                    status.set_subtitle(problem.0);
                    entry.grab_focus();
                    return;
                }
            };
            match start(repository) {
                Ok(requested) => {
                    *job.borrow_mut() = Some(requested);
                    status.set_title("Inspecting repository");
                    status.set_subtitle("Inspecting public repository metadata…");
                    inspect.set_sensitive(false);
                    cancel.set_visible(true);
                    cancel.set_sensitive(true);
                    cancel.grab_focus();
                    entry.set_sensitive(false);
                    spinner.set_visible(true);
                    spinner.start();
                }
                Err(problem) => {
                    status.set_title("Could not inspect repository");
                    status.set_subtitle(problem.0);
                }
            }
        }
    });
    cancel.connect_clicked({
        let job = job.clone();
        let status = status.downgrade();
        move |cancel| {
            if let Some(job) = job.borrow().as_ref() {
                job.cancel();
                cancel.set_sensitive(false);
                if let Some(status) = status.upgrade() {
                    status.set_title("Cancelling request");
                    status.set_subtitle("Cancellation requested; waiting for the worker to stop…");
                }
            }
        }
    });
    window.connect_destroy({
        let job = job.clone();
        move |_| {
            job.borrow_mut().take();
        }
    });
    glib::timeout_add_local(std::time::Duration::from_millis(50), {
        let job = job.clone();
        let window = window.downgrade();
        let entry = entry.downgrade();
        let inspect = inspect.downgrade();
        let cancel = cancel.downgrade();
        let status = status.downgrade();
        let spinner = spinner.downgrade();
        let sources = sources.clone();
        move || {
            let (
                Some(window),
                Some(entry),
                Some(inspect),
                Some(cancel),
                Some(status),
                Some(spinner),
            ) = (
                window.upgrade(),
                entry.upgrade(),
                inspect.upgrade(),
                cancel.upgrade(),
                status.upgrade(),
                spinner.upgrade(),
            )
            else {
                job.borrow_mut().take();
                return glib::ControlFlow::Break;
            };
            if !window.is_visible() {
                job.borrow_mut().take();
                return glib::ControlFlow::Break;
            }
            let outcome = job.borrow().as_ref().and_then(Job::poll);
            if let Some(outcome) = outcome {
                job.borrow_mut().take();
                inspect.set_sensitive(true);
                cancel.set_sensitive(false);
                cancel.set_visible(false);
                entry.set_sensitive(true);
                spinner.stop();
                spinner.set_visible(false);
                match outcome {
                    Ok(inspection) => {
                        show(inspection.clone());
                        sources.inspected(inspection);
                        status.set_title("Metadata preview ready");
                        status.set_subtitle("Metadata inspected. No installation, registration or activation was performed.");
                    }
                    Err(problem) => {
                        status.set_title("No metadata preview");
                        status.set_subtitle(problem.0);
                    }
                }
                entry.grab_focus();
            }
            glib::ControlFlow::Continue
        }
    });
    window.present();
    entry.grab_focus();
}

fn installation_rows(
    group: &adw::PreferencesGroup,
    inspection: &Inspection,
    install: Rc<Install>,
    changed: Rc<dyn Fn()>,
    window: &adw::PreferencesWindow,
    details: Rc<super::PluginDetailsQuery>,
) -> Vec<adw::ActionRow> {
    let mut rows = Vec::new();
    for plugin in &inspection.catalogue.plugins {
        for version in &plugin.versions {
            let row = adw::ActionRow::builder()
                .title(escape_display(&format!(
                    "{} — {}",
                    version.manifest.name, version.manifest.version
                )))
                .build();
            row.set_subtitle_lines(0);
            let selection = inspection.select(&plugin.id, &version.manifest.version);
            let button = gtk::Button::with_label("Install disabled");
            button.set_valign(gtk::Align::Center);
            button.update_property(&[gtk::accessible::Property::Label(&format!(
                "Install {} version {} disabled",
                plugin.id, version.manifest.version
            ))]);
            row.add_suffix(&button);
            match selection {
                Err(problem) => {
                    row.set_subtitle(problem.0);
                    button.set_sensitive(false);
                }
                Ok(selection) => {
                    let expected = details(&selection.identity()).map(|(entry, _)| entry.receipt);
                    if expected.is_some() {
                        button.set_label("Apply update…");
                        button.update_property(&[gtk::accessible::Property::Label(&format!(
                            "Apply {} version {} update",
                            plugin.id, version.manifest.version
                        ))]);
                    }
                    row.set_subtitle(&escape_display(&selection.description()));
                    let install = install.clone();
                    let changed = changed.clone();
                    let row = row.downgrade();
                    let window = window.downgrade();
                    let details = details.clone();
                    button.connect_clicked(move |button| {
                        let Some(row) = row.upgrade() else { return; };
                        if let Some(expected)=&expected {
                            let Some((entry,stored))=details(&selection.identity()).filter(|(entry,_)|&entry.receipt==expected) else {row.set_subtitle("Installed generation changed; reopen current metadata before applying an update.");return;};
                            let selection=match selection.clone().for_update(&entry,stored.as_ref()){Ok(selection)=>selection,Err(problem)=>{row.set_subtitle(problem.0);return;}};
                            let Some(parent)=window.upgrade() else {return;};
                            let dialog=adw::AlertDialog::builder().heading("Apply this plugin update?").body(selection.review_description()).build();
                            dialog.add_response("cancel","Cancel");dialog.add_response("apply","Apply update");dialog.set_default_response(Some("cancel"));dialog.set_close_response("cancel");
                            dialog.connect_response(None,{
                                let button=button.downgrade();let row=row.downgrade();let install=install.clone();let changed=changed.clone();let window=window.clone();let details=details.clone();let expected=expected.clone();let selection=selection.clone();
                                move |_,response| {
                                    if response!="apply" {return;}
                                    let (Some(button),Some(row),Some(parent))=(button.upgrade(),row.upgrade(),window.upgrade().filter(|window|window.is_visible())) else {return;};
                                    let Some((entry,stored))=details(&selection.identity()).filter(|(entry,_)|entry.receipt==expected) else {row.set_subtitle("Update confirmation is stale; installed generation changed.");return;};
                                    match selection.clone().for_update(&entry,stored.as_ref()) {
                                        Ok(selection)=>run_install(&button,&row,selection,install.clone(),changed.clone(),&parent),
                                        Err(problem)=>row.set_subtitle(problem.0),
                                    }
                                }
                            });
                            dialog.present(Some(&parent));
                        }else if let Some(parent)=window.upgrade().filter(|window|window.is_visible()){run_install(button,&row,selection.clone(),install.clone(),changed.clone(),&parent);}
                    });
                }
            }
            group.add(&row);
            rows.push(row);
        }
    }
    rows
}

fn run_install(
    button: &gtk::Button,
    row: &adw::ActionRow,
    selection: Selection,
    install: Rc<Install>,
    changed: Rc<dyn Fn()>,
    window: &adw::PreferencesWindow,
) {
    let updating = selection.is_update();
    let window = window.downgrade();
    match install(selection) {
        Err(problem) => row.set_subtitle(problem.0),
        Ok(job) => {
            button.set_sensitive(false);
            row.set_subtitle(job.phase());
            let (identity, generation) = job.operation();
            row.set_tooltip_text(Some(&format!(
                "Identity: {identity}\nOperation generation: {generation}"
            )));
            let cancel = gtk::Button::with_label("Cancel install");
            cancel.set_valign(gtk::Align::Center);
            row.add_suffix(&cancel);
            cancel.grab_focus();
            let job = Rc::new(job);
            cancel.connect_clicked({
                let job = job.clone();
                move |button| {
                    button.set_sensitive(false);
                    job.cancel();
                }
            });
            let mut destroy_handler = window.upgrade().map(|window| {
                let handler = window.connect_destroy({
                    let job = job.clone();
                    move |_| {
                        job.cancel();
                    }
                });
                (window.downgrade(), handler)
            });
            let changed = changed.clone();
            let row = row.downgrade();
            let button = button.downgrade();
            let cancel = cancel.downgrade();
            // do not tie authoritative reconciliation to a surviving view:
            // a committed selection still reaches the host after close.
            glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
                if let Some(outcome) = job.poll() {
                    if let Some((window, handler)) = destroy_handler.take() {
                        if let Some(window) = window.upgrade() {
                            window.disconnect(handler);
                        }
                    }
                    let committed = outcome.is_ok();
                    if outcome.is_ok() {
                        changed();
                    }
                    if let Some(row) = row.upgrade() {
                        if let Some(cancel) = cancel.upgrade() {
                            row.remove(&cancel);
                        }
                        row.set_subtitle(&match outcome {
                                                Ok(commit) => match commit.durability {
                                                    crate::settings::Durability::Durable if updating => "Update selected; previous verified generation retained. Current enabled choice is preserved; publisher/access and runtime admission can still refuse execution.".into(),
                                                    crate::settings::Durability::Durable => format!("Installed github:{}/{} disabled. Enable separately in Your plugins.", commit.receipt.repository_id, commit.receipt.plugin_id),
                                                    _ => "Selection written; durability uncertain. Inspect the installed inventory before retrying.".into(),
                                                },
                                                Err(problem) => problem.to_string(),
                                            });
                    }
                    if let Some(button) = button.upgrade() {
                        button.set_sensitive(!committed);
                    }
                    return glib::ControlFlow::Break;
                }
                if let Some(row) = row.upgrade() {
                    row.set_subtitle(job.phase());
                }
                glib::ControlFlow::Continue
            });
        }
    }
}
