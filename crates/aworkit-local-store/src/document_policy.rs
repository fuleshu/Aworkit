//! Security and compatibility checks for editable JSON documents.

use serde_json::Value;
use thiserror::Error;

use crate::{DocumentKind, JsonDocument};

const SUPPORTED_CONFIGURATION_SCHEMA_VERSION: u64 = 2;
const SUPPORTED_WORKFLOW_SCHEMA_VERSION: u64 = 1;

pub(crate) const fn supported_document_schema_version(kind: DocumentKind) -> u64 {
    match kind {
        DocumentKind::Configuration => SUPPORTED_CONFIGURATION_SCHEMA_VERSION,
        DocumentKind::Workflow => SUPPORTED_WORKFLOW_SCHEMA_VERSION,
    }
}

pub(crate) fn validate_editable_document(
    kind: DocumentKind,
    document: &JsonDocument,
) -> Result<(), DocumentPolicyError> {
    if document.schema_version().0 > supported_document_schema_version(kind) {
        return Err(DocumentPolicyError::ForwardSchema(
            document.schema_version().0,
        ));
    }
    validate_value(&document.value()?)
}

pub(crate) fn validate_inert_document(document: &JsonDocument) -> Result<(), DocumentPolicyError> {
    validate_value(&document.value()?)
}

fn validate_value(value: &Value) -> Result<(), DocumentPolicyError> {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                let normalized: String = key
                    .chars()
                    .filter(|character| character.is_ascii_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect();
                if matches!(
                    normalized.as_str(),
                    "password"
                        | "passwordvalue"
                        | "secret"
                        | "secretvalue"
                        | "token"
                        | "accesstoken"
                        | "refreshtoken"
                        | "apikey"
                        | "authorization"
                        | "credentialvalue"
                        | "leasematerial"
                ) {
                    return Err(DocumentPolicyError::SecretMaterial(key.clone()));
                }
                if normalized == "credentialref" {
                    let reference = value
                        .as_str()
                        .ok_or(DocumentPolicyError::InvalidCredentialReference)?;
                    validate_reference(reference)?;
                }
                validate_value(value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                validate_value(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_reference(value: &str) -> Result<(), DocumentPolicyError> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(DocumentPolicyError::InvalidCredentialReference)
    }
}

#[derive(Debug, Error)]
pub enum DocumentPolicyError {
    #[error("document schema version {0} is inspectable but not editable")]
    ForwardSchema(u64),
    #[error("document field '{0}' may contain secret material")]
    SecretMaterial(String),
    #[error("credentialRef must be a bounded opaque stable identifier")]
    InvalidCredentialReference,
    #[error(transparent)]
    InvalidDocument(#[from] crate::document::DocumentValidationError),
}
