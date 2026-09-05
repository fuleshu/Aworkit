//! Immutable web evidence, scoped to its originating Run and requested URL.
//! The receipt stream is internal artifact bookkeeping, not a second Chat transcript.

use aworkit_capability_host::{MAXIMUM_WEB_DOCUMENT_BYTES, WebDocumentV1};
use aworkit_local_store::{ArtifactStore, LocalHistoryStore};
use aworkit_protocol::{CommitBatchV1, EventV1, HistoryBackendV1, StableId};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const MAXIMUM_RETAINED_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct WebDocumentStore {
    root: PathBuf,
    gate: Arc<Mutex<()>>,
}

impl WebDocumentStore {
    /// Lazy open: an unavailable evidence disk must not prevent inline web results or app startup.
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            gate: Arc::new(Mutex::new(())),
        }
    }

    pub(crate) fn retain(
        &self,
        run: &StableId,
        document: &WebDocumentV1,
    ) -> Result<String, String> {
        let _guard = self
            .gate
            .lock()
            .map_err(|_| "web document lock unavailable")?;
        if document.text.len() > MAXIMUM_WEB_DOCUMENT_BYTES {
            return Err("web document too large".into());
        }
        let identity = serde_json::to_vec(&(run.as_str(), document)).map_err(|e| e.to_string())?;
        let id = format!("web.{:x}", Sha256::digest(&identity));
        let artifacts = ArtifactStore::open(&self.root).map_err(|e| e.to_string())?;
        if artifacts
            .metadata(&id)
            .is_ok_and(|m| m.availability == "available")
        {
            return Ok(id);
        }
        check_quota(&self.root, document.text.len() as u64)?;
        let history =
            LocalHistoryStore::open(self.root.join("history.sqlite")).map_err(|e| e.to_string())?;
        let event_id = format!("receipt.{id}");
        let token = artifacts
            .prepare(
                &id,
                "text/markdown; charset=utf-8",
                "web-document.md",
                document.text.as_bytes(),
            )
            .map_err(|e| e.to_string())?;
        let mut receipt = document.clone();
        receipt.text.clear();
        history
            .commit_v1(&CommitBatchV1 {
                backend: HistoryBackendV1::LocalSqlite,
                chat_id: stable(&id)?,
                run_id: run.clone(),
                branch_id: stable("main")?,
                expected_head: 0,
                expected_aggregate_version: 0,
                events: vec![EventV1 {
                    event_id: stable(&event_id)?,
                    schema_version: 1,
                    kind: "evidence.created".into(),
                    payload: json!({"schemaVersion":1,"runId":run,"document":receipt}),
                }],
                attempts: vec![],
                checkpoint: None,
                deduplication: None,
                outbox: vec![],
                prepared_artifacts: vec![
                    token.reference_for(&event_id).map_err(|e| e.to_string())?,
                ],
            })
            .map_err(|e| e.to_string())?;
        artifacts
            .finalize(&token, &event_id)
            .map_err(|e| e.to_string())?;
        Ok(id)
    }

    pub(crate) fn read(
        &self,
        run: &StableId,
        requested_url: &str,
        id: &str,
    ) -> Result<WebDocumentV1, String> {
        if !valid_id(id) {
            return Err("invalid web document reference".into());
        }
        let _guard = self
            .gate
            .lock()
            .map_err(|_| "web document lock unavailable")?;
        let history =
            LocalHistoryStore::open(self.root.join("history.sqlite")).map_err(|e| e.to_string())?;
        let events = history.events(id, "main").map_err(|e| e.to_string())?;
        let receipt = events
            .first()
            .ok_or("web document is unavailable on this machine")?;
        if receipt.payload["runId"].as_str() != Some(run.as_str()) {
            return Err("web document belongs to another Run".into());
        }
        let mut document: WebDocumentV1 =
            serde_json::from_value(receipt.payload["document"].clone())
                .map_err(|e| e.to_string())?;
        let requested_url = url::Url::parse(requested_url).map_err(|_| "invalid requested URL")?;
        if document.url != requested_url.as_str() {
            return Err("web document URL does not match the requested URL".into());
        }
        let artifacts = ArtifactStore::open(&self.root).map_err(|e| e.to_string())?;
        let bytes = artifacts
            .read_range(id, 0, MAXIMUM_WEB_DOCUMENT_BYTES)
            .map_err(|e| e.to_string())?;
        document.text = String::from_utf8(bytes).map_err(|_| "web document is not valid UTF-8")?;
        Ok(document)
    }
}

fn stable(value: &str) -> Result<StableId, String> {
    StableId::parse(value).map_err(|e| e.to_string())
}
pub(super) fn valid_id(id: &str) -> bool {
    id.len() == 68 && id.starts_with("web.") && id[4..].bytes().all(|c| c.is_ascii_hexdigit())
}

fn check_quota(root: &Path, additional: u64) -> Result<(), String> {
    let connection =
        rusqlite::Connection::open(root.join("history.sqlite")).map_err(|e| e.to_string())?;
    let current: i64 = connection
        .query_row(
            "SELECT (SELECT COALESCE(SUM(byte_size), 0) FROM artifacts) + (SELECT COALESCE(SUM(byte_size), 0) FROM prepared_artifacts WHERE finalized_event_id IS NULL)",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    if (current.max(0) as u64).saturating_add(additional) > MAXIMUM_RETAINED_BYTES {
        return Err("web evidence storage quota reached; preview remains available".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aworkit_capability_host::{WebDocumentMetadataV1, WebExtractionQualityV1};
    #[test]
    fn evidence_survives_restart_and_checks_run_url_and_integrity() {
        let root = tempfile::tempdir().unwrap();
        let store = WebDocumentStore::new(root.path().into());
        let run = stable("run.test").unwrap();
        let document = WebDocumentV1 {
            url: "https://example.com/".into(),
            title: "Title".into(),
            text: "αβγ immutable evidence".into(),
            bytes_downloaded: 99,
            metadata: WebDocumentMetadataV1 {
                final_url: "https://example.com/".into(),
                method: "text".into(),
                quality: WebExtractionQualityV1::Usable,
                download_truncated: false,
                snapshot_truncated: false,
                snapshot_bytes: None,
                render_settled: None,
                document_truncated: false,
                fetched_at_epoch_ms: 1,
                warnings: vec![],
            },
        };
        let id = store.retain(&run, &document).unwrap();
        assert_eq!(store.retain(&run, &document).unwrap(), id);
        let reopened = WebDocumentStore::new(root.path().into());
        assert_eq!(reopened.read(&run, &document.url, &id).unwrap(), document);
        assert!(
            reopened
                .read(&stable("run.other").unwrap(), &document.url, &id)
                .is_err()
        );
        assert!(
            reopened
                .read(&run, "https://example.com/other", &id)
                .is_err()
        );
        assert!(reopened.read(&run, &document.url, "../../escape").is_err());
        let metadata = ArtifactStore::open(root.path())
            .unwrap()
            .metadata(&id)
            .unwrap();
        std::fs::write(
            root.path()
                .join("objects")
                .join(&metadata.content_hash[..2])
                .join(&metadata.content_hash),
            b"corrupt",
        )
        .unwrap();
        assert!(reopened.read(&run, &document.url, &id).is_err());
    }
}
