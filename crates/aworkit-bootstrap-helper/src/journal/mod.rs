//! Tamper-evident enrollment and activation journal (Milestone 11.1).
//!
//! This module is the helper's single durable write-ahead record for one
//! mutually exclusive managed-local enrollment or activation transaction. It
//! persists intent before every effect and the exact observation afterward,
//! drives the closed enrollment and activation phase machines, seals exactly
//! one immutable terminal receipt, and survives core termination, helper
//! restart, and power loss through its hash-chained, fenced records.
//!
//! The journal only decides *what is durable*; it does not decide which repair
//! is acceptable, launch or kill processes, execute verification, or retain
//! secrets. Those are the responsibility of the coordinator, watchdog, and
//! profile components.

mod error;
mod hashing;
mod journal;
mod model;
mod phase;
mod storage;

#[cfg(test)]
mod tests;

pub use error::BootstrapJournalError;
pub use hashing::canonical_hash;
pub use journal::{ActivationJournal, ActivationJournalPortV1};
pub use model::*;
pub use storage::{
    ArcJournalStorage, FilesystemJournalStorage, InMemoryJournalStorage, JournalStorage,
};
