//! Shared fail-closed redaction for noncanonical local evidence.
//!
//! Invocation secret values are replaced before bytes enter a persistence
//! queue. Structured fields known to carry credentials are rejected entirely:
//! retaining the field with a placeholder would still make it too easy for a
//! future serializer change to persist the original value by mistake.

use std::{
    collections::BTreeSet,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

const REDACTION_MARKER: &str = "[REDACTED]";
const MAX_SECRET_VALUES: usize = 256;
const MAX_SECRET_VALUE_BYTES: usize = 16 * 1024;
const MAX_FORBIDDEN_FIELDS: usize = 128;
const MAX_REDACTED_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
static SET_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The invocation-scoped set applied to capture and diagnostic payloads.
#[derive(Clone)]
pub struct RedactionSet {
    generation: u64,
    identity: Arc<str>,
    secret_values: Arc<Vec<String>>,
    forbidden_fields: Arc<BTreeSet<String>>,
}

impl fmt::Debug for RedactionSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RedactionSet")
            .field("generation", &self.generation)
            .field("identity", &self.identity)
            .field("secret_value_count", &self.secret_values.len())
            .field("forbidden_field_count", &self.forbidden_fields.len())
            .finish()
    }
}

impl Default for RedactionSet {
    fn default() -> Self {
        Self {
            generation: 0,
            identity: Arc::from(new_identity()),
            secret_values: Arc::new(Vec::new()),
            forbidden_fields: Arc::new(default_forbidden_fields()),
        }
    }
}

impl RedactionSet {
    /// Builds one bounded invocation redaction set.
    ///
    /// Field names are matched case-insensitively after `-` is normalized to
    /// `_`; the built-in denylist is always retained.
    ///
    /// # Errors
    ///
    /// Returns an error when the secret set or a field name exceeds its bound
    /// or cannot be safely normalized.
    pub fn new(
        generation: u64,
        mut secret_values: Vec<String>,
        forbidden_fields: Vec<String>,
    ) -> Result<Self, RedactionError> {
        if secret_values.len() > MAX_SECRET_VALUES {
            return Err(RedactionError::TooManySecretValues);
        }
        if forbidden_fields.len() > MAX_FORBIDDEN_FIELDS {
            return Err(RedactionError::TooManyForbiddenFields);
        }
        for value in &secret_values {
            if value.is_empty()
                || value.len() > MAX_SECRET_VALUE_BYTES
                || value.contains('\0')
                || value.contains(REDACTION_MARKER)
                || REDACTION_MARKER.contains(value)
            {
                return Err(RedactionError::InvalidSecretValue);
            }
        }
        // Longer values must win over prefixes, and duplicates add no safety.
        secret_values
            .sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
        secret_values.dedup();

        let mut fields = default_forbidden_fields();
        for field in forbidden_fields {
            let normalized = normalize_field(&field)?;
            fields.insert(normalized);
        }
        if fields.len() > MAX_FORBIDDEN_FIELDS {
            return Err(RedactionError::TooManyForbiddenFields);
        }
        Ok(Self {
            generation,
            identity: Arc::from(new_identity()),
            secret_values: Arc::new(secret_values),
            forbidden_fields: Arc::new(fields),
        })
    }

    /// Identifies the immutable policy generation recorded by evidence stores.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns the unforgeable-by-construction identity shared by clones of
    /// this exact invocation redaction set.
    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// Redacts a complete UTF-8 frame and rejects credential-bearing fields.
    ///
    /// Detailed capture intentionally does not accept arbitrary binary frames:
    /// unsupported payloads end the optional capture rather than weakening the
    /// persistence boundary's ability to inspect and redact them.
    ///
    /// # Errors
    ///
    /// Returns an error for binary/NUL data, forbidden structured fields, or
    /// any payload that cannot be proven free of configured secret values.
    pub fn redact_payload(&self, payload: &[u8]) -> Result<RedactedPayload, RedactionError> {
        let text = std::str::from_utf8(payload).map_err(|_| RedactionError::UnsupportedBinary)?;
        match serde_json::from_str::<Value>(text) {
            Ok(json) => self.reject_forbidden_json_fields(&json)?,
            Err(_) if matches!(text.trim_start().as_bytes().first(), Some(b'{' | b'[')) => {
                return Err(RedactionError::MalformedStructuredPayload);
            }
            Err(_) => {
                if let Some(field) = self.find_text_field(text) {
                    return Err(RedactionError::ForbiddenField(field));
                }
            }
        }
        self.redact_text(text)
    }

    /// Redacts known invocation values in an ordinary diagnostic string.
    ///
    /// # Errors
    ///
    /// Returns an error for NUL data or when replacement cannot be completed
    /// without overflowing the bounded metadata counter.
    pub fn redact_text(&self, text: &str) -> Result<RedactedPayload, RedactionError> {
        if text.contains('\0') {
            return Err(RedactionError::NulByte);
        }
        if text.len() > MAX_REDACTED_OUTPUT_BYTES {
            return Err(RedactionError::OutputTooLarge);
        }
        let mut redacted = String::with_capacity(text.len());
        let mut replacements = 0_u64;
        let mut cursor = 0;
        while cursor < text.len() {
            let next = self
                .secret_values
                .iter()
                .filter_map(|secret| {
                    text[cursor..]
                        .find(secret)
                        .map(|offset| (cursor + offset, secret))
                })
                .min_by(|(left_offset, left_secret), (right_offset, right_secret)| {
                    left_offset
                        .cmp(right_offset)
                        .then_with(|| right_secret.len().cmp(&left_secret.len()))
                });
            let Some((position, secret)) = next else {
                redacted.push_str(&text[cursor..]);
                break;
            };
            redacted.push_str(&text[cursor..position]);
            redacted.push_str(REDACTION_MARKER);
            if redacted.len() > MAX_REDACTED_OUTPUT_BYTES {
                return Err(RedactionError::OutputTooLarge);
            }
            replacements = replacements
                .checked_add(1)
                .ok_or(RedactionError::ReplacementOverflow)?;
            cursor = position + secret.len();
        }
        if redacted.len() > MAX_REDACTED_OUTPUT_BYTES {
            return Err(RedactionError::OutputTooLarge);
        }
        if self
            .secret_values
            .iter()
            .any(|secret| redacted.contains(secret))
        {
            return Err(RedactionError::ResidualSecret);
        }
        Ok(RedactedPayload {
            bytes: redacted.into_bytes(),
            replacements,
        })
    }

    /// Rejects a structured diagnostic field before its value is considered.
    ///
    /// # Errors
    ///
    /// Returns an error when the field name is malformed or belongs to the
    /// credential-bearing denylist.
    pub fn validate_field_name(&self, field: &str) -> Result<(), RedactionError> {
        let normalized = normalize_field(field)?;
        if self.forbidden_fields.contains(&normalized) {
            return Err(RedactionError::ForbiddenField(field.to_owned()));
        }
        Ok(())
    }

    fn reject_forbidden_json_fields(&self, value: &Value) -> Result<(), RedactionError> {
        match value {
            Value::Object(object) => {
                for (key, value) in object {
                    self.validate_field_name(key)?;
                    self.reject_forbidden_json_fields(value)?;
                }
            }
            Value::Array(values) => {
                for value in values {
                    self.reject_forbidden_json_fields(value)?;
                }
            }
            Value::String(text) => {
                if let Some(field) = self.find_text_field(text) {
                    return Err(RedactionError::ForbiddenField(field));
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
        Ok(())
    }

    fn find_text_field(&self, text: &str) -> Option<String> {
        let lower = normalized_field_text(text);
        self.forbidden_fields.iter().find_map(|field| {
            let mut start = 0;
            while let Some(relative) = lower[start..].find(field) {
                let position = start + relative;
                let before = lower[..position].chars().next_back();
                let after_index = position + field.len();
                let after = lower[after_index..].chars().next();
                let boundary_before = before
                    .is_none_or(|character| !character.is_ascii_alphanumeric() && character != '_');
                let boundary_after = after
                    .is_none_or(|character| !character.is_ascii_alphanumeric() && character != '_');
                if boundary_before && boundary_after {
                    let suffix = lower[after_index..].trim_start_matches(|character: char| {
                        character.is_ascii_whitespace() || matches!(character, '"' | '\'')
                    });
                    if suffix.starts_with(':') || suffix.starts_with('=') {
                        return Some(field.clone());
                    }
                }
                start = after_index;
            }
            None
        })
    }
}

/// Bytes safe to enqueue for noncanonical persistence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RedactedPayload {
    bytes: Vec<u8>,
    replacements: u64,
}

impl RedactedPayload {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    #[must_use]
    pub fn replacements(&self) -> u64 {
        self.replacements
    }
}

/// A fail-closed redaction failure. Callers should drop diagnostics or truncate
/// optional detailed capture rather than persisting the rejected bytes.
#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum RedactionError {
    #[error("redaction set contains too many secret values")]
    TooManySecretValues,
    #[error("redaction set contains too many forbidden field names")]
    TooManyForbiddenFields,
    #[error("secret values must be bounded non-empty text without NUL or the redaction marker")]
    InvalidSecretValue,
    #[error("structured field name is invalid")]
    InvalidFieldName,
    #[error("credential-bearing field `{0}` is forbidden in persisted evidence")]
    ForbiddenField(String),
    #[error("binary payload cannot be safely redacted by this store")]
    UnsupportedBinary,
    #[error("structured payload is malformed and cannot be safely inspected")]
    MalformedStructuredPayload,
    #[error("payload contains a NUL byte")]
    NulByte,
    #[error("redaction replacement count overflowed")]
    ReplacementOverflow,
    #[error("redacted payload exceeds the hard output bound")]
    OutputTooLarge,
    #[error("a configured secret remained after redaction")]
    ResidualSecret,
}

fn normalize_field(field: &str) -> Result<String, RedactionError> {
    let normalized = normalized_field_text(field.trim());
    if normalized.is_empty()
        || normalized.len() > 128
        || !normalized
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(RedactionError::InvalidFieldName);
    }
    Ok(normalized)
}

fn normalized_field_text(value: &str) -> String {
    let characters = value.chars().collect::<Vec<_>>();
    let mut normalized = String::with_capacity(value.len());
    for (index, character) in characters.iter().copied().enumerate() {
        if character.is_ascii_uppercase()
            && index > 0
            && characters[index - 1].is_ascii_alphanumeric()
            && (characters[index - 1].is_ascii_lowercase()
                || characters[index - 1].is_ascii_digit()
                || characters
                    .get(index + 1)
                    .is_some_and(char::is_ascii_lowercase))
        {
            normalized.push('_');
        }
        normalized.push(if character == '-' {
            '_'
        } else {
            character.to_ascii_lowercase()
        });
    }
    normalized
}

fn default_forbidden_fields() -> BTreeSet<String> {
    [
        "authorization",
        "proxy_authorization",
        "api_key",
        "apikey",
        "access_token",
        "refresh_token",
        "client_secret",
        "password",
        "secret",
        "secret_value",
        "secret_lease",
        "credential_value",
        "environment_secret",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn new_identity() -> String {
    let sequence = SET_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let mut digest = Sha256::new();
    digest.update(std::process::id().to_le_bytes());
    digest.update(sequence.to_le_bytes());
    digest.update(timestamp.to_le_bytes());
    format!("redaction-set:{:x}", digest.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_longest_values_before_prefixes() {
        let set = RedactionSet::new(7, vec!["secret".into(), "secret-value".into()], Vec::new())
            .expect("set");
        let redacted = set
            .redact_payload(br#"{"message":"secret-value then secret"}"#)
            .expect("redaction");
        assert_eq!(redacted.replacements(), 2);
        assert_eq!(
            std::str::from_utf8(redacted.as_bytes()).expect("utf8"),
            r#"{"message":"[REDACTED] then [REDACTED]"}"#
        );
    }

    #[test]
    fn denies_known_secret_fields_in_json_and_text() {
        let set = RedactionSet::default();
        assert!(matches!(
            set.redact_payload(br#"{"authorization":"Bearer value"}"#),
            Err(RedactionError::ForbiddenField(_))
        ));
        assert!(matches!(
            set.redact_payload(b"Authorization: Bearer value"),
            Err(RedactionError::ForbiddenField(_))
        ));
        assert!(matches!(
            set.redact_payload(b"Authorization\n : Bearer value"),
            Err(RedactionError::ForbiddenField(_))
        ));
    }

    #[test]
    fn malformed_structured_payloads_fail_closed() {
        let set = RedactionSet::default();
        assert_eq!(
            set.redact_payload(b"{\"authorization\"\n : \"Bearer value\""),
            Err(RedactionError::MalformedStructuredPayload)
        );
    }

    #[test]
    fn protocol_headers_inside_json_strings_are_rejected() {
        let set = RedactionSet::default();
        assert!(matches!(
            set.redact_payload(br#"{"message":"Authorization: Bearer unknown"}"#),
            Err(RedactionError::ForbiddenField(_))
        ));
    }

    #[test]
    fn camel_case_credential_fields_are_normalized_before_matching() {
        let set = RedactionSet::default();
        assert!(matches!(
            set.redact_payload(br#"{"accessToken":"unknown"}"#),
            Err(RedactionError::ForbiddenField(_))
        ));
        assert!(matches!(
            set.redact_payload(b"clientSecret: unknown"),
            Err(RedactionError::ForbiddenField(_))
        ));
    }

    #[test]
    fn rejects_binary_payloads_instead_of_guessing() {
        assert_eq!(
            RedactionSet::default().redact_payload(&[0xff, 0x00]),
            Err(RedactionError::UnsupportedBinary)
        );
    }
}
