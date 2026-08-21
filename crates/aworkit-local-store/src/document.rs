//! Types representing an editable JSON document and its stable index key.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// The two canonical JSON collections managed by this repository.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    /// Application, project, and capability configuration documents.
    Configuration,
    /// Complete workflow graph documents.
    Workflow,
}

impl DocumentKind {
    /// Returns the immutable on-disk collection name.
    #[must_use]
    pub(crate) const fn directory_name(self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::Workflow => "workflows",
        }
    }
}

/// A positively numbered schema version declared by every editable body.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SchemaVersion(pub u64);

/// One losslessly retained, schema-versioned JSON body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonDocument {
    raw_json: Vec<u8>,
    schema_version: SchemaVersion,
}

impl JsonDocument {
    /// Validates a JSON body while retaining its original accepted bytes.
    ///
    /// Keeping the raw bytes prevents a repository read/write cycle from
    /// dropping unknown node types, fields, connections, or layout metadata.
    pub fn parse(raw_json: impl Into<Vec<u8>>) -> Result<Self, DocumentValidationError> {
        let raw_json = raw_json.into();
        let value: Value = serde_json::from_slice(&raw_json)?;
        let schema_version = value
            .as_object()
            .and_then(|object| object.get("schemaVersion"))
            .and_then(Value::as_u64)
            .filter(|version| *version > 0)
            .map(SchemaVersion)
            .ok_or(DocumentValidationError::MissingSchemaVersion)?;
        Ok(Self {
            raw_json,
            schema_version,
        })
    }

    /// Returns the unmodified canonical JSON bytes.
    #[must_use]
    pub fn raw_json(&self) -> &[u8] {
        &self.raw_json
    }

    /// Parses the retained body for callers that need to inspect known fields.
    pub fn value(&self) -> Result<Value, DocumentValidationError> {
        Ok(serde_json::from_slice(&self.raw_json)?)
    }

    /// Returns the required schema version declared by the JSON body.
    #[must_use]
    pub const fn schema_version(&self) -> SchemaVersion {
        self.schema_version
    }
}

/// Validation failures that leave existing canonical documents untouched.
#[derive(Debug, Error)]
pub enum DocumentValidationError {
    /// The supplied bytes are not one complete JSON value.
    #[error("document is not valid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    /// Editable documents must identify the schema used to interpret them.
    #[error("document must contain a positive integer schemaVersion field")]
    MissingSchemaVersion,
}
