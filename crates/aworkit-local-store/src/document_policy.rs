//! Security and compatibility checks for editable JSON documents.

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
    Ok(())
}

#[derive(Debug, Error)]
pub enum DocumentPolicyError {
    #[error("document schema version {0} is inspectable but not editable")]
    ForwardSchema(u64),
}
