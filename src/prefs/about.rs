//! The About page: what this build is and where to find the project.
//!
//! Opening the page reads only compiled-in values and `os-release`; nothing
//! touches the network until the user follows one of the links.

use super::*;
use crate::build_info::{self, BuildInfo};

const APPLICATION_NAME: &str = "Among Terminal";
const TAGLINE: &str = "Organize live terminals in project workspaces";
const LICENSE: &str = "Apache-2.0";

/// A project link: the row title, the address shown under it and the address
/// opened. The shown address omits the scheme so the row stays short.
struct Link {
    title: &'static str,
    shown: &'static str,
    uri: &'static str,
}

const LINKS: [Link; 3] = [
    Link {
        title: "Website",
        shown: "among-terminal.github.io",
        uri: "https://among-terminal.github.io/",
    },
    Link {
        title: "Source code",
        shown: "github.com/Among-Terminal/among",
        uri: "https://github.com/Among-Terminal/among",
    },
    Link {
        title: "Report an issue",
        shown: "github.com/Among-Terminal/among/issues",
        uri: "https://github.com/Among-Terminal/among/issues",
    },
];

/// The build tag under the name: the commit and the date, leaving out whichever
/// the build did not record, and a single "unknown" when it recorded neither.
fn build_label(info: &BuildInfo) -> String {
    let known: Vec<&str> = [info.commit.as_str(), info.date.as_str()]
        .into_iter()
        .filter(|value| *value != build_info::UNKNOWN)
        .collect();
    if known.is_empty() {
        format!("Build {}", build_info::UNKNOWN)
    } else {
        format!("Build {}", known.join(" · "))
    }
}

fn version_label(info: &BuildInfo) -> String {
    format!("Version {}", info.version)
}

fn badge(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("about-badge");
    label
}

fn header(info: &BuildInfo) -> gtk::Box {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
    column.set_halign(gtk::Align::Center);
    column.set_margin_top(12);
    column.set_margin_bottom(12);

    let logo = gtk::Image::from_icon_name("com.github.among-terminal");
    logo.set_pixel_size(96);
    // The logo is decoration; the name below carries the meaning.
    logo.set_accessible_role(gtk::AccessibleRole::Presentation);
    column.append(&logo);

    let name = gtk::Label::new(Some(APPLICATION_NAME));
    name.add_css_class("title-1");
    name.set_margin_top(6);
    column.append(&name);

    let tagline = gtk::Label::new(Some(TAGLINE));
    tagline.add_css_class("dim-label");
    tagline.set_wrap(true);
    tagline.set_justify(gtk::Justification::Center);
    column.append(&tagline);

    let badges = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    badges.set_halign(gtk::Align::Center);
    badges.set_margin_top(6);
    badges.append(&badge(&version_label(info)));
    badges.append(&badge(&build_label(info)));
    column.append(&badges);
    column
}

fn link_row(link: &Link) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(link.title);
    row.set_subtitle(&escape_display(link.shown));
    row.set_activatable(true);
    row.set_widget_name(&format!(
        "about-link-{}",
        link.title.to_lowercase().replace(' ', "-")
    ));
    row.set_tooltip_text(Some(link.uri));
    let arrow = gtk::Image::from_icon_name("go-next-symbolic");
    arrow.set_accessible_role(gtk::AccessibleRole::Presentation);
    row.add_suffix(&arrow);
    let uri = link.uri;
    row.connect_activated(move |row| {
        let window = row
            .root()
            .and_then(|root| root.downcast::<gtk::Window>().ok());
        gtk::UriLauncher::new(uri).launch(
            window.as_ref(),
            None::<&gio::Cancellable>,
            move |result| {
                if let Err(error) = result {
                    eprintln!("among: Could not open link: {error}");
                }
            },
        );
    });
    row
}

/// The row that puts the build details on the clipboard.
///
/// The icon turns into a check mark for a moment, so the copy is acknowledged
/// without a toast.
fn copy_row(info: BuildInfo) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title("Copy build details");
    row.set_subtitle("Version, build and system, for bug reports");
    row.set_activatable(true);
    row.set_widget_name("about-copy");
    let icon = gtk::Image::from_icon_name("edit-copy-symbolic");
    icon.set_accessible_role(gtk::AccessibleRole::Presentation);
    row.add_suffix(&icon);
    row.connect_activated(move |row| {
        let text = build_info::report_text(&info, &build_info::system());
        row.clipboard().set_text(&text);
        icon.set_icon_name(Some("object-select-symbolic"));
        let icon = icon.downgrade();
        glib::timeout_add_local_once(std::time::Duration::from_secs(2), move || {
            if let Some(icon) = icon.upgrade() {
                icon.set_icon_name(Some("edit-copy-symbolic"));
            }
        });
    });
    row
}

pub(super) fn page() -> adw::PreferencesPage {
    let info = build_info::current();
    let page = adw::PreferencesPage::new();
    page.set_name(Some("about"));
    page.set_title("About");
    page.set_icon_name(Some("help-about-symbolic"));

    let identity = adw::PreferencesGroup::new();
    identity.add(&header(&info));
    page.add(&identity);

    let links = adw::PreferencesGroup::new();
    for link in &LINKS {
        links.add(&link_row(link));
    }
    page.add(&links);

    let details = adw::PreferencesGroup::new();
    let license = adw::ActionRow::new();
    license.set_title("License");
    let name = gtk::Label::new(Some(LICENSE));
    name.add_css_class("dim-label");
    license.add_suffix(&name);
    details.add(&license);
    details.add(&copy_row(info));
    page.add(&details);
    page
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_links_are_the_documented_addresses() {
        let uris: Vec<_> = LINKS.iter().map(|link| link.uri).collect();
        assert_eq!(
            uris,
            [
                "https://among-terminal.github.io/",
                "https://github.com/Among-Terminal/among",
                "https://github.com/Among-Terminal/among/issues",
            ]
        );
        for link in &LINKS {
            assert_eq!(
                link.uri
                    .trim_start_matches("https://")
                    .trim_end_matches('/'),
                link.shown,
                "{} shows an address that is not the one it opens",
                link.title
            );
        }
    }

    #[test]
    fn the_badges_read_as_version_and_build() {
        let info = build_info::resolve(Some("0.2.0-rc.2"), Some("723e04b"), Some("2026-11-05"));
        assert_eq!(version_label(&info), "Version 0.2.0-rc.2");
        assert_eq!(build_label(&info), "Build 723e04b · 2026-11-05");
        let unknown = build_info::resolve(None, None, None);
        assert_eq!(build_label(&unknown), "Build unknown");
        let commit_only = build_info::resolve(None, Some("723e04b"), None);
        assert_eq!(build_label(&commit_only), "Build 723e04b");
        let date_only = build_info::resolve(None, None, Some("2026-11-05"));
        assert_eq!(build_label(&date_only), "Build 2026-11-05");
    }

    #[test]
    #[ignore = "requires an isolated GTK display"]
    fn the_page_shows_the_build_and_links_without_starting_anything() {
        adw::init().expect("isolated GTK display");
        let page = page();
        assert_eq!(page.name().as_deref(), Some("about"));
        let mut rows = Vec::new();
        fn collect(widget: &gtk::Widget, rows: &mut Vec<adw::ActionRow>) {
            if let Ok(row) = widget.clone().downcast::<adw::ActionRow>() {
                rows.push(row);
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                collect(&widget, rows);
                child = widget.next_sibling();
            }
        }
        collect(page.upcast_ref(), &mut rows);
        let titles: Vec<String> = rows.iter().map(|row| row.title().to_string()).collect();
        assert_eq!(
            titles,
            [
                "Website",
                "Source code",
                "Report an issue",
                "License",
                "Copy build details"
            ]
        );
    }
}
