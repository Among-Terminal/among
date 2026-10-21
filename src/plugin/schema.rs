//! settings are validated data, not validators supplied by plugin code. retain
//! unknown stored data, but deliver only declared, supported fields.

use serde::Deserialize;
use serde_json::{json, Map, Number, Value};
use std::collections::BTreeSet;

use super::data::{bounded_text, Problem};

pub const MAX_FIELDS: usize = 64;
pub const MAX_VALUE_BYTES: usize = 32 * 1024;
const MAX_STRING_CHARS: usize = 4096;
const MAX_CHOICES: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Apply {
    Live,
    Restart,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    pub value: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Boolean,
    Integer { min: Option<i64>, max: Option<i64> },
    Number { min: Option<f64>, max: Option<f64> },
    String { max_length: usize },
    Enum { choices: Vec<Choice> },
    Secret,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub key: String,
    pub label: String,
    pub kind: Kind,
    pub required: bool,
    pub default: Option<Value>,
}

impl Field {
    pub fn available(&self) -> bool {
        !matches!(self.kind, Kind::Secret | Kind::Unsupported)
    }

    pub fn validate(&self, value: &Value) -> Result<(), Problem> {
        let valid = match &self.kind {
            Kind::Boolean => value.is_boolean(),
            Kind::Integer { min, max } => value.as_i64().is_some_and(|number| {
                min.is_none_or(|min| number >= min) && max.is_none_or(|max| number <= max)
            }),
            Kind::Number { min, max } => value.as_f64().is_some_and(|number| {
                number.is_finite()
                    && min.is_none_or(|min| number >= min)
                    && max.is_none_or(|max| number <= max)
            }),
            Kind::String { max_length } => value
                .as_str()
                .is_some_and(|text| bounded_text(text, *max_length, true)),
            Kind::Enum { choices } => value
                .as_str()
                .is_some_and(|text| choices.iter().any(|choice| choice.value == text)),
            Kind::Secret | Kind::Unsupported => {
                return Err(Problem(
                    "setting kind is unavailable; no plain-text substitute",
                ))
            }
        };
        if valid {
            Ok(())
        } else {
            Err(Problem(
                "setting value does not satisfy its declared type or bounds",
            ))
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SettingsSchema {
    pub version: u32,
    pub apply: Apply,
    pub fields: Vec<Field>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSchema {
    version: u32,
    apply: Apply,
    fields: Vec<RawField>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawField {
    key: String,
    label: String,
    kind: String,
    #[serde(default = "required")]
    required: bool,
    #[serde(default, deserialize_with = "present_value")]
    default: Option<Value>,
    min: Option<Number>,
    max: Option<Number>,
    max_length: Option<usize>,
    choices: Option<Vec<Choice>>,
}

fn required() -> bool {
    true
}
fn present_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

pub fn setting_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'.')
        })
}

impl SettingsSchema {
    pub fn parse(value: Value) -> Result<Self, Problem> {
        super::data::serialized_size(&value, super::data::MAX_STATIC_BYTES)?;
        let raw: RawSchema =
            serde_json::from_value(value).map_err(|_| Problem("invalid settings schema"))?;
        if raw.version == 0 || raw.fields.len() > MAX_FIELDS {
            return Err(Problem("invalid settings version or field count"));
        }
        let mut keys = BTreeSet::new();
        let mut fields = Vec::new();
        for field in raw.fields {
            if !setting_key(&field.key)
                || !keys.insert(field.key.clone())
                || field.label.trim().is_empty()
                || !bounded_text(&field.label, 128, false)
            {
                return Err(Problem("invalid or duplicate settings field metadata"));
            }
            let numeric = matches!(field.kind.as_str(), "integer" | "number");
            if (!numeric && (field.min.is_some() || field.max.is_some()))
                || (field.kind != "string" && field.max_length.is_some())
                || (field.kind != "enum" && field.choices.is_some())
            {
                return Err(Problem(
                    "settings constraint is not applicable to this field kind",
                ));
            }
            let kind = match field.kind.as_str() {
                "boolean" => Kind::Boolean,
                "integer" => {
                    let min = integer_bound(field.min)?;
                    let max = integer_bound(field.max)?;
                    if min.zip(max).is_some_and(|(min, max)| min > max) {
                        return Err(Problem("inverted integer bounds"));
                    }
                    Kind::Integer { min, max }
                }
                "number" => {
                    let min = number_bound(field.min)?;
                    let max = number_bound(field.max)?;
                    if min.zip(max).is_some_and(|(min, max)| min > max) {
                        return Err(Problem("inverted number bounds"));
                    }
                    Kind::Number { min, max }
                }
                "string" => {
                    let max_length = field.max_length.unwrap_or(512);
                    if max_length > MAX_STRING_CHARS {
                        return Err(Problem("string bound exceeds 4096 characters"));
                    }
                    Kind::String { max_length }
                }
                "enum" => {
                    let choices = field.choices.ok_or(Problem("enum choices are required"))?;
                    let mut values = BTreeSet::new();
                    if choices.is_empty() || choices.len() > MAX_CHOICES {
                        return Err(Problem("invalid enum choice count"));
                    }
                    for choice in &choices {
                        if !bounded_text(&choice.value, 256, false)
                            || !values.insert(choice.value.clone())
                            || choice.label.trim().is_empty()
                            || !bounded_text(&choice.label, 128, false)
                        {
                            return Err(Problem("invalid or duplicate enum choice"));
                        }
                    }
                    Kind::Enum { choices }
                }
                "secret" => Kind::Secret,
                _ => Kind::Unsupported,
            };
            let mut checked = Field {
                key: field.key,
                label: field.label,
                kind,
                required: field.required,
                default: field.default,
            };
            if checked.available() {
                if let Some(default) = &checked.default {
                    checked.validate(default)?;
                }
            } else {
                // never retain a secret default as an ordinary string to make
                // an unavailable editor look usable.
                checked.default = None;
            }
            fields.push(checked);
        }
        let result = Self {
            version: raw.version,
            apply: raw.apply,
            fields,
        };
        let defaults: Map<_, _> = result
            .fields
            .iter()
            .filter_map(|field| {
                field
                    .default
                    .clone()
                    .map(|value| (field.key.clone(), value))
            })
            .collect();
        value_budget(&defaults)?;
        Ok(result)
    }

    pub fn supported(&self) -> bool {
        self.fields
            .iter()
            .all(|field| !field.required || field.available())
    }

    pub fn effective(&self, stored: Option<&Value>) -> Result<Map<String, Value>, Problem> {
        if !self.supported() {
            return Err(Problem("required settings feature is unsupported"));
        }
        let values = self.stored_values(stored)?;
        let mut selected = Map::new();
        for field in &self.fields {
            if !field.available() {
                continue;
            }
            let value = values
                .and_then(|values| values.get(&field.key))
                .or(field.default.as_ref());
            if let Some(value) = value {
                field.validate(value)?;
                selected.insert(field.key.clone(), value.clone());
            } else if field.required {
                return Err(Problem("a required setting has no accepted value"));
            }
        }
        value_budget(&selected)?;
        Ok(selected)
    }

    fn stored_values<'a>(
        &self,
        stored: Option<&'a Value>,
    ) -> Result<Option<&'a Map<String, Value>>, Problem> {
        let Some(stored) = stored else {
            return Ok(None);
        };
        super::data::serialized_size(stored, super::data::MAX_STATIC_BYTES)?;
        if stored.get("version").and_then(Value::as_u64) != Some(u64::from(self.version)) {
            return Err(Problem(
                "stored settings version requires explicit migration",
            ));
        }
        stored
            .get("values")
            .and_then(Value::as_object)
            .map(Some)
            .ok_or(Problem("stored settings values must be an object"))
    }

    pub fn merge_edit(
        &self,
        stored: Option<&Value>,
        changes: &Map<String, Value>,
    ) -> Result<Value, Problem> {
        self.stored_values(stored)?;
        self.validate_edit(changes)?;
        let mut merged = stored
            .cloned()
            .unwrap_or_else(|| json!({"version":self.version,"values":{}}));
        let values = merged
            .get_mut("values")
            .and_then(Value::as_object_mut)
            .ok_or(Problem("stored settings values must be an object"))?;
        values.extend(changes.clone());
        self.effective(Some(&merged))?;
        self.stored_values(Some(&merged))?;
        Ok(merged)
    }

    pub fn validate_edit(&self, changes: &Map<String, Value>) -> Result<(), Problem> {
        super::data::serialized_size(changes, MAX_VALUE_BYTES)?;
        for (key, value) in changes {
            let field = self
                .fields
                .iter()
                .find(|field| &field.key == key)
                .ok_or(Problem("cannot edit an undeclared setting"))?;
            field.validate(value)?;
        }
        Ok(())
    }
}

fn integer_bound(value: Option<Number>) -> Result<Option<i64>, Problem> {
    value
        .map(|number| {
            number
                .as_i64()
                .ok_or(Problem("integer constraint must be an i64"))
        })
        .transpose()
}

fn number_bound(value: Option<Number>) -> Result<Option<f64>, Problem> {
    value
        .map(|number| {
            number
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or(Problem("number constraint must be finite"))
        })
        .transpose()
}

fn value_budget(values: &Map<String, Value>) -> Result<(), Problem> {
    super::data::serialized_size(values, MAX_VALUE_BYTES)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> SettingsSchema {
        SettingsSchema::parse(json!({"version":3,"apply":"live","fields":[
            {"key":"enabled","label":"Enabled","kind":"boolean","default":false},
            {"key":"count","label":"Count","kind":"integer","min":1,"max":5,"default":2},
            {"key":"fraction","label":"Fraction","kind":"number","min":0,"max":1,"default":0.5},
            {"key":"text","label":"Text","kind":"string","max_length":4,"default":"ok"},
            {"key":"mode","label":"Mode","kind":"enum","choices":[{"value":"a","label":"A"},{"value":"b","label":"B"}],"default":"a"}
        ]})).unwrap()
    }

    #[test]
    fn every_supported_type_and_non_default_bound_affects_delivered_values() {
        let schema = schema();
        let original = json!({"version":3,"values":{"future":{"x":1}},"extension":"retained"});
        let changes = json!({"enabled":true,"count":5,"fraction":1,"text":"四文字数","mode":"b"});
        let merged = schema
            .merge_edit(Some(&original), changes.as_object().unwrap())
            .unwrap();
        assert_eq!(merged["values"]["future"], original["values"]["future"]);
        assert_eq!(merged["extension"], "retained");
        assert_eq!(
            schema.effective(Some(&merged)).unwrap(),
            *changes.as_object().unwrap()
        );
        assert_eq!(original["values"].as_object().unwrap().len(), 1);
        for (key, value) in [
            ("enabled", json!("true")),
            ("count", json!(1.1)),
            ("count", json!(6)),
            ("fraction", json!(-0.1)),
            ("text", json!("longer")),
            ("mode", json!("c")),
        ] {
            assert!(schema
                .merge_edit(Some(&original), &Map::from_iter([(key.into(), value)]))
                .is_err());
        }
    }

    #[test]
    fn defaults_do_not_materialize_and_unknown_values_are_not_delivered() {
        let schema = schema();
        let defaults = schema.effective(None).unwrap();
        assert_eq!(defaults["count"], 2);
        let stored = json!({"version":3,"values":{"future":"keep"}});
        let before = stored.to_string();
        assert!(!schema
            .effective(Some(&stored))
            .unwrap()
            .contains_key("future"));
        assert_eq!(stored.to_string(), before);
        assert!(schema
            .effective(Some(&json!({"version":2,"values":{}})))
            .is_err());
    }

    #[test]
    fn missing_required_values_and_unknown_edits_do_not_rewrite_stored_data() {
        let schema = SettingsSchema::parse(json!({"version":1,"apply":"live","fields":[
            {"key":"required","label":"Required","kind":"string","max_length":4},
            {"key":"optional","label":"Optional","kind":"boolean","required":false}
        ]}))
        .unwrap();
        assert!(schema.effective(None).is_err());
        let original = json!({"version":1,"values":{"future":"preserved"},"opaque":{"keep":true}});
        let changed = schema
            .merge_edit(
                Some(&original),
                json!({"required":"yes"}).as_object().unwrap(),
            )
            .unwrap();
        assert_eq!(
            schema.effective(Some(&changed)).unwrap(),
            *json!({"required":"yes"}).as_object().unwrap()
        );
        assert_eq!(changed["opaque"], original["opaque"]);
        assert_eq!(changed["values"]["future"], original["values"]["future"]);
        assert!(schema
            .merge_edit(Some(&changed), json!({"unknown":"no"}).as_object().unwrap())
            .is_err());
        assert!(schema
            .merge_edit(
                Some(&changed),
                json!({"required":"too long"}).as_object().unwrap()
            )
            .is_err());
        assert!(original["values"].get("required").is_none());
    }

    #[test]
    fn secret_and_unknown_fields_remain_unavailable_without_plain_text_fallbacks() {
        for kind in ["secret", "executable-validator"] {
            for required in [false, true] {
                let schema = SettingsSchema::parse(json!({"version":1,"apply":"restart","fields":[{"key":"credential","label":"Credential","kind":kind,"required":required,"default":"must-not-leak"}]})).unwrap();
                assert!(!schema.fields[0].available());
                assert!(schema.fields[0].default.is_none());
                assert_eq!(schema.supported(), !required);
                let stored = json!({"version":1,"values":{"credential":"must-not-leak"}});
                if required {
                    assert!(!schema
                        .effective(Some(&stored))
                        .unwrap_err()
                        .to_string()
                        .contains("must-not-leak"));
                } else {
                    assert!(schema.effective(Some(&stored)).unwrap().is_empty());
                }
                assert!(schema
                    .merge_edit(
                        Some(&stored),
                        json!({"credential":"another"}).as_object().unwrap()
                    )
                    .is_err());
            }
        }
    }

    #[test]
    fn duplicate_keys_bad_constraints_null_defaults_and_aggregate_size_are_refused() {
        let field = json!({"key":"x","label":"X","kind":"boolean","default":false});
        assert!(SettingsSchema::parse(
            json!({"version":1,"apply":"live","fields":[field.clone(),field]})
        )
        .is_err());
        for field in [
            json!({"key":"x","label":"X","kind":"integer","min":5,"max":1}),
            json!({"key":"x","label":"X","kind":"boolean","min":0}),
            json!({"key":"x","label":"X","kind":"boolean","default":null}),
        ] {
            assert!(
                SettingsSchema::parse(json!({"version":1,"apply":"live","fields":[field]}))
                    .is_err()
            );
        }
        let fields: Vec<_> = (0..9).map(|index| json!({"key":format!("x{index}"),"label":"X","kind":"string","max_length":4096,"default":"x".repeat(4096)})).collect();
        assert!(
            SettingsSchema::parse(json!({"version":1,"apply":"live","fields":fields})).is_err()
        );
    }
}
