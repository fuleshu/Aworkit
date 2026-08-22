//! Explicitly admitted inert, bounded, content-addressed portable artifacts.

use crate::{ExportPolicy, PortableError, PortablePaths};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MAX_PORTABLE_ARTIFACT_BYTES: usize = 4 * 1024 * 1024;
const ALLOWED_MEDIA_TYPES: &[&str] = &[
    "text/plain",
    "text/markdown",
    "text/csv",
    "application/json",
];

/// Metadata for an admitted inert artifact. Active content is never admitted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactDescriptor {
    pub media_type: String,
    pub byte_length: u64,
    pub digest: String,
}

#[derive(Clone, Debug)]
pub struct ArtifactStore {
    paths: PortablePaths,
    policy: ExportPolicy,
}

impl ArtifactStore {
    #[must_use]
    pub fn new(paths: PortablePaths) -> Self {
        Self {
            paths,
            policy: ExportPolicy,
        }
    }

    pub fn admit_text(
        &self,
        media_type: &str,
        bytes: &[u8],
    ) -> Result<ArtifactDescriptor, ArtifactError> {
        if !ALLOWED_MEDIA_TYPES.contains(&media_type) {
            return Err(ArtifactError::ActiveOrUnknownMediaType);
        }
        if bytes.is_empty() || bytes.len() > MAX_PORTABLE_ARTIFACT_BYTES || bytes.contains(&0) {
            return Err(ArtifactError::SizeOrEncoding);
        }
        let text = std::str::from_utf8(bytes).map_err(|_| ArtifactError::SizeOrEncoding)?;
        if media_type == "application/json" {
            let value: serde_json::Value = serde_json::from_str(text)?;
            let scrubbed = self.policy.scrub(&value)?;
            if scrubbed.value != value || !scrubbed.omissions.is_empty() {
                return Err(ArtifactError::PortableRewriteRequired);
            }
        } else {
            let _ = self
                .policy
                .scrub(&serde_json::Value::String(text.to_owned()))?;
        }
        let digest = self.paths.publish("artifacts", bytes)?;
        Ok(ArtifactDescriptor {
            media_type: media_type.to_owned(),
            byte_length: u64::try_from(bytes.len()).expect("usize fits u64"),
            digest,
        })
    }

    pub fn read(&self, digest: &str) -> Result<Vec<u8>, ArtifactError> {
        let bytes = self.paths.read("artifacts", digest)?;
        if bytes.len() > MAX_PORTABLE_ARTIFACT_BYTES {
            return Err(ArtifactError::SizeOrEncoding);
        }
        Ok(bytes)
    }

    pub fn read_verified(&self, descriptor: &ArtifactDescriptor) -> Result<Vec<u8>, ArtifactError> {
        if !ALLOWED_MEDIA_TYPES.contains(&descriptor.media_type.as_str())
            || descriptor.byte_length > MAX_PORTABLE_ARTIFACT_BYTES as u64
        {
            return Err(ArtifactError::InvalidDescriptor);
        }
        let bytes = self.read(&descriptor.digest)?;
        if bytes.len() as u64 != descriptor.byte_length {
            return Err(ArtifactError::InvalidDescriptor);
        }
        Ok(bytes)
    }

    pub fn read_range(
        &self,
        descriptor: &ArtifactDescriptor,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, ArtifactError> {
        if length > 256 * 1024 {
            return Err(ArtifactError::Range);
        }
        let bytes = self.read_verified(descriptor)?;
        let start = usize::try_from(offset).map_err(|_| ArtifactError::Range)?;
        let end = start.checked_add(length).ok_or(ArtifactError::Range)?;
        bytes
            .get(start..end.min(bytes.len()))
            .map(<[u8]>::to_vec)
            .ok_or(ArtifactError::Range)
    }
}

#[derive(Debug, Error)]
pub enum ArtifactError {
    #[error("portable artifact media type can carry active or unknown content")]
    ActiveOrUnknownMediaType,
    #[error("portable artifact violates its size or UTF-8 bound")]
    SizeOrEncoding,
    #[error("portable artifact descriptor does not match its bytes")]
    InvalidDescriptor,
    #[error("portable artifact contains fields that would require omission or rewriting")]
    PortableRewriteRequired,
    #[error("portable artifact range is invalid or too large")]
    Range,
    #[error(transparent)]
    Export(#[from] crate::ExportError),
    #[error(transparent)]
    Portable(#[from] PortableError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
