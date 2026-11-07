//! the shield and the access list state only facts the application knows:
//! where a plugin came from and whether it was allowed to run below full
//! protection. there is no score, and an unknown fact is never shown as safe.

use crate::plugin::install::listing::{self, Offer};
use crate::plugin::inventory::Entry;

/// Where a plugin came from, as far as numeric identities establish it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Origin {
    /// The pinned official repository and publisher.
    Official,
    /// A published plugin of a source the user added, or whose publisher
    /// could not be confirmed.
    Other,
    /// Installed from a local catalogue: no repository, publisher or release.
    Development,
}

impl Origin {
    pub(super) fn of_installed(entry: &Entry) -> Self {
        match entry.receipt.release() {
            None => Self::Development,
            Some(release) => {
                let publisher = release.source.as_ref().map(|source| source.publisher_id);
                if listing::is_official(release.repository_id, publisher) {
                    Self::Official
                } else {
                    Self::Other
                }
            }
        }
    }

    pub(super) fn of_offer(offer: &Offer) -> Self {
        if listing::is_official(offer.source.repository_id, Some(offer.source.publisher_id)) {
            Self::Official
        } else {
            Self::Other
        }
    }
}

/// The four shield variants. They differ in shape, not in colour alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Shield {
    Official,
    OtherPublisher,
    Development,
    Weaker,
}

impl Shield {
    /// A weaker level outranks the origin: it is the fact that changes what
    /// the plugin can do to the user's files.
    pub(super) fn of(origin: Origin, weaker_protection: bool) -> Self {
        if weaker_protection {
            return Self::Weaker;
        }
        match origin {
            Origin::Official => Self::Official,
            Origin::Other => Self::OtherPublisher,
            Origin::Development => Self::Development,
        }
    }

    pub(super) fn icon_name(self) -> &'static str {
        match self {
            Self::Official => "security-high-symbolic",
            Self::OtherPublisher => "security-low-symbolic",
            Self::Development => "dialog-question-symbolic",
            Self::Weaker => "dialog-warning-symbolic",
        }
    }

    /// The accessible name; it carries the same title as the tooltip.
    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Official => "Official plugin",
            Self::OtherPublisher => "Plugin from another publisher",
            Self::Development => "Development install",
            Self::Weaker => "Weaker protection",
        }
    }

    pub(super) fn detail(self) -> &'static str {
        match self {
            Self::Official => "Published by Among Terminal. Protection is checked when the plugin starts.",
            Self::OtherPublisher => "Published by someone else. Among Terminal has not reviewed it. Protection is checked when the plugin starts.",
            Self::Development => "Installed from a local folder. Its repository, publisher and release are not known.",
            Self::Weaker => "Full sandboxing is unavailable or reduced protection was allowed. Review the protection details before starting it.",
        }
    }

    /// Tooltip and accessible description: title, detail and what it may read.
    pub(super) fn tooltip(self, features: &[String]) -> String {
        format!(
            "{}\n{}\nCan read: {}.",
            self.title(),
            self.detail(),
            access_summary(features)
        )
    }
}

/// Plain words for what a manifest asks to read. Features the application
/// does not know are named as they are written rather than dropped.
pub(super) fn access(feature: &str) -> (&'static str, String) {
    let known = match feature {
        "status" => ("dialog-information-symbolic", "Show a status line"),
        "settings" => ("preferences-system-symbolic", "Keep its own settings"),
        "context.terminals" => ("utilities-terminal-symbolic", "Your terminals"),
        "context.cwd" => ("folder-symbolic", "Folders of your terminals"),
        "context.title" => ("view-list-symbolic", "Terminal titles"),
        "context.process" => ("system-run-symbolic", "Programs running in terminals"),
        "events.terminal" => ("view-reveal-symbolic", "Terminal events"),
        "system.metrics" => (
            "preferences-system-details-symbolic",
            "CPU, memory and load",
        ),
        "notifications.produce" => (
            "preferences-system-notifications-symbolic",
            "Send notifications",
        ),
        "notifications.deliver" => (
            "preferences-system-notifications-symbolic",
            "Deliver notifications",
        ),
        _ => return ("dialog-question-symbolic", feature.to_owned()),
    };
    (known.0, known.1.to_owned())
}

/// The comma-separated reading list, or "nothing from your terminals".
pub(super) fn access_summary(features: &[String]) -> String {
    let words: Vec<String> = features
        .iter()
        .filter(|feature| !matches!(feature.as_str(), "status" | "settings"))
        .map(|feature| access(feature).1.to_lowercase())
        .collect();
    if words.is_empty() {
        "nothing from your terminals".into()
    } else {
        words.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_differ_in_icon_and_text_and_weaker_protection_wins() {
        let shields = [
            Shield::of(Origin::Official, false),
            Shield::of(Origin::Other, false),
            Shield::of(Origin::Development, false),
            Shield::of(Origin::Official, true),
        ];
        let icons: std::collections::BTreeSet<_> =
            shields.iter().map(|shield| shield.icon_name()).collect();
        let titles: std::collections::BTreeSet<_> =
            shields.iter().map(|shield| shield.title()).collect();
        assert_eq!(icons.len(), 4, "shape, not colour, tells them apart");
        assert_eq!(titles.len(), 4);
        assert_eq!(shields[3], Shield::Weaker);
    }

    #[test]
    fn an_unknown_publisher_is_never_official() {
        assert!(!listing::is_official(1_431_055_733, None));
        assert!(!listing::is_official(1_431_055_733, Some(1)));
        assert!(listing::is_official(1_431_055_733, Some(339_567_100)));
        assert!(!listing::is_official(2, Some(339_567_100)));
    }

    #[test]
    fn the_tooltip_names_what_the_plugin_can_read() {
        let reads = vec!["status".to_owned(), "context.terminals".to_owned()];
        let text = Shield::Official.tooltip(&reads);
        assert!(text.starts_with("Official plugin\n"), "{text}");
        assert!(text.contains("Can read: your terminals."), "{text}");
        assert!(Shield::Official
            .tooltip(&[])
            .contains("Can read: nothing from your terminals."));
        assert_eq!(
            access_summary(&["invented.feature".to_owned()]),
            "invented.feature"
        );
    }
}
