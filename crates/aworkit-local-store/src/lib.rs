//! Durable, lossless JSON document repositories owned by the local store.
//!
//! The repository keeps the editable configuration and workflow bodies as
//! schema-versioned JSON files. A small manifest is an index only: it never
//! becomes a second editable representation of a document.

mod artifacts;
mod bounded_codec;
mod database;
mod debug_capture;
mod diagnostics;
mod document;
mod document_policy;
mod extension_inventory;
mod filesystem;
mod ledger;
mod maintenance;
mod manifest;
mod portable_journal;
mod projections;
mod redaction;
mod repair_ledger;
mod repository;
mod storage;

pub use artifacts::{ArtifactMetadata, ArtifactStore, ArtifactToken};
pub use debug_capture::{
    CaptureAppendOutcome, CaptureChunk, CaptureChunkMetadata, CaptureCorrelation, CaptureError,
    CaptureFrame, CaptureManifest, CapturePage, CapturePolicy, CaptureReader, CaptureRequest,
    CaptureSource, CaptureState, CaptureStoreMode, DebugCaptureStore, RetentionReport,
};
pub use diagnostics::{
    DiagnosticCorrelation, DiagnosticCursor, DiagnosticDropReason, DiagnosticError,
    DiagnosticHealth, DiagnosticInput, DiagnosticLogConfig, DiagnosticLogStore, DiagnosticPage,
    DiagnosticRecord, DiagnosticRecordId, DiagnosticRetentionReport, DiagnosticSegmentMetadata,
    DiagnosticSegmentState, DiagnosticSeverity, DiagnosticUnavailableRange, DiagnosticWriteOutcome,
};
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
pub use redaction::{RedactedPayload, RedactionError, RedactionSet};
pub use repair_ledger::{
    ActivateCandidateRequest, ActivationEligibility, CandidateDisclosure, CandidateEvidence,
    CoreEventAppendBatchReceipt, CoreEventAppendBatchRequest, CoreEventAppendReceipt,
    CoreEventAppendRequest, CoreEventInput, CoreEventVersions, DiagnosisRecord,
    DiagnosticEvidenceReference, ErrorGroup, ErrorGroupStatus, ErrorOccurrence,
    EvidenceAvailability, EvidenceReference, EvidenceTombstone, LedgerAppendRequest,
    OccurrenceReceipt, PrepareCandidateRequest, RecordOccurrenceRequest, RegressionRecord,
    RejectionRecord, RepairCandidate, RepairEvidenceLedger, RepairIntegrityReport,
    RepairLedgerError, RepairLedgerMode, RepairTransition, RestartBaton, RollbackPoint,
    RollbackRecord, StoredCoreEvent, VerificationOutcome, VerificationRecord, VerificationStart,
    WorkaroundRecord,
};
pub use repository::{
    DocumentAccessMode, DocumentConflict, DocumentRepository, RepositoryError, RepositoryRoot,
};
pub use storage::{
    IntegrityReport, MigrationReceipt, RestoreReceipt, StorageCoordinator, StorageMode,
};
