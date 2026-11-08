//! accepted policy values are separate from the losslessly merged document.
//! unknown keys survive edits; unknown versions/conditions never grant execution.

use super::{commands::Configuration, Eligibility, Policy};
use serde_json::{json, Value};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preferences {
    pub visual: Policy,
    pub audio: Policy,
    pub desktop: Policy,
    pub sound: AudioSource,
    pub command: Policy,
    pub configuration: Option<Configuration>,
    pub providers: std::collections::BTreeMap<super::providers::Key, Policy>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            visual: Policy::VISUAL_DEFAULT,
            audio: Policy::DELIVERY_DEFAULT,
            desktop: Policy::DESKTOP_DEFAULT,
            sound: AudioSource::Builtin,
            command: Policy::DELIVERY_DEFAULT,
            configuration: None,
            providers: Default::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Edit {
    Visual(Policy),
    Audio(Policy),
    Desktop(Policy),
    Sound(AudioSource),
    CommandPolicy(Policy),
    CommandConfiguration(Option<Configuration>),
    Provider(super::providers::Key, Policy),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioSource {
    Builtin,
    Local(String),
}

impl Edit {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Sound(AudioSource::Local(path)) => {
                if !Path::new(path).is_absolute() || path.contains('\0') || path.len() > 4096 {
                    return Err("Select a local absolute WAV path");
                }
            }
            Self::CommandConfiguration(Some(config)) => {
                Configuration::new(Path::new(config.executable()), config.arguments().to_vec())
                    .map_err(|_| "Invalid command executable or literal arguments")?;
            }
            Self::Provider(key, _) => {
                super::providers::Key::new(&key.identity, &key.channel)?;
            }
            _ => {}
        }
        Ok(())
    }
}

pub const CONDITIONS: [&str; 3] = [
    "always",
    "originating-terminal-unfocused",
    "application-background",
];

pub fn condition(value: Eligibility) -> &'static str {
    match value {
        Eligibility::Always => CONDITIONS[0],
        Eligibility::OriginatingTerminalUnfocused => CONDITIONS[1],
        Eligibility::ApplicationBackground => CONDITIONS[2],
    }
}

pub fn eligibility(value: &str) -> Result<Eligibility, &'static str> {
    match value {
        "always" => Ok(Eligibility::Always),
        "originating-terminal-unfocused" => Ok(Eligibility::OriginatingTerminalUnfocused),
        "application-background" => Ok(Eligibility::ApplicationBackground),
        _ => Err("Unknown notification condition; execution is disabled"),
    }
}

fn policy(value: Option<&Value>, default: Policy) -> Result<Policy, &'static str> {
    let Some(value) = value else {
        return Ok(default);
    };
    if !value.is_object() {
        return Err("Notification policy must be an object");
    }
    Ok(Policy {
        enabled: match value.get("enabled") {
            None => default.enabled,
            Some(value) => value
                .as_bool()
                .ok_or("Notification enabled must be boolean")?,
        },
        eligibility: match value.get("eligibility") {
            None => default.eligibility,
            Some(value) => eligibility(
                value
                    .as_str()
                    .ok_or("Notification condition must be text")?,
            )?,
        },
    })
}

impl Preferences {
    pub fn parse(document: &Value) -> Result<Self, &'static str> {
        if !document.is_object() || document.get("version").and_then(Value::as_u64) != Some(1) {
            return Err("Unsupported General preferences version");
        }
        let notifications = document.get("notifications");
        if notifications.is_some_and(|value| !value.is_object()) {
            return Err("Notifications must be an object");
        }
        let get = |key| notifications.and_then(|value| value.get(key));
        let visual = policy(get("visual"), Policy::VISUAL_DEFAULT)?;
        let audio = policy(get("audio"), Policy::DELIVERY_DEFAULT)?;
        let desktop = policy(get("desktop"), Policy::DESKTOP_DEFAULT)?;
        let sound = match get("audio").and_then(|value| value.get("source")) {
            None => AudioSource::Builtin,
            Some(Value::String(value)) if value == "builtin" => AudioSource::Builtin,
            Some(value) => {
                let path = value
                    .get("local")
                    .and_then(Value::as_str)
                    .filter(|path| {
                        Path::new(path).is_absolute() && !path.contains('\0') && path.len() <= 4096
                    })
                    .ok_or(
                        "Unsupported saved audio source; select a local WAV or built-in sound",
                    )?;
                AudioSource::Local(path.to_owned())
            }
        };
        let command = policy(get("command"), Policy::DELIVERY_DEFAULT)?;
        let configuration = get("command").and_then(|value| value.get("configuration"));
        let configuration = match configuration {
            None | Some(Value::Null) => None,
            Some(value) => {
                let program = value
                    .get("executable")
                    .and_then(Value::as_str)
                    .ok_or("Command executable must be an absolute path")?;
                let arguments = value
                    .get("arguments")
                    .and_then(Value::as_array)
                    .ok_or("Command arguments must be a JSON string array")?
                    .iter()
                    .map(|arg| {
                        arg.as_str()
                            .map(str::to_owned)
                            .ok_or("Command arguments must be a JSON string array")
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Some(
                    Configuration::new(Path::new(program), arguments)
                        .map_err(|_| "Invalid command executable or literal arguments")?,
                )
            }
        };
        if command.enabled && configuration.is_none() {
            return Err("Configure an executable before enabling commands");
        }
        let mut providers = std::collections::BTreeMap::new();
        if let Some(entries) = get("providers") {
            let entries = entries
                .as_object()
                .ok_or("Provider policies must be an object")?;
            for (identity, entry) in entries {
                let channels = entry
                    .get("channels")
                    .and_then(Value::as_object)
                    .ok_or("Provider channels must be an object")?;
                for (channel, value) in channels {
                    let key = super::providers::Key::new(identity, channel)?;
                    providers.insert(key, policy(Some(value), Policy::DELIVERY_DEFAULT)?);
                }
            }
        }
        Ok(Self {
            visual,
            audio,
            desktop,
            sound,
            command,
            configuration,
            providers,
        })
    }

    pub fn edit(&mut self, edit: &Edit) -> Result<(), &'static str> {
        edit.validate()?;
        match edit {
            Edit::Provider(key, policy) => {
                self.providers.insert(key.clone(), *policy);
            }
            Edit::Visual(policy) => self.visual = *policy,
            Edit::Audio(policy) => self.audio = *policy,
            Edit::Desktop(policy) => self.desktop = *policy,
            Edit::Sound(sound) => self.sound = sound.clone(),
            Edit::CommandPolicy(policy) => {
                if policy.enabled && self.configuration.is_none() {
                    return Err("Configure an executable before enabling commands");
                }
                self.command = *policy;
            }
            Edit::CommandConfiguration(configuration) => {
                self.configuration = configuration.clone();
                if configuration.is_none() {
                    self.command.enabled = false;
                }
            }
        }
        Ok(())
    }
}

pub fn merge(document: &mut Value, edit: &Edit) -> Result<(), String> {
    let mut accepted = Preferences::parse(document).map_err(str::to_owned)?;
    accepted.edit(edit).map_err(str::to_owned)?;
    let root = document
        .as_object_mut()
        .ok_or("General preferences must be an object")?;
    let notifications = root.entry("notifications").or_insert_with(|| json!({}));
    let entry = if let Edit::Provider(key, _) = edit {
        let providers = notifications
            .as_object_mut()
            .ok_or("Notifications must be an object")?
            .entry("providers")
            .or_insert_with(|| json!({}));
        let provider = providers
            .as_object_mut()
            .ok_or("Provider policies must be an object")?
            .entry(key.identity.clone())
            .or_insert_with(|| json!({"channels":{}}));
        let channels = provider
            .get_mut("channels")
            .and_then(Value::as_object_mut)
            .ok_or("Provider channels must be an object")?;
        channels
            .entry(key.channel.clone())
            .or_insert_with(|| json!({}))
    } else {
        let key = match edit {
            Edit::Visual(_) => "visual",
            Edit::Audio(_) | Edit::Sound(_) => "audio",
            Edit::Desktop(_) => "desktop",
            _ => "command",
        };
        if notifications.get(key).is_none() {
            notifications[key] = json!({});
        }
        &mut notifications[key]
    };
    match edit {
        Edit::Provider(_, policy)
        | Edit::Visual(policy)
        | Edit::Audio(policy)
        | Edit::Desktop(policy)
        | Edit::CommandPolicy(policy) => {
            entry["enabled"] = json!(policy.enabled);
            entry["eligibility"] = json!(condition(policy.eligibility));
        }
        Edit::Sound(sound) => {
            entry["source"] = match sound {
                AudioSource::Builtin => json!("builtin"),
                AudioSource::Local(path) => json!({"local":path}),
            }
        }
        Edit::CommandConfiguration(Some(config)) => {
            // preserve future fields within configuration as well as policy.
            if entry.get("configuration").is_none_or(Value::is_null) {
                entry["configuration"] = json!({});
            }
            entry["configuration"]["executable"] = json!(config.executable());
            entry["configuration"]["arguments"] = json!(config.arguments());
        }
        Edit::CommandConfiguration(None) => {
            entry["configuration"] = Value::Null;
            entry["enabled"] = json!(false);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_and_existing_documents_inherit_enabled_background_desktop_alerts() {
        for document in [
            json!({"version": 1}),
            json!({"version": 1, "notifications": {}}),
            json!({"version": 1, "notifications": {"audio": {"enabled": true}}}),
            json!({"version": 1, "notifications": {"desktop": {}}}),
        ] {
            let original = document.clone();
            let preferences = Preferences::parse(&document).unwrap();
            assert_eq!(preferences.desktop, Policy::DESKTOP_DEFAULT);
            assert!(preferences.desktop.enabled);
            assert_eq!(
                preferences.desktop.eligibility,
                Eligibility::ApplicationBackground
            );
            assert_eq!(document, original);
        }
        assert_eq!(Preferences::default().desktop, Policy::DESKTOP_DEFAULT);
    }

    #[test]
    fn desktop_edits_round_trip_without_changing_other_policies_or_unknown_keys() {
        let original = json!({
            "version": 1, "future": {"opaque": [1, 2]},
            "notifications": {
                "future": 7,
                "audio": {"enabled": true, "source": "builtin", "future": "keep"},
                "desktop": {"future": {"keep": true}}
            }
        });
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
                let mut document = original.clone();
                let before = Preferences::parse(&document).unwrap();
                merge(&mut document, &Edit::Desktop(policy)).unwrap();
                let accepted = Preferences::parse(&document).unwrap();
                let mut edited = before.clone();
                edited.edit(&Edit::Desktop(policy)).unwrap();
                assert_eq!(accepted, edited);
                assert_eq!(accepted.desktop, policy);
                assert_eq!(accepted.audio, before.audio);
                assert_eq!(accepted.visual, before.visual);
                assert_eq!(accepted.command, before.command);
                assert_eq!(
                    document["notifications"]["audio"],
                    original["notifications"]["audio"]
                );
                assert_eq!(
                    document["notifications"]["desktop"]["future"],
                    original["notifications"]["desktop"]["future"]
                );
                assert_eq!(document["notifications"]["future"], 7);
                assert_eq!(document["future"], original["future"]);
            }
        }
    }

    #[test]
    fn partial_desktop_policies_inherit_only_the_missing_parts() {
        let disabled = Preferences::parse(
            &json!({"version": 1, "notifications": {"desktop": {"enabled": false}}}),
        )
        .unwrap();
        assert_eq!(
            disabled.desktop,
            Policy {
                enabled: false,
                eligibility: Eligibility::ApplicationBackground
            }
        );
        let always = Preferences::parse(
            &json!({"version": 1, "notifications": {"desktop": {"eligibility": "always"}}}),
        )
        .unwrap();
        assert_eq!(
            always.desktop,
            Policy {
                enabled: true,
                eligibility: Eligibility::Always
            }
        );
    }

    #[test]
    fn malformed_desktop_policies_are_refused_without_repair() {
        for desktop in [
            json!(null),
            json!(false),
            json!({"enabled": "true"}),
            json!({"eligibility": 1}),
            json!({"eligibility": "future"}),
        ] {
            let mut document = json!({"version": 1, "notifications": {"desktop": desktop}});
            let original = document.clone();
            assert!(Preferences::parse(&document).is_err());
            assert!(merge(&mut document, &Edit::Desktop(Policy::DESKTOP_DEFAULT)).is_err());
            assert_eq!(document, original);
        }
    }
}
