//! Durable, lossless JSON document repositories owned by the local store.
//!
//! The repository keeps the editable configuration and workflow bodies as
//! schema-versioned JSON files. A small manifest is an index only: it never
//! becomes a second editable representation of a document.

mod artifacts;
mod document;
mod filesystem;
mod portable_journal;
mod ledger;
mod manifest;
mod projections;
mod repository;
mod storage;

pub use artifacts::{ArtifactMetadata, ArtifactStore, ArtifactToken};
pub use document::{DocumentKind, JsonDocument, SchemaVersion};
pub use ledger::{
    Attempt, Checkpoint, CommitBatch, CommitOutcome, CommitReceipt, Deduplication, Event,
    LocalHistoryStore, OutboxEntry, PendingOutbox, StoreError,
};
pub use projections::{ProjectionStore, TimelineEntry};
pub use portable_journal::{PortableJournalError, PortableJournalPhase, PortableJournalRecord, PortableRuntimeJournal};
pub use repository::{DocumentConflict, DocumentRepository, RepositoryError, RepositoryRoot};
pub use storage::{IntegrityReport, StorageCoordinator, StorageMode};
