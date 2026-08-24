//! Crash-consistent, secret-free credential-operation intent journal.
//!
//! Settings metadata and the operating-system credential store cannot share a
//! transaction. This journal publishes every affected opaque reference before
//! either store is mutated, so startup can reconcile an interrupted operation
//! from current Settings plus the active frozen Chat binding.

use std::collections::BTreeSet;

use aworkit_local_store::{
    DocumentAccessMode, DocumentKind, DocumentRepository, JsonDocument, RepositoryRoot,
};
use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

const JOURNAL_DOCUMENT_ID: &str = "credential-operations.desktop";
const JOURNAL_SCHEMA_VERSION: u16 = 1;
const MAXIMUM_PENDING_OPERATIONS: usize = 256;
const MAXIMUM_REFERENCES_PER_OPERATION: usize = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CredentialOperationKindV1 {
    Create,
    Replace,
    Delete,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CredentialOperationIntentV1 {
    pub operation_id: String,
    pub kind: CredentialOperationKindV1,
    pub credential_refs: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CredentialOperationJournalDocumentV1 {
    schema_version: u16,
    operations: Vec<CredentialOperationIntentV1>,
}

impl Default for CredentialOperationJournalDocumentV1 {
    fn default() -> Self {
        Self {
            schema_version: JOURNAL_SCHEMA_VERSION,
            operations: Vec::new(),
        }
    }
}

/// Test-only process termination points. Production instances never arm one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CredentialCrashPointV1 {
    BeforePut,
    AfterPutBeforeSettings,
    AfterReplacementSettingsBeforeObsoleteDelete,
    AfterDeleteMetadataBeforeStoreDelete,
}

/// An unavailable journal disables credential mutations but never prevents the
/// rest of the desktop profile from opening.
pub(crate) struct CredentialOperationJournal {
    available: Option<AvailableJournal>,
    warning: Option<String>,
    #[cfg(test)]
    crash_point: Option<CredentialCrashPointV1>,
}

struct AvailableJournal {
    repository: RepositoryRoot,
    version: u64,
    document: CredentialOperationJournalDocumentV1,
}

impl CredentialOperationJournal {
    /// Opens or creates the journal. Corruption or an unavailable filesystem is
    /// projected as a warning; existing Settings remain usable and readable.
    pub(crate) fn open(data_root: &std::path::Path) -> Self {
        match open_available(data_root) {
            Ok(available) => Self {
                available: Some(available),
                warning: None,
                #[cfg(test)]
                crash_point: None,
            },
            Err(error) => Self {
                available: None,
                warning: Some(format!(
                    "Credential maintenance is paused because its durable journal could not be opened: {error}"
                )),
                #[cfg(test)]
                crash_point: None,
            },
        }
    }

    pub(crate) fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    pub(crate) fn pending(&self) -> &[CredentialOperationIntentV1] {
        self.available
            .as_ref()
            .map_or(&[], |journal| journal.document.operations.as_slice())
    }

    /// Durably records an operation before any affected OS-store record is
    /// written or removed. Both the operation ID and references are opaque.
    pub(crate) fn begin(
        &mut self,
        kind: CredentialOperationKindV1,
        credential_refs: Vec<String>,
    ) -> Result<String, String> {
        validate_references(&credential_refs)?;
        let operation_id = random_stable_id("credential-operation")?;
        let intent = CredentialOperationIntentV1 {
            operation_id: operation_id.clone(),
            kind,
            credential_refs,
        };
        let journal = self.available_mut()?;
        if journal.document.operations.len() >= MAXIMUM_PENDING_OPERATIONS {
            return Err(format!(
                "credential maintenance has reached its {MAXIMUM_PENDING_OPERATIONS}-operation recovery limit"
            ));
        }
        let mut next = journal.document.clone();
        next.operations.push(intent);
        save(journal, next)?;
        Ok(operation_id)
    }

    /// Removes a fully reconciled intent with another atomic versioned commit.
    /// Callers must not invoke this while any affected cleanup is pending.
    pub(crate) fn finalize(&mut self, operation_id: &str) -> Result<(), String> {
        let journal = self.available_mut()?;
        let mut next = journal.document.clone();
        let prior_len = next.operations.len();
        next.operations
            .retain(|operation| operation.operation_id != operation_id);
        if next.operations.len() == prior_len {
            return Ok(());
        }
        save(journal, next)
    }

    #[cfg(test)]
    pub(crate) fn arm_crash_point(&mut self, point: CredentialCrashPointV1) {
        self.crash_point = Some(point);
    }

    /// Returns a synthetic abrupt-termination error without performing normal
    /// compensation. A reopened runtime must resolve the durable intent.
    pub(crate) fn fail_if(&mut self, point: CredentialCrashPointV1) -> Result<(), String> {
        #[cfg(test)]
        if self.crash_point == Some(point) {
            self.crash_point = None;
            return Err(format!(
                "simulated process termination at credential phase {point:?}"
            ));
        }
        #[cfg(not(test))]
        let _ = point;
        Ok(())
    }

    fn available_mut(&mut self) -> Result<&mut AvailableJournal, String> {
        self.available.as_mut().ok_or_else(|| {
            self.warning.clone().unwrap_or_else(|| {
                "credential maintenance is paused because its journal is unavailable".into()
            })
        })
    }
}

fn open_available(data_root: &std::path::Path) -> Result<AvailableJournal, String> {
    let repository = RepositoryRoot::open(data_root.join("documents"))
        .map_err(|error| format!("cannot open credential journal repository: {error}"))?;
    match repository
        .load(DocumentKind::Configuration, JOURNAL_DOCUMENT_ID)
        .map_err(|error| format!("cannot load credential journal: {error}"))?
    {
        Some(stored) => {
            if stored.access != DocumentAccessMode::Editable {
                return Err("credential journal schema is not editable".into());
            }
            let document: CredentialOperationJournalDocumentV1 =
                serde_json::from_slice(stored.document.raw_json())
                    .map_err(|error| format!("credential journal is invalid: {error}"))?;
            validate_document(&document)?;
            Ok(AvailableJournal {
                repository,
                version: stored.version,
                document,
            })
        }
        None => {
            let document = CredentialOperationJournalDocumentV1::default();
            let encoded = encode(&document)?;
            let stored = repository
                .save(
                    DocumentKind::Configuration,
                    JOURNAL_DOCUMENT_ID,
                    None,
                    &encoded,
                )
                .map_err(|error| format!("cannot create credential journal: {error}"))?;
            Ok(AvailableJournal {
                repository,
                version: stored.version,
                document,
            })
        }
    }
}

fn save(
    journal: &mut AvailableJournal,
    next: CredentialOperationJournalDocumentV1,
) -> Result<(), String> {
    validate_document(&next)?;
    let encoded = encode(&next)?;
    let stored = journal
        .repository
        .save(
            DocumentKind::Configuration,
            JOURNAL_DOCUMENT_ID,
            Some(journal.version),
            &encoded,
        )
        .map_err(|error| format!("cannot commit credential journal: {error}"))?;
    journal.version = stored.version;
    journal.document = next;
    Ok(())
}

fn encode(document: &CredentialOperationJournalDocumentV1) -> Result<JsonDocument, String> {
    let bytes = serde_jcs::to_vec(document)
        .map_err(|error| format!("cannot encode credential journal: {error}"))?;
    JsonDocument::parse(bytes).map_err(|error| format!("cannot encode credential journal: {error}"))
}

fn validate_document(document: &CredentialOperationJournalDocumentV1) -> Result<(), String> {
    if document.schema_version != JOURNAL_SCHEMA_VERSION {
        return Err(format!(
            "unsupported credential journal schema {}",
            document.schema_version
        ));
    }
    if document.operations.len() > MAXIMUM_PENDING_OPERATIONS {
        return Err("credential journal exceeds its pending-operation limit".into());
    }
    let mut operation_ids = BTreeSet::new();
    for operation in &document.operations {
        validate_operation_id(&operation.operation_id)?;
        if !operation_ids.insert(operation.operation_id.as_str()) {
            return Err("credential journal contains duplicate operation IDs".into());
        }
        validate_references(&operation.credential_refs)?;
    }
    Ok(())
}

fn validate_operation_id(value: &str) -> Result<(), String> {
    let id = StableId::parse(value.to_owned())
        .map_err(|error| format!("credential journal operation ID is invalid: {error}"))?;
    if id.as_str().starts_with("credential-operation.") {
        Ok(())
    } else {
        Err("credential journal operation ID uses an invalid namespace".into())
    }
}

fn validate_references(references: &[String]) -> Result<(), String> {
    if references.is_empty() || references.len() > MAXIMUM_REFERENCES_PER_OPERATION {
        return Err("credential operation must contain one or two affected references".into());
    }
    let mut unique = BTreeSet::new();
    for reference in references {
        let id = StableId::parse(reference.clone())
            .map_err(|error| format!("credential operation reference is invalid: {error}"))?;
        if !id.as_str().starts_with("credential.") {
            return Err("credential operation reference uses an invalid namespace".into());
        }
        if !unique.insert(reference.as_str()) {
            return Err("credential operation contains duplicate references".into());
        }
    }
    Ok(())
}

pub(crate) fn random_credential_ref() -> Result<String, String> {
    random_stable_id("credential")
}

fn random_stable_id(namespace: &str) -> Result<String, String> {
    let mut random = [0_u8; 24];
    getrandom::fill(&mut random)
        .map_err(|_| "cannot allocate an opaque credential operation identity".to_owned())?;
    let opaque = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    random.zeroize();
    let value = format!("{namespace}.{opaque}");
    StableId::parse(value.clone())
        .map_err(|error| format!("cannot allocate an opaque credential identity: {error}"))?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn journal_is_versioned_reopenable_and_contains_only_opaque_references() {
        let root = TempDir::new().unwrap();
        let mut journal = CredentialOperationJournal::open(root.path());
        let operation_id = journal
            .begin(
                CredentialOperationKindV1::Replace,
                vec!["credential.new-ref".into(), "credential.old-ref".into()],
            )
            .unwrap();
        assert_eq!(journal.pending().len(), 1);
        drop(journal);

        let mut reopened = CredentialOperationJournal::open(root.path());
        assert_eq!(reopened.pending()[0].operation_id, operation_id);
        assert_eq!(
            reopened.pending()[0].credential_refs,
            ["credential.new-ref", "credential.old-ref"]
        );
        reopened.finalize(&operation_id).unwrap();
        drop(reopened);
        assert!(
            CredentialOperationJournal::open(root.path())
                .pending()
                .is_empty()
        );

        for entry in walk(root.path()) {
            let bytes = fs::read(entry).unwrap();
            assert!(
                !bytes
                    .windows(b"plaintext-value".len())
                    .any(|window| { window == b"plaintext-value" })
            );
        }
    }

    #[test]
    fn journal_rejects_nonopaque_or_duplicate_references_before_commit() {
        let root = TempDir::new().unwrap();
        let mut journal = CredentialOperationJournal::open(root.path());
        assert!(
            journal
                .begin(CredentialOperationKindV1::Create, vec!["token.raw".into()])
                .is_err()
        );
        assert!(
            journal
                .begin(
                    CredentialOperationKindV1::Replace,
                    vec!["credential.same".into(), "credential.same".into()]
                )
                .is_err()
        );
        assert!(journal.pending().is_empty());
    }

    fn walk(root: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut files = Vec::new();
        let mut directories = vec![root.to_path_buf()];
        while let Some(directory) = directories.pop() {
            for entry in fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    directories.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        files
    }
}
