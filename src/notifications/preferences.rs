//! accepted policy values are separate from the losslessly merged document.
//! unknown keys survive edits; unknown versions/conditions never grant execution.

use super::{commands::Configuration, Eligibility, Policy};
use serde_json::{json, Value};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preferences {
    pub visual: Policy,
    pub audio: Policy,
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
