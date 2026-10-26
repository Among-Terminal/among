//! The versioned plugin protocol.
//!
//! Plugins are separate processes, so the wire format is the whole contract.
//! Everything crossing the boundary is plain JSON: no GTK types, no widget
//! handles, nothing that would pin an ABI. Parsing is deliberately strict —
//! unknown message types and out-of-range values are rejected rather than
//! coerced, because a plugin is untrusted input as far as the host is concerned.

use serde::{Deserialize, Serialize};

/// Protocol version spoken by this host.
///
/// Managed admission checks the declared minimum and negotiated feature set.
pub const PROTOCOL_MAJOR: u32 = 1;
pub const PROTOCOL_MINOR: u32 = 2;

/// Upper bound on text a plugin may send in one fragment.
///
/// Bounds the work done per message and what a single fragment can occupy on
/// screen; the renderer truncates further when a slot is narrower still.
pub const MAX_TEXT_CHARS: usize = 512;

/// Upper bound on fragments in one response.
pub const MAX_FRAGMENTS: usize = 64;

/// Upper bound on sources contributing to a single slot.
pub const MAX_SOURCES_PER_SLOT: usize = 32;

/// Priority bounds, mirroring `src/slots.rs`.
pub const MIN_PRIORITY: i32 = -1_000;
pub const MAX_PRIORITY: i32 = 1_000;

/// Upper bound on a refresh interval, so a plugin cannot pin the host into a
/// tight polling loop by declaring zero.
pub const MAX_REFRESH_INTERVAL_MS: u64 = 3_600_000;

/// Internal terminal events and the shared transport shutdown frame. Events are
/// converted to granted managed frames before they cross the process boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    #[cfg(test)]
    Hello { major: u32, minor: u32 },
    /// A terminal was created or its process ended.
    Event {
        event: EventKind,
        pane: u64,
        detail: Option<String>,
    },
    /// The user asked to stop.
    Shutdown,
}

/// What happened in the host that a plugin may care about.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    PaneOpened,
    PaneClosed,
    ProcessExited { status: i32 },
    WorkingDirectoryChanged,
    WorkspaceActivated,
}

/// Internal terminal snapshot; managed serialization filters fields by grants.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TerminalInfo {
    pub pane: u64,
    pub workspace: usize,
    /// The working directory, or `None` while unknown.
    ///
    /// A plain shell reports nothing until it changes directory, so absence is
    /// normal and must not be reported as an error.
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub running: bool,
}

/// Internal application context, never serialized directly to a managed child.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Context {
    pub active_workspace: usize,
    pub workspace_count: usize,
    pub terminals: Vec<TerminalInfo>,
    /// Set when the scope being asked about refers to one pane.
    #[serde(default)]
    pub focused_pane: Option<u64>,
}

/// Status payload validation shared by managed status and rendering tests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    /// The plugin's answer to a `Status` request.
    Status {
        request_id: u64,
        #[serde(default)]
        refresh_ms: Option<u64>,
        fragments: Vec<IncomingFragment>,
    },
}

/// One content item as it arrives from a plugin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IncomingFragment {
    /// Optional graphical fraction; text-only managed fragments remain valid.
    #[serde(default)]
    pub meter: Option<f64>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub style: Option<String>,
    #[serde(default)]
    pub tooltip: Option<String>,
    #[serde(default)]
    pub priority: i32,
}

/// A compact sidebar summary as it arrives from a plugin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IncomingSummary {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub tooltip: Option<String>,
    #[serde(default)]
    pub style: Option<String>,
}

/// Why a plugin message was refused.
///
/// Each variant is a separate fault the host must survive, which is why the
/// validation functions return a reason instead of silently repairing input.
#[derive(Debug, Clone, PartialEq)]
pub enum Rejection {
    /// The line was not valid JSON, or did not match the schema.
    Malformed,
    /// Valid JSON of a type this host does not know.
    UnknownType,
    /// A field held a value outside the permitted range.
    OutOfRange(&'static str),
    /// More fragments or sources than permitted.
    TooMany(&'static str),
    /// A fragment carried neither text nor an icon.
    Empty,
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Rejection::Malformed => write!(f, "message was not valid protocol data"),
            Rejection::UnknownType => write!(f, "message type is not recognised"),
            Rejection::OutOfRange(field) => write!(f, "field `{field}` was out of range"),
            Rejection::TooMany(field) => write!(f, "too many {field} in one message"),
            Rejection::Empty => write!(f, "fragment carried no text and no icon"),
        }
    }
}

impl std::error::Error for Rejection {}

/// A validated status response.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidStatus {
    pub request_id: u64,
    pub refresh_ms: Option<u64>,
    pub fragments: Vec<IncomingFragment>,
}

/// Parse one newline-delimited message from a plugin.
///
/// Returns `Err` for anything the host must not act on. Validation is separate
/// from parsing so a malformed line and an out-of-range value report different
/// causes.
#[cfg(test)]
fn parse_response(line: &str) -> Result<Response, Rejection> {
    if line.len() >= super::transport::MAX_FRAME_BYTES {
        return Err(Rejection::OutOfRange("frame_bytes"));
    }
    if !super::transport::depth_is_bounded(line.as_bytes()) {
        return Err(Rejection::OutOfRange("json_depth"));
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err(Rejection::Malformed);
    }
    serde_json::from_str(trimmed).map_err(|_| Rejection::Malformed)
}

/// Validate a status response before it reaches the slot store.
pub fn validate_status(response: Response) -> Result<ValidStatus, Rejection> {
    let Response::Status {
        request_id,
        refresh_ms,
        fragments,
    } = response;

    if let Some(interval) = refresh_ms {
        if interval > MAX_REFRESH_INTERVAL_MS {
            return Err(Rejection::OutOfRange("refresh_ms"));
        }
    }
    if fragments.len() > MAX_FRAGMENTS {
        return Err(Rejection::TooMany("fragments"));
    }
    for fragment in &fragments {
        if fragment
            .meter
            .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
        {
            return Err(Rejection::OutOfRange("meter"));
        }
        if fragment.priority < MIN_PRIORITY || fragment.priority > MAX_PRIORITY {
            return Err(Rejection::OutOfRange("priority"));
        }
        let text_len = fragment
            .text
            .as_deref()
            .map(str::chars)
            .map(Iterator::count)
            .unwrap_or(0);
        if text_len > MAX_TEXT_CHARS {
            return Err(Rejection::OutOfRange("text"));
        }
        let has_content = text_len > 0 || fragment.icon.as_deref().is_some_and(|i| !i.is_empty());
        if !has_content {
            return Err(Rejection::Empty);
        }
    }
    Ok(ValidStatus {
        request_id,
        refresh_ms,
        fragments,
    })
}

/// Build the `scope` string for a slot request.
pub fn scope_name(scope: &crate::slots::Scope) -> String {
    use crate::slots::Scope::*;
    match scope {
        AppHeader => "app_header".into(),
        AppFooter => "app_footer".into(),
        WorkspaceHeader => "workspace_header".into(),
        PaneHeader(id) => format!("pane_header:{}", id.0),
        PaneFooter(id) => format!("pane_footer:{}", id.0),
    }
}

/// Build the `scope` string for a workspace summary request.
pub fn summary_scope(workspace: usize) -> String {
    format!("workspace_summary:{workspace}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn parser_checks_frame_and_nesting_bounds_before_deserializing() {
        use super::*;
        let oversized = " ".repeat(crate::plugin::transport::MAX_FRAME_BYTES);
        assert_eq!(
            parse_response(&oversized),
            Err(Rejection::OutOfRange("frame_bytes"))
        );
        let deep = format!(
            "{{\"type\":\"status\",\"request_id\":1,\"fragments\":[],\"extra\":{}0{}}}",
            "[".repeat(crate::plugin::transport::MAX_JSON_DEPTH),
            "]".repeat(crate::plugin::transport::MAX_JSON_DEPTH),
        );
        assert_eq!(
            parse_response(&deep),
            Err(Rejection::OutOfRange("json_depth"))
        );
        let quoted = serde_json::json!({"type":"status","request_id":1,"fragments":[{"text":"[".repeat(256)}]}).to_string();
        assert!(matches!(
            parse_response(&quoted),
            Ok(Response::Status { .. })
        ));
    }

    #[test]
    fn meter_bounds_are_validated_and_text_payloads_remain_valid() {
        for value in [0.0, 0.5, 1.0] {
            let response = super::parse_response(&serde_json::json!({"type":"status","request_id":1,"fragments":[{"text":"CPU","meter":value}]}).to_string());
            assert!(response.is_ok());
            assert!(super::validate_status(response.unwrap()).is_ok());
        }
        for value in [-0.1, 1.1] {
            let response = super::parse_response(&serde_json::json!({"type":"status","request_id":1,"fragments":[{"text":"CPU","meter":value}]}).to_string());
            assert_eq!(
                super::validate_status(response.unwrap()).unwrap_err(),
                super::Rejection::OutOfRange("meter")
            );
        }
    }
    use super::*;

    #[test]
    fn a_valid_status_parses_and_validates() {
        let line = r#"{"type":"status","request_id":7,"refresh_ms":2000,
                       "fragments":[{"text":"main","style":"success","priority":10}]}"#;
        let parsed = parse_response(line).unwrap();
        let valid = validate_status(parsed).unwrap();
        assert_eq!(valid.request_id, 7);
        assert_eq!(valid.refresh_ms, Some(2000));
        assert_eq!(valid.fragments.len(), 1);
        assert_eq!(valid.fragments[0].style.as_deref(), Some("success"));
    }

    #[test]
    fn a_fragment_with_only_an_icon_is_accepted() {
        let line = r#"{"type":"status","request_id":1,"fragments":[{"icon":"folder-symbolic"}]}"#;
        let valid = validate_status(parse_response(line).unwrap()).unwrap();
        assert_eq!(valid.fragments.len(), 1);
    }

    #[test]
    fn malformed_json_is_rejected() {
        assert_eq!(
            parse_response("{not json").unwrap_err(),
            Rejection::Malformed
        );
        assert_eq!(parse_response("").unwrap_err(), Rejection::Malformed);
        assert_eq!(parse_response("   ").unwrap_err(), Rejection::Malformed);
    }

    #[test]
    fn an_unknown_message_type_is_rejected() {
        let line = r#"{"type":"definitely_not_a_thing","request_id":1}"#;
        assert_eq!(parse_response(line).unwrap_err(), Rejection::Malformed);

        let line = r#"{"type":"hello","major":1,"minor":0}"#;
        assert_eq!(parse_response(line).unwrap_err(), Rejection::Malformed);
    }

    #[test]
    fn an_out_of_range_priority_is_rejected() {
        let line = format!(
            r#"{{"type":"status","request_id":1,"fragments":[{{"text":"x","priority":{}}}]}}"#,
            MAX_PRIORITY + 1
        );
        let parsed = parse_response(&line).unwrap();
        assert_eq!(
            validate_status(parsed).unwrap_err(),
            Rejection::OutOfRange("priority")
        );
    }

    #[test]
    fn a_priority_at_either_bound_is_accepted() {
        for priority in [MIN_PRIORITY, MAX_PRIORITY] {
            let line = format!(
                r#"{{"type":"status","request_id":1,"fragments":[{{"text":"x","priority":{priority}}}]}}"#
            );
            assert!(validate_status(parse_response(&line).unwrap()).is_ok());
        }
    }

    #[test]
    fn a_negative_priority_beyond_the_floor_is_rejected() {
        let line = format!(
            r#"{{"type":"status","request_id":1,"fragments":[{{"text":"x","priority":{}}}]}}"#,
            MIN_PRIORITY - 1
        );
        assert_eq!(
            validate_status(parse_response(&line).unwrap()).unwrap_err(),
            Rejection::OutOfRange("priority")
        );
    }

    #[test]
    fn a_refresh_interval_at_the_limit_is_accepted() {
        let line = format!(
            r#"{{"type":"status","request_id":1,"refresh_ms":{MAX_REFRESH_INTERVAL_MS},"fragments":[{{"text":"x"}}]}}"#
        );
        assert!(validate_status(parse_response(&line).unwrap()).is_ok());
    }

    #[test]
    fn an_enormous_refresh_interval_does_not_pin_the_host() {
        let line = format!(
            r#"{{"type":"status","request_id":1,"refresh_ms":{},"fragments":[{{"text":"x"}}]}}"#,
            MAX_REFRESH_INTERVAL_MS + 1
        );
        assert!(validate_status(parse_response(&line).unwrap()).is_err());
    }

    #[test]
    fn too_many_fragments_are_rejected() {
        let items: Vec<String> = (0..MAX_FRAGMENTS + 1)
            .map(|_| r#"{"text":"x"}"#.to_string())
            .collect();
        let line = format!(
            r#"{{"type":"status","request_id":1,"fragments":[{}]}}"#,
            items.join(",")
        );
        assert_eq!(
            validate_status(parse_response(&line).unwrap()).unwrap_err(),
            Rejection::TooMany("fragments")
        );
    }

    #[test]
    fn overlong_text_is_rejected() {
        let long = "x".repeat(MAX_TEXT_CHARS + 1);
        let line =
            format!(r#"{{"type":"status","request_id":1,"fragments":[{{"text":"{long}"}}]}}"#);
        assert_eq!(
            validate_status(parse_response(&line).unwrap()).unwrap_err(),
            Rejection::OutOfRange("text")
        );
    }

    #[test]
    fn text_at_the_limit_is_accepted() {
        let long = "x".repeat(MAX_TEXT_CHARS);
        let line =
            format!(r#"{{"type":"status","request_id":1,"fragments":[{{"text":"{long}"}}]}}"#);
        assert!(validate_status(parse_response(&line).unwrap()).is_ok());
    }

    #[test]
    fn a_fragment_with_no_content_at_all_is_rejected() {
        let line = r#"{"type":"status","request_id":1,"fragments":[{"priority":5}]}"#;
        assert_eq!(
            validate_status(parse_response(line).unwrap()).unwrap_err(),
            Rejection::Empty
        );
    }

    #[test]
    fn an_empty_fragment_list_is_valid() {
        let line = r#"{"type":"status","request_id":1,"fragments":[]}"#;
        let valid = validate_status(parse_response(line).unwrap()).unwrap();
        assert!(valid.fragments.is_empty());
    }

    #[test]
    fn scope_names_are_unambiguous() {
        use crate::slots::Scope;
        assert_eq!(scope_name(&Scope::AppHeader), "app_header");
        assert_eq!(
            scope_name(&Scope::PaneHeader(crate::tree::PaneId(3))),
            "pane_header:3"
        );
        assert_eq!(
            scope_name(&Scope::PaneFooter(crate::tree::PaneId(7))),
            "pane_footer:7"
        );
        assert_eq!(summary_scope(2), "workspace_summary:2");
    }

    #[test]
    fn requests_round_trip_through_json() {
        let request = Request::Event {
            event: EventKind::WorkingDirectoryChanged,
            pane: 1,
            detail: Some("/fixture".into()),
        };
        let encoded = serde_json::to_string(&request).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&encoded).unwrap(), request);
    }

    #[test]
    fn a_terminal_without_a_cwd_deserialises_to_unknown() {
        let raw = r#"{"pane":1,"workspace":0}"#;
        let info: TerminalInfo = serde_json::from_str(raw).unwrap();
        assert_eq!(info.cwd, None);
    }
}
