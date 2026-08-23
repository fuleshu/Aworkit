//! Canonical hashing for the hash-chained journal.
//!
//! Every digest is SHA-256 over RFC 8785/JCS canonical JSON, hex-encoded and
//! prefixed `sha256:`, matching the workspace-wide identity convention. Record
//! hashes bind the record to its predecessor so any rewrite breaks the chain.

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::error::BootstrapJournalError;
use super::model::JournalRecordV1;

/// Canonical SHA-256 identity of a serializable value.
pub fn canonical_hash<T: Serialize>(value: &T) -> Result<String, BootstrapJournalError> {
    let bytes = serde_jcs::to_vec(value).map_err(|_| BootstrapJournalError::Encoding)?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

/// Computes a record hash that binds the payload to its predecessor.
pub fn record_hash(record: &JournalRecordV1) -> Result<String, BootstrapJournalError> {
    canonical_hash(&(
        record.ordinal,
        &record.previous_record_hash,
        &record.payload,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::model::{BootstrapPhaseV1, JournalRecordPayloadV1, PhaseAdvanceV1};

    fn sample_record(ordinal: u64, previous: Option<String>) -> JournalRecordV1 {
        JournalRecordV1 {
            ordinal,
            previous_record_hash: previous,
            payload: JournalRecordPayloadV1::PhaseAdvance(PhaseAdvanceV1 {
                phase: BootstrapPhaseV1::Idle,
            }),
            record_hash: String::new(),
        }
    }

    #[test]
    fn record_hash_is_stable_and_binds_the_predecessor() {
        let first = sample_record(0, None);
        let first_hash = record_hash(&first).expect("hash");
        assert_eq!(record_hash(&first).expect("hash"), first_hash);

        let second_a = sample_record(1, Some(first_hash.clone()));
        let second_b = sample_record(1, Some(first_hash.clone()));
        assert_eq!(
            record_hash(&second_a).expect("hash"),
            record_hash(&second_b).expect("hash")
        );

        // A different predecessor must yield a different hash.
        let other = "sha256:".to_string() + &"0".repeat(64);
        let second_c = sample_record(1, Some(other));
        assert_ne!(
            record_hash(&second_a).expect("hash"),
            record_hash(&second_c).expect("hash")
        );
    }
}
