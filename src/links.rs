//! What counts as a link in a terminal, and whether a pointer press may follow it.
//!
//! The context menu and the Ctrl+click gesture must agree about which text is a
//! link and which target may be launched: a menu entry offering a target the
//! gesture refuses, or the reverse, would be a bug the user sees. The policy
//! therefore lives here once and both callers read it.
//!
//! Detection depends on terminal state set up by [`install`]: an explicit OSC 8
//! hyperlink, and a plain-URL matcher. Because that state belongs to the
//! terminal, not to the optional context menu, installation happens when a pane
//! is built.

use gtk::glib;
use vte::prelude::*;

/// A conservative absolute-URI match: paired delimiters and quotes are not guessed.
const PLAIN_URI_PATTERN: &str = r#"(?i)\b(?:https?://|mailto:)[^\s<>"'()\[\]{}]+"#;

/// Punctuation a sentence puts after a URL that is not part of it.
const SENTENCE_PUNCTUATION: [char; 6] = ['.', ',', ';', ':', '!', '?'];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub uri: String,
    /// Whether the target may be handed to the desktop URI handler.
    ///
    /// A scheme outside http, https and mailto is still a link: the context
    /// menu offers **Copy Link** for it. It is not something this application
    /// launches.
    pub openable: bool,
}

impl Link {
    pub fn parse(value: &str) -> Option<Self> {
        if value.len() > 8192 || value.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return None;
        }
        let uri = glib::Uri::parse(value, glib::UriFlags::NONE).ok()?;
        let scheme = uri.scheme().to_ascii_lowercase();
        if matches!(scheme.as_str(), "http" | "https")
            && uri.host().is_none_or(|host| host.is_empty())
        {
            return None;
        }
        if uri.path().is_empty() && scheme == "mailto" {
            return None;
        }
        Some(Self {
            uri: value.to_owned(),
            openable: matches!(scheme.as_str(), "http" | "https" | "mailto"),
        })
    }
}

/// Turn on explicit hyperlinks and the plain-URL matcher for `terminal`.
///
/// Called once per terminal, when its pane is built. A matcher that cannot be
/// compiled is logged, and explicit hyperlinks keep working without it.
pub fn install(terminal: &vte::Terminal) {
    terminal.set_allow_hyperlink(true);
    // VTE requires PCRE2_MULTILINE for terminal match expressions.
    match vte::Regex::for_match(PLAIN_URI_PATTERN, 0x400) {
        Ok(regex) => {
            terminal.match_add_regex(&regex, 0);
        }
        Err(error) => eprintln!("among: link matcher unavailable: {error}"),
    }
}

/// The link at terminal coordinates, if the terminal reports one.
///
/// An explicit hyperlink wins over whatever text its cells display, so a link
/// whose label is not its target still opens the target. Only a plain-text
/// match has sentence punctuation trimmed: the terminal knows exactly which
/// characters belong to an OSC 8 target.
pub fn at(terminal: &vte::Terminal, x: f64, y: f64) -> Option<Link> {
    if let Some(uri) = terminal.check_hyperlink_at(x, y) {
        return Link::parse(uri.as_str());
    }
    terminal
        .check_match_at(x, y)
        .0
        .and_then(|uri| from_plain_match(&uri))
}

/// Parse text captured by the plain-URL matcher.
fn from_plain_match(value: &str) -> Option<Link> {
    Link::parse(value.trim_end_matches(&SENTENCE_PUNCTUATION[..]))
}

/// Whether a press should be consumed in order to open `link`.
///
/// Kept free of widgets so the whole truth table can be tested: only a single
/// primary press with Ctrl held, over a target that may actually launch,
/// belongs to the link gesture. Everything else has to reach the terminal.
/// The live gesture is already limited to the primary button; the button is
/// checked here as well so the rule holds for any caller, not only for one
/// controller's configuration.
pub fn activates(ctrl: bool, button: u32, presses: i32, link: Option<&Link>) -> bool {
    ctrl && button == gtk::gdk::BUTTON_PRIMARY
        && presses == 1
        && link.is_some_and(|link| link.openable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_policy_never_turns_unsupported_schemes_into_commands() {
        for value in [
            "https://example.test/path?a=1&b=2",
            "HTTP://example.test",
            "mailto:a@example.test",
        ] {
            let link = Link::parse(value).expect("valid supported URI");
            assert!(link.openable);
            assert_eq!(link.uri, value);
        }
        for value in ["file:///tmp/test", "javascript:alert(1)", "custom:test"] {
            assert!(!Link::parse(value).expect("valid copy-only URI").openable);
        }
        for value in [
            "not-a-uri",
            "https:///missing-host",
            "https://a.test/\ncommand",
            "mailto:",
        ] {
            assert!(Link::parse(value).is_none(), "{value:?}");
        }
        assert!(Link::parse(&format!("https://example.test/{}", "a".repeat(8192))).is_none());
    }

    #[test]
    fn a_plain_match_loses_only_sentence_punctuation() {
        let link = from_plain_match("https://example.test/path?a=1&b=2.").expect("plain URL");
        assert_eq!(link.uri, "https://example.test/path?a=1&b=2");
        assert!(link.openable);
        // Punctuation inside a path is part of the target; only the tail goes.
        assert_eq!(
            from_plain_match("https://example.test/a,b;c").map(|link| link.uri),
            Some("https://example.test/a,b;c".into())
        );
        // An explicit hyperlink target is never trimmed, so the same text that a
        // plain match loses is kept when the terminal knows it is the target.
        assert_eq!(
            Link::parse("https://example.test/report.").map(|link| link.uri),
            Some("https://example.test/report.".into())
        );
    }

    #[test]
    fn only_ctrl_single_primary_presses_over_openable_targets_activate() {
        let openable = Link::parse("https://example.test/x");
        let copy_only = Link::parse("custom:test");
        assert!(activates(true, 1, 1, openable.as_ref()));
        for (ctrl, button, presses, link) in [
            (false, 1, 1, openable.as_ref()),
            (true, 2, 1, openable.as_ref()),
            (true, 3, 1, openable.as_ref()),
            (true, 1, 2, openable.as_ref()),
            (true, 1, 1, copy_only.as_ref()),
            (true, 1, 1, None),
        ] {
            assert!(
                !activates(ctrl, button, presses, link),
                "ctrl={ctrl} button={button} presses={presses}"
            );
        }
    }
}
