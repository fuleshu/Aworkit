//! Durable, lossless JSON document repositories owned by the local store.
//!
//! The repository keeps the editable configuration and workflow bodies as
//! schema-versioned JSON files. A small manifest is an index only: it never
//! becomes a second editable representation of a document.

mod artifacts;
mod database;
mod document;
mod document_policy;
mod extension_inventory;
mod filesystem;
mod ledger;
mod maintenance;
mod manifest;
mod portable_journal;
mod projections;
mod repository;
mod storage;

pub use artifacts::{ArtifactMetadata, ArtifactStore, ArtifactToken};
pub use document::{DocumentKind, JsonDocument, SchemaVersion};
pub use document_policy::DocumentPolicyError;
pub use extension_inventory::{
    ExtensionInventory, ExtensionInventoryError, ExtensionInventoryMode,
};
pub use ledger::{
    Attempt, Checkpoint, CommitBatch, CommitOutcome, CommitReceipt, Deduplication, Event,
    LocalHistoryStore, OutboxEntry, PendingOutbox, StoreError,
};
pub use portable_journal::{
    PortableJournalError, PortableJournalPhase, PortableJournalRecord, PortableRuntimeJournal,
};
pub use projections::{
    ArtifactProjection, ChatSummary, EvidenceLocator, ProjectionCursor, ProjectionHealth,
    ProjectionPage, ProjectionStore, SearchHit, TimelineEntry,
};
pub use repository::{
    DocumentAccessMode, DocumentConflict, DocumentRepository, RepositoryError, RepositoryRoot,
};
pub use storage::{
    IntegrityReport, MigrationReceipt, RestoreReceipt, StorageCoordinator, StorageMode,
};
