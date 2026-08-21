//! Deny-by-default scrubber for portable records.

use serde_json::{Map, Value};
use thiserror::Error;

/// Explicit evidence that a field was intentionally omitted from portable data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OmissionFact {
    pub pointer: String,
    pub reason: String,
}

/// A sanitized value and the omissions made while producing it.
#[derive(Clone, Debug, PartialEq)]
pub struct ScrubbedValue {
    pub value: Value,
    pub omissions: Vec<OmissionFact>,
}

/// Stable portability policy. Unknown sensitive-looking fields fail closed.
#[derive(Clone, Copy, Debug, Default)]
pub struct ExportPolicy;

impl ExportPolicy {
    /// Scrubs debug-only fields and rejects secrets, authority, native state, and active content.
    pub fn scrub(&self, value: &Value) -> Result<ScrubbedValue, ExportError> {
        let mut omissions = Vec::new();
        let value = scrub_value(value, "", &mut omissions)?;
        Ok(ScrubbedValue { value, omissions })
    }
}

fn scrub_value(
    value: &Value,
    pointer: &str,
    omissions: &mut Vec<OmissionFact>,
) -> Result<Value, ExportError> {
    match value {
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(index, item)| scrub_value(item, &format!("{pointer}/{index}"), omissions))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(items) => {
            let mut result = Map::new();
            for (key, item) in items {
                let lower = key.to_ascii_lowercase();
                let field = format!("{pointer}/{}", key.replace('~', "~0").replace('/', "~1"));
                if ["debug", "trace", "rawprotocol", "capture"]
                    .iter()
                    .any(|word| lower.contains(word))
                {
                    omissions.push(OmissionFact {
                        pointer: field,
                        reason: "debug_capture".into(),
                    });
                    continue;
                }
                if [
                    "secret",
                    "credential",
                    "token",
                    "password",
                    "lease",
                    "approval",
                    "permit",
                    "authority",
                    "reasoning",
                    "rig",
                    "provider",
                    "native",
                    "executable",
                    "plugin",
                    "script",
                    "binary",
                    "absolute",
                    "machine",
                    "device",
                    "username",
                ]
                .iter()
                .any(|word| lower.contains(word))
                {
                    return Err(ExportError::ForbiddenField(field));
                }
                result.insert(key.clone(), scrub_value(item, &field, omissions)?);
            }
            Ok(Value::Object(result))
        }
        _ => Ok(value.clone()),
    }
}

#[derive(Debug, Error)]
pub enum ExportError {
    #[error("portable export rejected forbidden field {0}")]
    ForbiddenField(String),
}
