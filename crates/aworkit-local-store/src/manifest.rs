//! Manifest-only metadata for locating immutable canonical JSON bodies.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::document::{DocumentKind, SchemaVersion};

/// The version of the non-editable repository index format.
pub(crate) const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// A compact lookup index; JSON bodies remain the sole editable document form.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub(crate) schema_version: u32,
    pub(crate) documents: BTreeMap<String, ManifestEntry>,
}

impl Manifest {
    /// Creates an empty index under the current manifest format.
    #[must_use]
    pub(crate) fn empty() -> Self {
        Self {
            schema_version: MANIFEST_SCHEMA_VERSION,
            documents: BTreeMap::new(),
        }
    }
}

/// Metadata required to find and compare one immutable body generation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ManifestEntry {
    pub(crate) kind: DocumentKind,
    pub(crate) document_version: u64,
    pub(crate) schema_version: SchemaVersion,
    pub(crate) content_hash: String,
    pub(crate) relative_path: String,
}
