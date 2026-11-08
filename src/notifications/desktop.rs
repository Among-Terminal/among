//! Plain text and eligibility for the desktop bell channel.
//!
//! Terminal titles and directories are program-controlled. Decode invalid UTF-8,
//! remove markup delimiters and controls, and bound the final payload before it
//! reaches a desktop service. This module does not deliver notifications.

use super::{Focus, Policy, Source};

mod delivery;
pub use delivery::{Availability, Runtime, Sender, Target, GLOBAL_INTERVAL};

/// Maximum title length in Unicode scalar values (at most 512 UTF-8 bytes).
/// This is a payload budget, not an estimate of what a notification UI displays.
pub const MAX_TITLE_CHARS: usize = 128;
/// Maximum body length in Unicode scalar values (at most 2048 UTF-8 bytes).
/// The body has a larger budget because it carries both a title and a directory.
pub const MAX_BODY_CHARS: usize = 512;

/// Trusted translatable labels. `bell_in` contains a `{title}` placeholder so
/// translations can put the terminal title in the appropriate position.
#[derive(Clone, Copy)]
pub struct Labels<'a> {
    pub application: &'a str,
    pub bell: &'a str,
    pub bell_in: &'a str,
}

impl Labels<'static> {
    pub const ENGLISH: Self = Self {
        application: "Among Terminal",
        bell: "Bell",
        bell_in: "Bell in '{title}'",
    };
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text {
    pub title: String,
    pub body: String,
}

impl Text {
    pub fn new(
        workspace: Option<&[u8]>,
        terminal_title: Option<&[u8]>,
        directory: Option<&[u8]>,
    ) -> Self {
        Self::with_labels(workspace, terminal_title, directory, Labels::ENGLISH)
    }

    pub fn with_labels(
        workspace: Option<&[u8]>,
        terminal_title: Option<&[u8]>,
        directory: Option<&[u8]>,
        labels: Labels<'_>,
    ) -> Self {
        let workspace = sanitise(workspace.unwrap_or_default(), MAX_TITLE_CHARS);
        let title = if workspace.is_empty() {
            sanitise(labels.application.as_bytes(), MAX_TITLE_CHARS)
        } else {
            workspace
        };
        let terminal_title = sanitise(terminal_title.unwrap_or_default(), MAX_BODY_CHARS);
        let directory = sanitise(directory.unwrap_or_default(), MAX_BODY_CHARS);
        let mut body = if terminal_title.is_empty() {
            labels.bell.to_owned()
        } else {
            labels.bell_in.replace("{title}", &terminal_title)
        };
        if !directory.is_empty() {
            body.push(' ');
            body.push_str(&directory);
        }
        Self {
            title,
            body: sanitise(body.as_bytes(), MAX_BODY_CHARS),
        }
    }
}

/// Clean a byte string while allocating only the bounded result. Length counts
/// Unicode scalar values, not grapheme clusters; a cut always leaves valid UTF-8.
/// Whitespace becomes one space, including tab/newline controls. Other C0/C1
/// controls and `<`, `>` and `&` are removed so no markup can be interpreted.
pub fn sanitise(input: &[u8], max_chars: usize) -> String {
    let mut output = String::new();
    let mut length = 0;
    let mut space = false;
    for chunk in input.utf8_chunks() {
        for character in chunk
            .valid()
            .chars()
            .chain((!chunk.invalid().is_empty()).then_some('\u{fffd}'))
        {
            if character.is_whitespace() {
                space = !output.is_empty();
                continue;
            }
            if character.is_control() || matches!(character, '<' | '>' | '&') {
                continue;
            }
            if space && length < max_chars {
                output.push(' ');
                length += 1;
            }
            space = false;
            if length == max_chars {
                return output.trim_end().to_owned();
            }
            output.push(character);
            length += 1;
        }
    }
    output
}

/// Desktop alerts require a terminal target and never notify about the terminal
/// currently viewed, including when the user selected the Always condition.
/// The shared policy retains its existing meaning for every other channel.
pub fn permits(policy: Policy, source: Option<Source>, focus: Focus) -> bool {
    source.is_some_and(|source| !focus.views(source)) && policy.permits(source, focus)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::{Eligibility, Host};

    #[test]
    fn known_metadata_is_shown_and_unknown_parts_are_omitted() {
        for (workspace, title, directory, expected_title, expected_body) in [
            (None, None, None, "Among Terminal", "Bell"),
            (Some("build"), None, None, "build", "Bell"),
            (None, Some("make"), None, "Among Terminal", "Bell in 'make'"),
            (None, None, Some("/work"), "Among Terminal", "Bell /work"),
            (Some("build"), Some("make"), None, "build", "Bell in 'make'"),
            (Some("build"), None, Some("/work"), "build", "Bell /work"),
            (
                None,
                Some("make"),
                Some("/work"),
                "Among Terminal",
                "Bell in 'make' /work",
            ),
            (
                Some("build"),
                Some("make"),
                Some("/work"),
                "build",
                "Bell in 'make' /work",
            ),
        ] {
            assert_eq!(
                Text::new(
                    workspace.map(str::as_bytes),
                    title.map(str::as_bytes),
                    directory.map(str::as_bytes)
                ),
                Text {
                    title: expected_title.into(),
                    body: expected_body.into()
                }
            );
        }
    }

    #[test]
    fn translated_labels_control_the_fallback_and_title_position() {
        let labels = Labels {
            application: "Terminal",
            bell: "Signal",
            bell_in: "'{title}' rang",
        };
        assert_eq!(Text::with_labels(None, None, None, labels).body, "Signal");
        assert_eq!(
            Text::with_labels(None, Some(b"make"), None, labels),
            Text {
                title: "Terminal".into(),
                body: "'make' rang".into()
            }
        );
    }

    #[test]
    fn hostile_text_cannot_supply_markup_or_controls() {
        let text = Text::new(
            Some(b"<b>build</b>&\x1b[1m"),
            Some(" \t<make>&\n\u{0085}\u{009f}".as_bytes()),
            Some(b"/work\x00\r\n/project"),
        );
        assert_eq!(text.body, "Bell in 'make' /work /project");
        for value in [&text.title, &text.body] {
            assert!(!value.chars().any(char::is_control));
            assert!(!value.contains(['<', '>', '&']));
        }
    }

    #[test]
    fn invalid_utf8_and_combining_marks_remain_valid_at_the_limit() {
        assert_eq!(
            sanitise(b"make\xff\xf0\x28\x8c\xbc", 32),
            "make\u{fffd}\u{fffd}(\u{fffd}\u{fffd}"
        );
        let combined = "e\u{0301}x";
        assert_eq!(sanitise(combined.as_bytes(), 1), "e");
        assert_eq!(sanitise(combined.as_bytes(), 2), "e\u{0301}");
        assert_eq!(sanitise(combined.as_bytes(), 3), combined);
        assert_eq!(sanitise("界界".as_bytes(), 1), "界");
    }

    #[test]
    fn empty_whitespace_and_oversized_inputs_stay_within_the_payload_budgets() {
        for input in [b"".as_slice(), b" \t\n\r", b"\0\x1b<>&"] {
            assert_eq!(sanitise(input, MAX_BODY_CHARS), "");
        }
        assert_eq!(sanitise(b"hello", 0), "");
        assert_eq!(sanitise(b"a b", 2), "a");
        let huge = "界".repeat(MAX_BODY_CHARS * 3);
        let text = Text::new(
            Some(huge.as_bytes()),
            Some(huge.as_bytes()),
            Some(huge.as_bytes()),
        );
        assert_eq!(text.title.chars().count(), MAX_TITLE_CHARS);
        assert_eq!(text.body.chars().count(), MAX_BODY_CHARS);
        assert!(text.title.len() <= MAX_TITLE_CHARS * 4);
        assert!(text.body.len() <= MAX_BODY_CHARS * 4);
    }

    #[test]
    fn a_viewed_terminal_is_suppressed_under_every_desktop_condition() {
        let mut host = Host::new().unwrap();
        let source = host.attach(1).unwrap();
        let sibling = host.attach(2).unwrap();
        for eligibility in [
            Eligibility::Always,
            Eligibility::OriginatingTerminalUnfocused,
            Eligibility::ApplicationBackground,
        ] {
            for enabled in [false, true] {
                let policy = Policy {
                    enabled,
                    eligibility,
                };
                for active in [false, true] {
                    for viewed in [None, Some(source), Some(sibling)] {
                        let focus = Focus {
                            application_active: active,
                            viewed,
                        };
                        let expected = enabled
                            && !(active && viewed == Some(source))
                            && match eligibility {
                                Eligibility::Always | Eligibility::OriginatingTerminalUnfocused => {
                                    true
                                }
                                Eligibility::ApplicationBackground => !active,
                            };
                        assert_eq!(permits(policy, Some(source), focus), expected,
                            "{eligibility:?}, enabled={enabled}, active={active}, viewed={viewed:?}");
                        assert!(!permits(policy, None, focus));
                    }
                }
            }
        }
    }
}
