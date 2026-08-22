//! Deny-by-default portable export policy and omission evidence.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

/// Explicit evidence that a field was intentionally omitted from portable data.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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

/// Selects a documented root vocabulary before arbitrary data is admitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortableRecordClass {
    SemanticPayload,
    Checkpoint,
    Manifest,
    ArtifactMetadata,
}

/// Stable portability policy. Unknown sensitive-looking fields and active
/// content fail closed at any nesting depth.
#[derive(Clone, Copy, Debug, Default)]
pub struct ExportPolicy;

impl ExportPolicy {
    /// Compatibility entry point for a semantic event payload.
    pub fn scrub(&self, value: &Value) -> Result<ScrubbedValue, ExportError> {
        self.scrub_record(PortableRecordClass::SemanticPayload, value)
    }

    pub fn scrub_record(
        &self,
        class: PortableRecordClass,
        value: &Value,
    ) -> Result<ScrubbedValue, ExportError> {
        validate_root(class, value)?;
        let mut omissions = Vec::new();
        let value = scrub_value(value, "", &mut omissions)?;
        omissions.sort_by(|left, right| left.pointer.cmp(&right.pointer));
        Ok(ScrubbedValue { value, omissions })
    }

    #[must_use]
    pub fn policy_hash(&self) -> &'static str {
        "sha256:7c356af29b04bfd71c164dbc5d230068371ec03a46599f755f17e14c2d636c50"
    }
}

fn validate_root(class: PortableRecordClass, value: &Value) -> Result<(), ExportError> {
    match class {
        PortableRecordClass::SemanticPayload => Ok(()),
        PortableRecordClass::Checkpoint => require_object_keys(
            value,
            &[
                "lastEventId",
                "aggregateVersion",
                "snapshotHash",
                "stateHash",
            ],
        ),
        PortableRecordClass::Manifest => require_object_keys(
            value,
            &[
                "family",
                "major",
                "minor",
                "requiredFeatures",
                "sessionId",
                "chatId",
                "runId",
                "frozenSnapshotHash",
                "canonicalBranchId",
                "exportPolicyHash",
                "branchId",
                "parentBranchId",
                "parentCheckpointHash",
                "parentHeadHash",
            ],
        ),
        PortableRecordClass::ArtifactMetadata => {
            require_object_keys(value, &["mediaType", "byteLength", "digest"])
        }
    }
}

fn require_object_keys(value: &Value, allowed: &[&str]) -> Result<(), ExportError> {
    let object = value.as_object().ok_or(ExportError::WrongRecordShape)?;
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(ExportError::UnknownField(format!("/{key}")));
        }
    }
    Ok(())
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
                let normalized = normalize_key(key);
                let field = format!("{pointer}/{}", escape_pointer(key));
                if is_omitted_field(&normalized) {
                    omissions.push(OmissionFact {
                        pointer: field,
                        reason: "local_detailed_capture".into(),
                    });
                    continue;
                }
                if is_forbidden_field(&normalized) {
                    return Err(ExportError::ForbiddenField(field));
                }
                result.insert(key.clone(), scrub_value(item, &field, omissions)?);
            }
            Ok(Value::Object(result))
        }
        Value::String(text) => {
            scan_text(pointer, text)?;
            Ok(value.clone())
        }
        _ => Ok(value.clone()),
    }
}

fn normalize_key(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_omitted_field(value: &str) -> bool {
    matches!(
        value,
        "debug"
            | "debugcapture"
            | "trace"
            | "tracecapture"
            | "rawprotocol"
            | "rawstream"
            | "tokenchunks"
            | "forensiccapture"
    )
}

fn is_forbidden_field(value: &str) -> bool {
    let exact = [
        "secret",
        "secretvalue",
        "credential",
        "credentialvalue",
        "password",
        "passtoken",
        "accesstoken",
        "refreshtoken",
        "authorization",
        "authorizationheader",
        "apikey",
        "privatekey",
        "lease",
        "leasehandle",
        "approval",
        "approvaltoken",
        "permit",
        "authority",
        "inheritedauthority",
        "reasoningraw",
        "hiddenreasoning",
        "chainofthought",
        "nativehandle",
        "nativeobject",
        "providerconfig",
        "providerendpoint",
        "environment",
        "env",
        "absolutepath",
        "machinepath",
        "username",
        "hostname",
        "deviceid",
        "executable",
        "script",
        "binary",
        "pluginbytes",
    ];
    exact.contains(&value)
        || value.ends_with("secret")
        || value.ends_with("password")
        || value.ends_with("credentialvalue")
        || value.ends_with("privatekey")
        || value.ends_with("authorizationheader")
}

fn scan_text(pointer: &str, text: &str) -> Result<(), ExportError> {
    let lower = text.to_ascii_lowercase();
    let credential_like = lower.starts_with("bearer ")
        || lower.contains("-----begin private key-----")
        || lower.contains("-----begin rsa private key-----")
        || looks_like_prefixed_token(text, "sk-")
        || looks_like_prefixed_token(text, "ghp_")
        || looks_like_prefixed_token(text, "github_pat_");
    let machine_path = text.starts_with("/home/")
        || text.starts_with("/Users/")
        || text.starts_with("\\\\")
        || (text.len() > 3
            && text.as_bytes()[0].is_ascii_alphabetic()
            && text.as_bytes()[1] == b':'
            && matches!(text.as_bytes()[2], b'\\' | b'/'));
    if credential_like {
        Err(ExportError::CredentialLikeValue(pointer.to_owned()))
    } else if machine_path {
        Err(ExportError::MachinePath(pointer.to_owned()))
    } else {
        Ok(())
    }
}

fn looks_like_prefixed_token(text: &str, prefix: &str) -> bool {
    text.match_indices(prefix).any(|(index, _)| {
        text[index + prefix.len()..]
            .bytes()
            .take_while(u8::is_ascii_alphanumeric)
            .take(20)
            .count()
            >= 16
    })
}

fn escape_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ExportError {
    #[error("portable export rejected forbidden field {0}")]
    ForbiddenField(String),
    #[error("portable export rejected unknown typed-record field {0}")]
    UnknownField(String),
    #[error("portable typed record has the wrong shape")]
    WrongRecordShape,
    #[error("portable export rejected credential-like content at {0}")]
    CredentialLikeValue(String),
    #[error("portable export rejected machine-local path at {0}")]
    MachinePath(String),
}
