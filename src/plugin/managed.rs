//! managed messages are a separate admitted profile. stable target references
//! never borrow authority from a mutable legacy workspace index.

use super::data::{self, Problem};
use super::manifest::Manifest;
use super::protocol::{Context, EventKind, IncomingFragment};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

pub const MAJOR: u32 = super::protocol::PROTOCOL_MAJOR;
pub const MINOR: u32 = super::protocol::PROTOCOL_MINOR;
const PAGE_BYTES: usize = 28 * 1024;
const PAGE_TERMINALS: usize = 32;
pub const FEATURES: &[&str] = &[
    "status",
    "settings",
    "context.terminals",
    "context.cwd",
    "context.title",
    "context.process",
    "events.terminal",
];

#[derive(Clone, Debug, Default)]
pub struct World {
    pub session: String,
    pub workspaces: Vec<u64>,
    pub legacy: Context,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Target {
    Application,
    Workspace {
        #[serde(
            serialize_with = "serialize_reference",
            deserialize_with = "deserialize_reference"
        )]
        reference: u64,
    },
    Pane {
        #[serde(
            serialize_with = "serialize_reference",
            deserialize_with = "deserialize_reference"
        )]
        reference: u64,
    },
}

fn serialize_reference<S: serde::Serializer>(
    reference: &u64,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&reference.to_string())
}

pub fn parse_reference(value: &str) -> Result<u64, Problem> {
    if value.len() > 20 {
        return Err(Problem("invalid managed target or cursor"));
    }
    let reference = value
        .parse::<u64>()
        .map_err(|_| Problem("invalid managed target or cursor"))?;
    if reference.to_string() != value {
        return Err(Problem("noncanonical managed target or cursor"));
    }
    Ok(reference)
}

fn deserialize_reference<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<u64, D::Error> {
    let value = String::deserialize(deserializer)?;
    parse_reference(&value).map_err(serde::de::Error::custom)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub slot: String,
    pub target: Target,
}

impl World {
    pub fn scope(&self, legacy: &str) -> Option<Scope> {
        let scope = match legacy {
            "app_header" | "app_footer" => Scope {
                slot: legacy.into(),
                target: Target::Application,
            },
            "workspace_header" => Scope {
                slot: legacy.into(),
                target: Target::Workspace {
                    reference: *self.workspaces.get(self.legacy.active_workspace)?,
                },
            },
            _ => {
                let (slot, suffix) = legacy.split_once(':')?;
                let target = match slot {
                    "workspace_summary" => Target::Workspace {
                        reference: *self.workspaces.get(suffix.parse::<usize>().ok()?)?,
                    },
                    "pane_header" | "pane_footer" => {
                        let reference = suffix.parse().ok()?;
                        if !self
                            .legacy
                            .terminals
                            .iter()
                            .any(|terminal| terminal.pane == reference)
                        {
                            return None;
                        }
                        Target::Pane { reference }
                    }
                    _ => return None,
                };
                Scope {
                    slot: slot.into(),
                    target,
                }
            }
        };
        Some(scope)
    }

    pub fn resolve(&self, scope: &Scope) -> Option<String> {
        match scope.target {
            Target::Application => matches!(scope.slot.as_str(), "app_header" | "app_footer")
                .then(|| scope.slot.clone()),
            Target::Workspace { reference } => {
                let index = self
                    .workspaces
                    .iter()
                    .position(|workspace| *workspace == reference)?;
                match scope.slot.as_str() {
                    "workspace_summary" => Some(format!("workspace_summary:{index}")),
                    "workspace_header" if index == self.legacy.active_workspace => {
                        Some("workspace_header".into())
                    }
                    _ => None,
                }
            }
            Target::Pane { reference } => self
                .legacy
                .terminals
                .iter()
                .any(|terminal| {
                    terminal.pane == reference
                        && matches!(scope.slot.as_str(), "pane_header" | "pane_footer")
                })
                .then(|| format!("{}:{reference}", scope.slot)),
        }
    }

    pub fn context(&self, scope: &Scope, grants: &BTreeSet<String>) -> Option<Value> {
        self.page(scope, grants, None)
    }

    pub fn page(
        &self,
        scope: &Scope,
        grants: &BTreeSet<String>,
        cursor: Option<u64>,
    ) -> Option<Value> {
        self.resolve(scope)?;
        let terminals = grants.contains("context.terminals");
        if !terminals && scope.target != Target::Application {
            return None;
        }
        let mut context = Map::new();
        context.insert("session".into(), json!(self.session));
        if !terminals {
            return Some(Value::Object(context));
        }
        let mut selected = Vec::new();
        let mut next = None;
        let mut more = false;
        for terminal in self
            .legacy
            .terminals
            .iter()
            .filter(|terminal| cursor.is_none_or(|cursor| terminal.pane > cursor))
            .filter(|terminal| match scope.target {
                Target::Application => true,
                Target::Workspace { reference } => {
                    self.workspaces.get(terminal.workspace) == Some(&reference)
                }
                Target::Pane { reference } => terminal.pane == reference,
            })
        {
            if selected.len() >= PAGE_TERMINALS {
                more = true;
                break;
            }
            let mut entry = Map::new();
            let mut unavailable = Vec::new();
            entry.insert("pane".into(), json!(terminal.pane.to_string()));
            if let Some(reference) = self.workspaces.get(terminal.workspace) {
                entry.insert("workspace".into(), json!(reference.to_string()));
            }
            if grants.contains("context.cwd") {
                insert_text(&mut entry, &mut unavailable, "cwd", terminal.cwd.as_deref());
            }
            if grants.contains("context.title") {
                insert_text(
                    &mut entry,
                    &mut unavailable,
                    "title",
                    terminal.title.as_deref(),
                );
            }
            if grants.contains("context.process") {
                entry.insert("running".into(), json!(terminal.running));
            }
            if !unavailable.is_empty() {
                entry.insert("unavailable".into(), json!(unavailable));
            }
            selected.push(Value::Object(entry));
            if data::serialized_size(&selected, PAGE_BYTES).is_err() {
                selected.pop();
                more = true;
                break;
            }
            next = Some(terminal.pane.to_string());
        }
        if scope.target == Target::Application {
            context.insert(
                "active_workspace".into(),
                json!(self
                    .workspaces
                    .get(self.legacy.active_workspace)
                    .map(u64::to_string)),
            );
            context.insert(
                "focused_pane".into(),
                json!(self.legacy.focused_pane.map(|pane| pane.to_string())),
            );
        }
        context.insert("terminals".into(), Value::Array(selected));
        context.insert(
            "next_cursor".into(),
            if more { json!(next) } else { Value::Null },
        );
        Some(Value::Object(context))
    }

    pub fn event(
        &self,
        event: &EventKind,
        pane: u64,
        detail: Option<&str>,
        grants: &BTreeSet<String>,
    ) -> Option<Value> {
        if !grants.contains("events.terminal") || !grants.contains("context.terminals") {
            return None;
        }
        let terminal = self
            .legacy
            .terminals
            .iter()
            .find(|terminal| terminal.pane == pane)?;
        if matches!(event, EventKind::ProcessExited { .. }) && !grants.contains("context.process") {
            return None;
        }
        let mut value = serde_json::to_value(event).ok()?;
        value["pane"] = json!(pane.to_string());
        value["workspace"] = json!(self.workspaces.get(terminal.workspace)?.to_string());
        if matches!(event, EventKind::WorkingDirectoryChanged) && grants.contains("context.cwd") {
            let mut fields = Map::new();
            let mut unavailable = Vec::new();
            insert_text(&mut fields, &mut unavailable, "cwd", detail);
            value["cwd"] = fields.remove("cwd").unwrap_or(Value::Null);
            if !unavailable.is_empty() {
                value["unavailable"] = json!(unavailable);
            }
        }
        Some(value)
    }
}

fn insert_text(
    output: &mut Map<String, Value>,
    unavailable: &mut Vec<&'static str>,
    key: &'static str,
    value: Option<&str>,
) {
    let accepted = value
        .filter(|text| text.len() <= 4096)
        .map(|text| json!(text))
        .filter(|value| data::serialized_size(value, 8192).is_ok());
    if value.is_some() && accepted.is_none() {
        unavailable.push(key);
    }
    output.insert(key.into(), accepted.unwrap_or(Value::Null));
}

pub fn granted_features(
    manifest: &Manifest,
    receipt: &super::inventory::Receipt,
    local: Option<&Value>,
) -> Result<BTreeSet<String>, Problem> {
    manifest.compatibility(MAJOR, MINOR, FEATURES)?;
    let review = local
        .and_then(|local| local.get("review"))
        .ok_or(Problem("explicit plugin access review is required"))?;
    if !receipt.matches_review(review) {
        return Err(Problem(
            "publisher or repository identity requires renewed review",
        ));
    }
    let grants = review
        .get("features")
        .and_then(Value::as_array)
        .filter(|grants| grants.len() <= 32)
        .ok_or(Problem("invalid retained plugin grants"))?;
    let mut selected = BTreeSet::new();
    for grant in grants {
        let feature = grant
            .as_str()
            .ok_or(Problem("invalid retained plugin grant"))?;
        if FEATURES.contains(&feature)
            && manifest
                .required_features
                .iter()
                .chain(&manifest.optional_features)
                .any(|declared| declared == feature)
        {
            selected.insert(feature.to_string());
        }
    }
    if manifest
        .required_features
        .iter()
        .any(|feature| !selected.contains(feature))
    {
        return Err(Problem("required plugin access has not been granted"));
    }
    feature_dependencies(&selected)?;
    Ok(selected)
}

pub fn feature_dependencies(selected: &BTreeSet<String>) -> Result<(), Problem> {
    if selected.iter().any(|feature| {
        matches!(
            feature.as_str(),
            "context.cwd" | "context.title" | "context.process" | "events.terminal"
        )
    }) && !selected.contains("context.terminals")
    {
        return Err(Problem(
            "terminal field and event grants require terminal context",
        ));
    }
    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    ManagedHello {
        major: u32,
        minor: u32,
        identity: String,
        session: String,
        generation: String,
        features: BTreeSet<String>,
    },
    Status {
        session: String,
        generation: String,
        request_id: u64,
        scope: Scope,
        context: Value,
    },
    Configure {
        session: String,
        generation: String,
        revision: u64,
        version: u32,
        values: Map<String, Value>,
    },
    ContextPage {
        session: String,
        generation: String,
        request_id: u64,
        scope: Scope,
        context: Value,
    },
    Event {
        session: String,
        generation: String,
        event: Value,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    ManagedHello {
        major: u32,
        minor: u32,
        identity: String,
        session: String,
        generation: String,
        features: BTreeSet<String>,
    },
    Status {
        session: String,
        generation: String,
        request_id: u64,
        refresh_ms: Option<u64>,
        fragments: Vec<IncomingFragment>,
    },
    Configured {
        session: String,
        generation: String,
        revision: u64,
        outcome: Outcome,
        reason: Option<Reason>,
    },
    Enumerate {
        session: String,
        generation: String,
        request_id: u64,
        scope: Scope,
        cursor: Option<String>,
    },
    Notify {},
    Error {},
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Applied,
    Rejected,
    RestartRequired,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    InvalidConfiguration,
    UnsupportedVersion,
    Unavailable,
    InternalError,
}

pub fn parse(bytes: &[u8]) -> Result<Response, Problem> {
    serde_json::from_value(data::parse_unique(
        bytes,
        super::transport::MAX_FRAME_BYTES - 1,
    )?)
    .map_err(|_| Problem("invalid managed protocol message"))
}

#[cfg(test)]
mod tests {
    use super::super::protocol::TerminalInfo;
    use super::*;

    fn world() -> World {
        World {
            session: "session-one".into(),
            workspaces: vec![10, 20],
            legacy: Context {
                active_workspace: 0,
                workspace_count: 2,
                terminals: vec![
                    TerminalInfo {
                        pane: 1,
                        workspace: 0,
                        cwd: Some("/allowed".into()),
                        title: Some("title".into()),
                        running: true,
                    },
                    TerminalInfo {
                        pane: 2,
                        workspace: 1,
                        cwd: Some("/other".into()),
                        title: None,
                        running: false,
                    },
                ],
                focused_pane: Some(1),
            },
        }
    }

    #[test]
    fn references_follow_the_original_workspace_not_a_reused_legacy_index() {
        let mut world = world();
        let scope = world.scope("workspace_summary:1").unwrap();
        world.workspaces.swap(0, 1);
        assert_eq!(world.resolve(&scope).unwrap(), "workspace_summary:0");
        world.workspaces.remove(0);
        world.workspaces.push(30);
        assert!(world.resolve(&scope).is_none());
        let header = world.scope("workspace_header").unwrap();
        world.legacy.active_workspace = 1;
        assert!(world.resolve(&header).is_none());
    }

    #[test]
    fn each_field_grant_changes_actual_serialized_context_without_sibling_leakage() {
        let world = world();
        let pane = world.scope("pane_header:1").unwrap();
        assert!(world.context(&pane, &BTreeSet::new()).is_none());
        let application = world.scope("app_header").unwrap();
        assert_eq!(
            world.context(&application, &BTreeSet::new()).unwrap(),
            json!({"session":"session-one"})
        );
        let mut grants = BTreeSet::from(["context.terminals".to_string()]);
        let context = world.context(&pane, &grants).unwrap();
        assert_eq!(context["terminals"].as_array().unwrap().len(), 1);
        for (grant, field) in [
            ("context.cwd", "cwd"),
            ("context.title", "title"),
            ("context.process", "running"),
        ] {
            assert!(context["terminals"][0].get(field).is_none());
            grants.insert(grant.into());
            let changed = world.context(&pane, &grants).unwrap();
            assert!(changed["terminals"][0].get(field).is_some());
            assert!(!changed.to_string().contains("/other"));
        }
    }

    #[test]
    fn enumeration_rejects_noncanonical_numeric_or_unknown_target_authority() {
        for reference in [
            json!(1),
            json!("01"),
            json!("+1"),
            json!("-1"),
            json!("18446744073709551616"),
        ] {
            let message = json!({"type":"enumerate","session":"session-one","generation":"generation","request_id":1,
                "scope":{"slot":"pane_header","target":{"kind":"pane","reference":reference}},"cursor":null});
            assert!(parse(message.to_string().as_bytes()).is_err());
        }
        let world = world();
        for scope in [
            Scope {
                slot: "workspace_summary".into(),
                target: Target::Application,
            },
            Scope {
                slot: "app_header".into(),
                target: Target::Pane { reference: 1 },
            },
            Scope {
                slot: "pane_header".into(),
                target: Target::Pane { reference: 99 },
            },
        ] {
            assert!(world
                .page(&scope, &BTreeSet::from(["context.terminals".into()]), None)
                .is_none());
        }
    }
}
