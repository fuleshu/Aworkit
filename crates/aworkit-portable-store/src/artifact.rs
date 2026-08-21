//! Permitted inert content-addressed portable artifacts.

use crate::{ExportPolicy, PortableError, PortablePaths};
use serde::{Deserialize, Serialize};

/// Metadata for an admitted inert artifact. Active content is never admitted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
    ) -> Result<ArtifactDescriptor, PortableError> {
        if !media_type.starts_with("text/") && media_type != "application/json" {
            return Err(PortableError::InvalidNamespace);
        }
        let text = std::str::from_utf8(bytes).map_err(|_| PortableError::InvalidNamespace)?;
        let _ = self
            .policy
            .scrub(&serde_json::Value::String(text.to_owned()))
            .map_err(|_| PortableError::InvalidNamespace)?;
        let digest = self.paths.publish("artifacts", bytes)?;
        Ok(ArtifactDescriptor {
            media_type: media_type.to_owned(),
            byte_length: u64::try_from(bytes.len()).expect("usize fits"),
            digest,
        })
    }
    pub fn read(&self, digest: &str) -> Result<Vec<u8>, PortableError> {
        self.paths.read("artifacts", digest)
    }
}
