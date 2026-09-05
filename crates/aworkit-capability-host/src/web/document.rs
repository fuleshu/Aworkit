//! Retrieval evidence is separate from the model preview and never silently complete.

use crate::CancellationToken;
use serde::{Deserialize, Serialize};

pub const MAXIMUM_WEB_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;

/// Transport input to the same extractor used for HTTP and rendered DOMs.
#[derive(Clone, Debug)]
pub struct WebSourceV1 {
    pub final_url: String,
    pub body: String,
    pub content_type: String,
    pub bytes_downloaded: u64,
    pub truncated: bool,
    pub warning: Option<String>,
    /// Only compatibility test transports supply an already extracted title.
    pub title: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WebExtractionQualityV1 {
    Usable,
    NeedsRendering,
    Empty,
    Blocked,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebDocumentMetadataV1 {
    pub final_url: String,
    pub method: String,
    pub quality: WebExtractionQualityV1,
    pub download_truncated: bool,
    pub snapshot_truncated: bool,
    #[serde(default)]
    pub snapshot_bytes: Option<u64>,
    pub render_settled: Option<bool>,
    pub document_truncated: bool,
    pub fetched_at_epoch_ms: u64,
    pub warnings: Vec<String>,
}

/// Full bounded extraction retained by the caller before previewing it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebDocumentV1 {
    pub url: String,
    pub title: String,
    pub text: String,
    pub bytes_downloaded: u64,
    pub metadata: WebDocumentMetadataV1,
}

/// Browser adapter captures HTML only; it does not extract or interpret page instructions.
pub trait WebRendererPort: Send + Sync {
    fn render(
        &self,
        url: &str,
        maximum_snapshot_bytes: usize,
        cancellation: &CancellationToken,
    ) -> Result<WebRenderSnapshotV1, String>;
}

#[derive(Clone, Debug)]
pub struct WebRenderSnapshotV1 {
    pub final_url: String,
    pub html: String,
    pub truncated: bool,
    pub settled: bool,
}

/// A UTF-8 prefix; the caller carries completeness in metadata, never in guessed punctuation.
pub(crate) fn prefix(value: &str, maximum: usize) -> &str {
    let mut end = value.len().min(maximum);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}
