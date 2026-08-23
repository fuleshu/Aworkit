//! The tamper-evident enrollment and activation journal component.
//!
//! [`ActivationJournal`] is the sole durable write-ahead record for one
//! mutually exclusive managed-local transaction. It enforces the single-flight
//! lock, the append-only hash chain, ordinal and phase fencing, the closed
//! phase machines, exactly-one terminal receipt, and crash recovery. It only
//! drives its [`JournalStorage`](super::storage::JournalStorage) port; it makes
//! no decisions about which repair is acceptable.

use std::sync::atomic::{AtomicBool, Ordering};

use aworkit_protocol::StableId;
use aworkit_trusted_core::{
    BootstrapResultKindV1, BootstrapResultV1, EnrollmentPreparedV1,
    ManagedLocalEnrollmentRequestV1, ManualRecoveryNoticeV1,
};

use super::error::BootstrapJournalError;
use super::hashing::record_hash;
use super::model::*;
use super::phase;
use super::storage::ArcJournalStorage;

/// The journal's core-facing port.
///
/// Only the bootstrap protocol gateway and the activation/rollback coordinator
/// hold operational access; there is no UI, worker, capability-host,
/// portable-store, or plugin port.
pub trait ActivationJournalPortV1: Send + Sync {
    /// Claims the single-flight maintenance lock for one transaction.
    fn acquire_single_flight(&self) -> Result<(), BootstrapJournalError>;

    fn append_enrollment_intent(
        &self,
        request: &ManagedLocalEnrollmentRequestV1,
        identities: &EnrollmentIdentitiesV1,
    ) -> Result<JournalDurableAckV1, BootstrapJournalError>;

    fn append_enrollment_observation(
        &self,
        mutation: &EnrollmentJournalMutationV1,
    ) -> Result<JournalDurableAckV1, BootstrapJournalError>;

    fn store_enrollment_prepared(
        &self,
        receipt: &EnrollmentPreparedV1,
    ) -> Result<JournalDurableAckV1, BootstrapJournalError>;

    fn append_baton_accepted(
        &self,
        baton: &BatonAcceptedV1,
    ) -> Result<JournalDurableAckV1, BootstrapJournalError>;

    fn append_effect_intent(
        &self,
        mutation: &BootstrapJournalMutationV1,
    ) -> Result<JournalDurableAckV1, BootstrapJournalError>;

    fn append_observed_effect(
        &self,
        mutation: &BootstrapJournalMutationV1,
    ) -> Result<JournalDurableAckV1, BootstrapJournalError>;

    fn advance_phase(
        &self,
        mutation: &BootstrapPhaseAdvanceV1,
    ) -> Result<JournalDurableAckV1, BootstrapJournalError>;

    /// Durably records that an authenticated, generation-fenced command was
    /// admitted, before the gateway acknowledges it.
    fn append_command_admitted(
        &self,
        record: &CommandAdmittedV1,
    ) -> Result<JournalDurableAckV1, BootstrapJournalError>;

    fn store_manual_recovery_notice(
        &self,
        notice: &ManualRecoveryNoticeV1,
    ) -> Result<JournalDurableAckV1, BootstrapJournalError>;

    fn store_bootstrap_result(
        &self,
        result: &BootstrapResultV1,
    ) -> Result<JournalDurableAckV1, BootstrapJournalError>;

    fn load_enrollment_recovery(
        &self,
        id: &StableId,
    ) -> Result<Option<EnrollmentRecoveryStateV1>, BootstrapJournalError>;

    fn load_activation_recovery(
        &self,
        id: &StableId,
    ) -> Result<Option<BootstrapRecoveryStateV1>, BootstrapJournalError>;

    fn read_enrollment_prepared(
        &self,
        id: &StableId,
    ) -> Result<EnrollmentPreparedV1, BootstrapJournalError>;

    fn read_bootstrap_result(
        &self,
        id: &StableId,
    ) -> Result<BootstrapResultV1, BootstrapJournalError>;

    /// Finalizes a sealed terminal transaction and releases the lock.
    fn seal_terminal(&self) -> Result<(), BootstrapJournalError>;
}

/// A durable, single-writer, hash-chained journal over one storage port.
pub struct ActivationJournal {
    storage: ArcJournalStorage,
    held: AtomicBool,
}

impl ActivationJournal {
    /// Wraps a storage port; the caller must claim the lock before mutating.
    #[must_use]
    pub fn new(storage: ArcJournalStorage) -> Self {
        Self {
            storage,
            held: AtomicBool::new(false),
        }
    }

    fn require_held(&self) -> Result<(), BootstrapJournalError> {
        if self.held.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(BootstrapJournalError::NotLocked)
        }
    }

    fn require_capacity(records: &[JournalRecordV1]) -> Result<(), BootstrapJournalError> {
        if u64::try_from(records.len()).expect("record length fits in u64") >= MAX_JOURNAL_RECORDS {
            Err(BootstrapJournalError::RecordCapExceeded)
        } else {
            Ok(())
        }
    }

    /// Appends `payload` at the next ordinal and durably commits it.
    fn append(
        &self,
        records: &[JournalRecordV1],
        payload: JournalRecordPayloadV1,
    ) -> Result<JournalRecordV1, BootstrapJournalError> {
        Self::require_capacity(records)?;
        let ordinal = u64::try_from(records.len()).expect("record length fits in u64");
        let previous = records.last().map(|record| record.record_hash.clone());
        let mut record = JournalRecordV1 {
            ordinal,
            previous_record_hash: previous,
            payload,
            record_hash: String::new(),
        };
        record.record_hash = record_hash(&record)?;
        self.storage.append_record(&record)?;
        Ok(record)
    }

    fn activation_id(&self) -> Result<StableId, BootstrapJournalError> {
        match self.storage.read_header()? {
            Some(header @ JournalHeaderV1::Activation(_)) => {
                Self::verify_header(&header)?;
                let JournalHeaderV1::Activation(header) = header else {
                    unreachable!()
                };
                Ok(header.activation_id)
            }
            _ => Err(BootstrapJournalError::KindConflict),
        }
    }

    fn enrollment_id(&self) -> Result<StableId, BootstrapJournalError> {
        match self.storage.read_header()? {
            Some(header @ JournalHeaderV1::Enrollment(_)) => {
                Self::verify_header(&header)?;
                let JournalHeaderV1::Enrollment(header) = header else {
                    unreachable!()
                };
                Ok(header.enrollment_id)
            }
            _ => Err(BootstrapJournalError::KindConflict),
        }
    }

    /// Verifies ordinal continuity and every record hash in the loaded chain.
    fn verify_header(header: &JournalHeaderV1) -> Result<(), BootstrapJournalError> {
        let (schema, helper, protocol, stored, expected) = match header {
            JournalHeaderV1::Enrollment(header) => (
                header.schema_version,
                header.helper_version,
                header.protocol_version,
                &header.header_hash,
                super::hashing::canonical_hash(&(
                    &header.schema_version,
                    header.enrollment_id.as_str(),
                    &header.helper_version,
                    &header.protocol_version,
                ))?,
            ),
            JournalHeaderV1::Activation(header) => (
                header.schema_version,
                header.helper_version,
                header.protocol_version,
                &header.header_hash,
                super::hashing::canonical_hash(&(
                    &header.schema_version,
                    header.activation_id.as_str(),
                    &header.baton_hash,
                    &header.profile_version,
                    &header.helper_version,
                    &header.protocol_version,
                ))?,
            ),
        };
        if schema != JOURNAL_SCHEMA_VERSION_V1
            || helper != JOURNAL_HELPER_VERSION_V1
            || protocol != JOURNAL_HELPER_VERSION_V1
            || stored != &expected
        {
            return Err(BootstrapJournalError::HeaderCorrupt);
        }
        Ok(())
    }

    fn verify_chain(
        header: &JournalHeaderV1,
        records: &[JournalRecordV1],
    ) -> Result<(), BootstrapJournalError> {
        Self::verify_header(header)?;
        if records.len() > usize::try_from(MAX_JOURNAL_RECORDS).expect("record cap fits usize") {
            return Err(BootstrapJournalError::RecordCapExceeded);
        }
        let mut previous = None;
        for (index, record) in records.iter().enumerate() {
            let expected_ordinal = u64::try_from(index).expect("record index fits in u64");
            if record.ordinal != expected_ordinal {
                return Err(BootstrapJournalError::ChainBroken {
                    ordinal: expected_ordinal,
                });
            }
            let expected_previous = if expected_ordinal == 0 {
                None
            } else {
                Some(previous.ok_or(BootstrapJournalError::ChainBroken {
                    ordinal: record.ordinal,
                })?)
            };
            if record.previous_record_hash != expected_previous {
                return Err(BootstrapJournalError::ChainBroken {
                    ordinal: record.ordinal,
                });
            }
            if record_hash(record)? != record.record_hash {
                return Err(BootstrapJournalError::ChainBroken {
                    ordinal: record.ordinal,
                });
            }
            let kind_matches = matches!(
                (header, &record.payload),
                (
                    JournalHeaderV1::Enrollment(_),
                    JournalRecordPayloadV1::EnrollmentIntent(_)
                        | JournalRecordPayloadV1::EnrollmentObservation(_)
                ) | (
                    JournalHeaderV1::Activation(_),
                    JournalRecordPayloadV1::BatonAccepted(_)
                        | JournalRecordPayloadV1::EffectIntent(_)
                        | JournalRecordPayloadV1::ObservedEffect(_)
                        | JournalRecordPayloadV1::PhaseAdvance(_)
                        | JournalRecordPayloadV1::CommandAdmitted(_)
                )
            );
            if !kind_matches {
                return Err(BootstrapJournalError::KindConflict);
            }
            previous = Some(record.record_hash.clone());
        }
        Ok(())
    }

    fn open_effect(records: &[JournalRecordV1]) -> Option<&BootstrapEffectV1> {
        let mut open = None;
        for record in records {
            match &record.payload {
                JournalRecordPayloadV1::EffectIntent(effect) => open = Some(effect),
                JournalRecordPayloadV1::ObservedEffect(_) => open = None,
                _ => {}
            }
        }
        open
    }

    fn effects_match(intent: &BootstrapEffectV1, observation: &BootstrapEffectV1) -> bool {
        intent.current_slot_hash == observation.current_slot_hash
            && intent.target_slot_hash == observation.target_slot_hash
            && intent.capability_generation == observation.capability_generation
            && intent.process_generation == observation.process_generation
    }

    fn ack(
        kind: JournalTransactionKindV1,
        id: &StableId,
        record: &JournalRecordV1,
    ) -> JournalDurableAckV1 {
        JournalDurableAckV1 {
            transaction_kind: kind,
            transaction_id: id.clone(),
            ordinal: record.ordinal,
            record_hash: record.record_hash.clone(),
        }
    }

    fn snapshot_phase(&self, records: &[JournalRecordV1]) -> JournalPhaseV1 {
        match self.storage.read_header() {
            Ok(Some(JournalHeaderV1::Enrollment(_))) => {
                JournalPhaseV1::Enrollment(derive_enrollment_phase(records))
            }
            Ok(Some(JournalHeaderV1::Activation(_))) => {
                JournalPhaseV1::Bootstrap(derive_bootstrap_phase(records))
            }
            _ => JournalPhaseV1::Bootstrap(BootstrapPhaseV1::Idle),
        }
    }

    fn write_snapshot(&self, records: &[JournalRecordV1]) -> Result<(), BootstrapJournalError> {
        let (kind, id) = match self.storage.read_header()? {
            Some(JournalHeaderV1::Enrollment(header)) => {
                (JournalTransactionKindV1::Enrollment, header.enrollment_id)
            }
            Some(JournalHeaderV1::Activation(header)) => {
                (JournalTransactionKindV1::Activation, header.activation_id)
            }
            None => return Ok(()),
        };
        let head = records.last();
        let snapshot = JournalSnapshotV1 {
            transaction_kind: kind,
            transaction_id: id,
            head_ordinal: head.map(|record| record.ordinal).unwrap_or(0),
            head_record_hash: head
                .map(|record| record.record_hash.clone())
                .unwrap_or_default(),
            phase: self.snapshot_phase(records),
        };
        self.storage.write_snapshot(&snapshot)?;
        Ok(())
    }

    /// Appends a fenced selector/process effect as an intent or an observation.
    fn append_effect(
        &self,
        mutation: &BootstrapJournalMutationV1,
        is_intent: bool,
    ) -> Result<JournalDurableAckV1, BootstrapJournalError> {
        self.require_held()?;
        let id = self.activation_id()?;
        if mutation.activation_id != id {
            return Err(BootstrapJournalError::IdentityConflict);
        }
        let records = self.storage.load_chain()?;
        let header = self
            .storage
            .read_header()?
            .ok_or(BootstrapJournalError::HeaderCorrupt)?;
        Self::verify_chain(&header, &records)?;
        Self::require_capacity(&records)?;
        if mutation.expected_ordinal != records.len() as u64 {
            return Err(BootstrapJournalError::StaleOrdinal {
                expected: mutation.expected_ordinal,
                actual: records.len() as u64,
            });
        }
        let current = derive_bootstrap_phase(&records);
        if mutation.expected_phase != current {
            return Err(BootstrapJournalError::StalePhase {
                expected: format!("{:?}", mutation.expected_phase),
                actual: format!("{current:?}"),
            });
        }
        // An intent carries no observation yet; an observation must close its
        // intent with an exact OS-state hash before any later phase advance.
        let has_observation = !mutation.effect.observation_hash.is_empty();
        if is_intent == has_observation {
            return Err(BootstrapJournalError::Invalid(
                "effect intent and observed effect must differ by their observation hash",
            ));
        }
        match (is_intent, Self::open_effect(&records)) {
            (true, Some(_)) => {
                return Err(BootstrapJournalError::Invalid(
                    "an earlier effect intent has no durable observation",
                ));
            }
            (false, Some(intent)) if !Self::effects_match(intent, &mutation.effect) => {
                return Err(BootstrapJournalError::Invalid(
                    "observed effect does not close the durable intent",
                ));
            }
            (false, None) => {
                return Err(BootstrapJournalError::Invalid(
                    "observed effect has no durable intent",
                ));
            }
            _ => {}
        }
        let payload = if is_intent {
            JournalRecordPayloadV1::EffectIntent(mutation.effect.clone())
        } else {
            JournalRecordPayloadV1::ObservedEffect(mutation.effect.clone())
        };
        let record = self.append(&records, payload)?;
        let combined: Vec<JournalRecordV1> = records
            .iter()
            .cloned()
            .chain(std::iter::once(record.clone()))
            .collect();
        self.write_snapshot(&combined)?;
        Ok(Self::ack(
            JournalTransactionKindV1::Activation,
            &id,
            &record,
        ))
    }
}

impl Drop for ActivationJournal {
    fn drop(&mut self) {
        if self.held.swap(false, Ordering::SeqCst) {
            self.storage.release_lock();
        }
    }
}

/// Replays the activation chain to recover the current bootstrap phase.
pub fn derive_bootstrap_phase(records: &[JournalRecordV1]) -> BootstrapPhaseV1 {
    let mut phase = BootstrapPhaseV1::Idle;
    for record in records {
        match &record.payload {
            JournalRecordPayloadV1::BatonAccepted(_) => phase = BootstrapPhaseV1::AdmittingBaton,
            JournalRecordPayloadV1::PhaseAdvance(advance) => phase = advance.phase,
            JournalRecordPayloadV1::EffectIntent(_)
            | JournalRecordPayloadV1::ObservedEffect(_)
            | JournalRecordPayloadV1::CommandAdmitted(_) => {}
            JournalRecordPayloadV1::EnrollmentIntent(_)
            | JournalRecordPayloadV1::EnrollmentObservation(_) => {}
        }
    }
    phase
}

/// Replays the enrollment chain to recover the current enrollment phase.
pub fn derive_enrollment_phase(records: &[JournalRecordV1]) -> EnrollmentPhaseV1 {
    let mut phase = EnrollmentPhaseV1::Intent;
    for record in records {
        if matches!(
            record.payload,
            JournalRecordPayloadV1::EnrollmentObservation(_)
        ) {
            phase = EnrollmentPhaseV1::Published;
        }
    }
    phase
}

impl ActivationJournalPortV1 for ActivationJournal {
    fn acquire_single_flight(&self) -> Result<(), BootstrapJournalError> {
        if self.held.load(Ordering::SeqCst) {
            return Err(BootstrapJournalError::Busy);
        }
        self.storage.try_acquire_lock()?;
        self.held.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn append_enrollment_intent(
        &self,
        request: &ManagedLocalEnrollmentRequestV1,
        identities: &EnrollmentIdentitiesV1,
    ) -> Result<JournalDurableAckV1, BootstrapJournalError> {
        self.require_held()?;
        let records = self.storage.load_chain()?;
        if !records.is_empty() {
            return Err(BootstrapJournalError::StaleOrdinal {
                expected: 0,
                actual: records.len() as u64,
            });
        }
        let header_hash = super::hashing::canonical_hash(&(
            &JOURNAL_SCHEMA_VERSION_V1,
            request.request_id.as_str(),
            &JOURNAL_HELPER_VERSION_V1,
            &JOURNAL_HELPER_VERSION_V1,
        ))?;
        let header = JournalHeaderV1::Enrollment(EnrollmentJournalHeaderV1 {
            schema_version: JOURNAL_SCHEMA_VERSION_V1,
            enrollment_id: request.request_id.clone(),
            helper_version: JOURNAL_HELPER_VERSION_V1,
            protocol_version: JOURNAL_HELPER_VERSION_V1,
            header_hash,
        });
        match self.storage.read_header()? {
            None => self.storage.write_header(&header)?,
            Some(existing) if existing == header => Self::verify_header(&existing)?,
            Some(_) => return Err(BootstrapJournalError::KindConflict),
        }
        let payload = JournalRecordPayloadV1::EnrollmentIntent(EnrollmentIntentV1 {
            request: request.clone(),
            managed_root_identity_hash: identities.managed_root_identity_hash.clone(),
            launcher_identity_hash: identities.launcher_identity_hash.clone(),
            journal_identity_hash: identities.journal_identity_hash.clone(),
            selector_identity_hash: identities.selector_identity_hash.clone(),
        });
        let record = self.append(&[], payload)?;
        self.write_snapshot(&[record.clone()])?;
        Ok(Self::ack(
            JournalTransactionKindV1::Enrollment,
            &request.request_id,
            &record,
        ))
    }

    fn append_enrollment_observation(
        &self,
        mutation: &EnrollmentJournalMutationV1,
    ) -> Result<JournalDurableAckV1, BootstrapJournalError> {
        self.require_held()?;
        let id = self.enrollment_id()?;
        if mutation.enrollment_id != id {
            return Err(BootstrapJournalError::IdentityConflict);
        }
        let records = self.storage.load_chain()?;
        let header = self
            .storage
            .read_header()?
            .ok_or(BootstrapJournalError::HeaderCorrupt)?;
        Self::verify_chain(&header, &records)?;
        Self::require_capacity(&records)?;
        if mutation.expected_ordinal != records.len() as u64 {
            return Err(BootstrapJournalError::StaleOrdinal {
                expected: mutation.expected_ordinal,
                actual: records.len() as u64,
            });
        }
        let current = derive_enrollment_phase(&records);
        if mutation.expected_phase != current {
            return Err(BootstrapJournalError::StalePhase {
                expected: format!("{:?}", mutation.expected_phase),
                actual: format!("{current:?}"),
            });
        }
        if !phase::enrollment_can_advance(current, EnrollmentPhaseV1::Published) {
            return Err(BootstrapJournalError::IllegalPhaseTransition {
                from: format!("{current:?}"),
                to: format!("{:?}", EnrollmentPhaseV1::Published),
            });
        }
        let record = self.append(
            &records,
            JournalRecordPayloadV1::EnrollmentObservation(mutation.observation.clone()),
        )?;
        self.write_snapshot(
            &records
                .iter()
                .cloned()
                .chain(std::iter::once(record.clone()))
                .collect::<Vec<_>>(),
        )?;
        Ok(Self::ack(
            JournalTransactionKindV1::Enrollment,
            &id,
            &record,
        ))
    }

    fn store_enrollment_prepared(
        &self,
        receipt: &EnrollmentPreparedV1,
    ) -> Result<JournalDurableAckV1, BootstrapJournalError> {
        self.require_held()?;
        let id = self.enrollment_id()?;
        if receipt.request_id != id {
            return Err(BootstrapJournalError::IdentityConflict);
        }
        if self.storage.read_receipt()?.is_some() {
            return Err(BootstrapJournalError::TerminalSealed);
        }
        let records = self.storage.load_chain()?;
        let header = self
            .storage
            .read_header()?
            .ok_or(BootstrapJournalError::HeaderCorrupt)?;
        Self::verify_chain(&header, &records)?;
        let current = derive_enrollment_phase(&records);
        if phase::enrollment_is_terminal(current) {
            return Err(BootstrapJournalError::TerminalImmutable);
        }
        if !phase::enrollment_can_advance(current, EnrollmentPhaseV1::Prepared) {
            return Err(BootstrapJournalError::StalePhase {
                expected: "published".to_owned(),
                actual: format!("{current:?}"),
            });
        }
        let last = records.last().ok_or(BootstrapJournalError::Invalid(
            "enrollment has no durable observation",
        ))?;
        let mut seal = EnrollmentTerminalSealV1 {
            receipt: receipt.clone(),
            head_ordinal: last.ordinal,
            head_record_hash: last.record_hash.clone(),
            seal_hash: String::new(),
        };
        seal.seal_hash = super::hashing::canonical_hash(&(
            &seal.receipt,
            seal.head_ordinal,
            &seal.head_record_hash,
        ))?;
        self.storage
            .seal_receipt(&TerminalReceiptV1::EnrollmentPrepared(seal))?;
        Ok(Self::ack(JournalTransactionKindV1::Enrollment, &id, last))
    }

    fn append_baton_accepted(
        &self,
        baton: &BatonAcceptedV1,
    ) -> Result<JournalDurableAckV1, BootstrapJournalError> {
        self.require_held()?;
        let records = self.storage.load_chain()?;
        if !records.is_empty() {
            return Err(BootstrapJournalError::StaleOrdinal {
                expected: 0,
                actual: records.len() as u64,
            });
        }
        let header_hash = super::hashing::canonical_hash(&(
            &JOURNAL_SCHEMA_VERSION_V1,
            baton.activation_id.as_str(),
            &baton.baton_hash,
            &baton.profile_version,
            &JOURNAL_HELPER_VERSION_V1,
            &JOURNAL_HELPER_VERSION_V1,
        ))?;
        let header = JournalHeaderV1::Activation(ActivationJournalHeaderV1 {
            schema_version: JOURNAL_SCHEMA_VERSION_V1,
            activation_id: baton.activation_id.clone(),
            baton_hash: baton.baton_hash.clone(),
            profile_version: baton.profile_version,
            helper_version: JOURNAL_HELPER_VERSION_V1,
            protocol_version: JOURNAL_HELPER_VERSION_V1,
            header_hash,
        });
        match self.storage.read_header()? {
            None => self.storage.write_header(&header)?,
            Some(existing) if existing == header => Self::verify_header(&existing)?,
            Some(_) => return Err(BootstrapJournalError::KindConflict),
        }
        let record = self.append(&[], JournalRecordPayloadV1::BatonAccepted(baton.clone()))?;
        self.write_snapshot(&[record.clone()])?;
        Ok(Self::ack(
            JournalTransactionKindV1::Activation,
            &baton.activation_id,
            &record,
        ))
    }

    fn append_effect_intent(
        &self,
        mutation: &BootstrapJournalMutationV1,
    ) -> Result<JournalDurableAckV1, BootstrapJournalError> {
        self.append_effect(mutation, true)
    }

    fn append_observed_effect(
        &self,
        mutation: &BootstrapJournalMutationV1,
    ) -> Result<JournalDurableAckV1, BootstrapJournalError> {
        self.append_effect(mutation, false)
    }

    fn advance_phase(
        &self,
        mutation: &BootstrapPhaseAdvanceV1,
    ) -> Result<JournalDurableAckV1, BootstrapJournalError> {
        self.require_held()?;
        let id = self.activation_id()?;
        if mutation.activation_id != id {
            return Err(BootstrapJournalError::IdentityConflict);
        }
        let records = self.storage.load_chain()?;
        let header = self
            .storage
            .read_header()?
            .ok_or(BootstrapJournalError::HeaderCorrupt)?;
        Self::verify_chain(&header, &records)?;
        Self::require_capacity(&records)?;
        if mutation.expected_ordinal != records.len() as u64 {
            return Err(BootstrapJournalError::StaleOrdinal {
                expected: mutation.expected_ordinal,
                actual: records.len() as u64,
            });
        }
        let current = derive_bootstrap_phase(&records);
        if mutation.expected_phase != current {
            return Err(BootstrapJournalError::StalePhase {
                expected: format!("{:?}", mutation.expected_phase),
                actual: format!("{current:?}"),
            });
        }
        if phase::bootstrap_is_terminal(current)
            && !(current == BootstrapPhaseV1::Verified
                && mutation.next_phase == BootstrapPhaseV1::Recovering)
        {
            return Err(BootstrapJournalError::TerminalImmutable);
        }
        if Self::open_effect(&records).is_some() {
            return Err(BootstrapJournalError::Invalid(
                "phase cannot advance before the intended effect is observed",
            ));
        }
        if !phase::bootstrap_can_advance(current, mutation.next_phase) {
            return Err(BootstrapJournalError::IllegalPhaseTransition {
                from: format!("{current:?}"),
                to: format!("{:?}", mutation.next_phase),
            });
        }
        let record = self.append(
            &records,
            JournalRecordPayloadV1::PhaseAdvance(PhaseAdvanceV1 {
                phase: mutation.next_phase,
            }),
        )?;
        let combined: Vec<JournalRecordV1> = records
            .iter()
            .cloned()
            .chain(std::iter::once(record.clone()))
            .collect();
        self.write_snapshot(&combined)?;
        Ok(Self::ack(
            JournalTransactionKindV1::Activation,
            &id,
            &record,
        ))
    }

    fn append_command_admitted(
        &self,
        record: &CommandAdmittedV1,
    ) -> Result<JournalDurableAckV1, BootstrapJournalError> {
        self.require_held()?;
        let id = self.activation_id()?;
        if record.activation_id != id {
            return Err(BootstrapJournalError::IdentityConflict);
        }
        let records = self.storage.load_chain()?;
        let header = self
            .storage
            .read_header()?
            .ok_or(BootstrapJournalError::HeaderCorrupt)?;
        Self::verify_chain(&header, &records)?;
        Self::require_capacity(&records)?;
        let current = derive_bootstrap_phase(&records);
        if phase::bootstrap_is_terminal(current) {
            return Err(BootstrapJournalError::TerminalImmutable);
        }
        if record.durable_phase != current {
            return Err(BootstrapJournalError::StalePhase {
                expected: format!("{:?}", record.durable_phase),
                actual: format!("{current:?}"),
            });
        }
        if let Some(existing) = records.iter().find_map(|entry| match &entry.payload {
            JournalRecordPayloadV1::CommandAdmitted(existing)
                if existing.command_id == record.command_id =>
            {
                Some(existing)
            }
            _ => None,
        }) {
            return if existing.command_hash == record.command_hash {
                Err(BootstrapJournalError::Invalid(
                    "command id is already durably admitted",
                ))
            } else {
                Err(BootstrapJournalError::ChainBroken {
                    ordinal: records.len() as u64,
                })
            };
        }
        let record = self.append(
            &records,
            JournalRecordPayloadV1::CommandAdmitted(record.clone()),
        )?;
        let combined: Vec<JournalRecordV1> = records
            .iter()
            .cloned()
            .chain(std::iter::once(record.clone()))
            .collect();
        self.write_snapshot(&combined)?;
        Ok(Self::ack(
            JournalTransactionKindV1::Activation,
            &id,
            &record,
        ))
    }

    fn store_manual_recovery_notice(
        &self,
        notice: &ManualRecoveryNoticeV1,
    ) -> Result<JournalDurableAckV1, BootstrapJournalError> {
        self.require_held()?;
        let id = self.activation_id()?;
        if notice.activation_id != id {
            return Err(BootstrapJournalError::IdentityConflict);
        }
        if notice.instructions.is_empty()
            || notice.instructions.len() > MAX_RECOVERY_INSTRUCTIONS
            || notice
                .instructions
                .iter()
                .any(|instruction| instruction.is_empty() || instruction.len() > 512)
        {
            return Err(BootstrapJournalError::Invalid(
                "manual-recovery instructions exceed their bound",
            ));
        }
        let records = self.storage.load_chain()?;
        let header = self
            .storage
            .read_header()?
            .ok_or(BootstrapJournalError::HeaderCorrupt)?;
        Self::verify_chain(&header, &records)?;
        if derive_bootstrap_phase(&records) != BootstrapPhaseV1::ManualRecoveryRequired {
            return Err(BootstrapJournalError::StalePhase {
                expected: format!("{:?}", BootstrapPhaseV1::ManualRecoveryRequired),
                actual: format!("{:?}", derive_bootstrap_phase(&records)),
            });
        }
        self.storage.write_notice(notice)?;
        let last = records
            .last()
            .ok_or(BootstrapJournalError::Invalid(
                "activation has no durable record to anchor the notice",
            ))?
            .clone();
        Ok(Self::ack(JournalTransactionKindV1::Activation, &id, &last))
    }

    fn store_bootstrap_result(
        &self,
        result: &BootstrapResultV1,
    ) -> Result<JournalDurableAckV1, BootstrapJournalError> {
        self.require_held()?;
        let id = self.activation_id()?;
        if result.activation_id != id {
            return Err(BootstrapJournalError::IdentityConflict);
        }
        if !aworkit_trusted_core::bootstrap_result_hash_v1(result)
            .is_ok_and(|hash| hash == result.receipt_hash)
        {
            return Err(BootstrapJournalError::Invalid(
                "bootstrap receipt hash is invalid",
            ));
        }
        if self.storage.read_receipt()?.is_some() {
            return Err(BootstrapJournalError::TerminalSealed);
        }
        let records = self.storage.load_chain()?;
        let header = self
            .storage
            .read_header()?
            .ok_or(BootstrapJournalError::HeaderCorrupt)?;
        Self::verify_chain(&header, &records)?;
        if Self::open_effect(&records).is_some() {
            return Err(BootstrapJournalError::Invalid(
                "terminal result cannot be sealed with an unobserved effect",
            ));
        }
        let current = derive_bootstrap_phase(&records);
        if !phase::result_can_seal(current, &result.result) {
            return Err(BootstrapJournalError::StalePhase {
                expected: "phase compatible with result disposition".to_owned(),
                actual: format!("{current:?}"),
            });
        }
        let last = records
            .last()
            .ok_or(BootstrapJournalError::Invalid(
                "activation has no durable record",
            ))?
            .clone();
        let notice_hash = self
            .storage
            .read_notice()?
            .as_ref()
            .map(super::hashing::canonical_hash)
            .transpose()?;
        if matches!(
            &result.result,
            BootstrapResultKindV1::ManualRecoveryRequired { .. }
        ) && notice_hash.is_none()
        {
            return Err(BootstrapJournalError::Invalid(
                "manual-recovery result requires a durable notice",
            ));
        }
        let mut seal = BootstrapTerminalSealV1 {
            receipt: result.clone(),
            head_ordinal: last.ordinal,
            head_record_hash: last.record_hash.clone(),
            manual_recovery_notice_hash: notice_hash,
            seal_hash: String::new(),
        };
        seal.seal_hash = super::hashing::canonical_hash(&(
            &seal.receipt,
            seal.head_ordinal,
            &seal.head_record_hash,
            &seal.manual_recovery_notice_hash,
        ))?;
        self.storage
            .seal_receipt(&TerminalReceiptV1::BootstrapResult(seal))?;
        Ok(Self::ack(JournalTransactionKindV1::Activation, &id, &last))
    }

    fn load_enrollment_recovery(
        &self,
        id: &StableId,
    ) -> Result<Option<EnrollmentRecoveryStateV1>, BootstrapJournalError> {
        let header = self.storage.read_header()?;
        let Some(JournalHeaderV1::Enrollment(enrollment_header)) = &header else {
            return Ok(None);
        };
        if &enrollment_header.enrollment_id != id {
            return Ok(None);
        }
        let records = self.storage.load_chain()?;
        Self::verify_chain(header.as_ref().expect("header is present"), &records)?;
        let receipt = self.storage.read_receipt()?;
        let terminal = match receipt {
            Some(TerminalReceiptV1::EnrollmentPrepared(seal)) => {
                let expected = super::hashing::canonical_hash(&(
                    &seal.receipt,
                    seal.head_ordinal,
                    &seal.head_record_hash,
                ))?;
                let Some(head) = records.last() else {
                    return Err(BootstrapJournalError::ChainBroken { ordinal: 0 });
                };
                if seal.seal_hash != expected
                    || seal.head_ordinal != head.ordinal
                    || seal.head_record_hash != head.record_hash
                    || seal.receipt.request_id != *id
                {
                    return Err(BootstrapJournalError::ChainBroken {
                        ordinal: head.ordinal,
                    });
                }
                Some(seal.receipt)
            }
            Some(TerminalReceiptV1::BootstrapResult(_)) => {
                return Err(BootstrapJournalError::KindConflict);
            }
            None => None,
        };
        let phase = terminal
            .as_ref()
            .map(|_| EnrollmentPhaseV1::Prepared)
            .unwrap_or_else(|| derive_enrollment_phase(&records));
        Ok(Some(EnrollmentRecoveryStateV1 {
            enrollment_id: id.clone(),
            phase,
            head_ordinal: records.last().map(|record| record.ordinal).unwrap_or(0),
            head_record_hash: records
                .last()
                .map(|record| record.record_hash.clone())
                .unwrap_or_default(),
            terminal,
        }))
    }

    fn load_activation_recovery(
        &self,
        id: &StableId,
    ) -> Result<Option<BootstrapRecoveryStateV1>, BootstrapJournalError> {
        let header = self.storage.read_header()?;
        let Some(JournalHeaderV1::Activation(activation_header)) = &header else {
            return Ok(None);
        };
        if &activation_header.activation_id != id {
            return Ok(None);
        }
        let records = self.storage.load_chain()?;
        Self::verify_chain(header.as_ref().expect("header is present"), &records)?;
        let receipt = self.storage.read_receipt()?;
        let terminal = match receipt {
            Some(TerminalReceiptV1::BootstrapResult(seal)) => {
                let expected = super::hashing::canonical_hash(&(
                    &seal.receipt,
                    seal.head_ordinal,
                    &seal.head_record_hash,
                    &seal.manual_recovery_notice_hash,
                ))?;
                let Some(head) = records.last() else {
                    return Err(BootstrapJournalError::ChainBroken { ordinal: 0 });
                };
                let notice_hash = self
                    .storage
                    .read_notice()?
                    .as_ref()
                    .map(super::hashing::canonical_hash)
                    .transpose()?;
                if seal.seal_hash != expected
                    || seal.head_ordinal != head.ordinal
                    || seal.head_record_hash != head.record_hash
                    || seal.receipt.activation_id != *id
                    || seal.manual_recovery_notice_hash != notice_hash
                {
                    return Err(BootstrapJournalError::ChainBroken {
                        ordinal: head.ordinal,
                    });
                }
                Some(seal.receipt)
            }
            Some(TerminalReceiptV1::EnrollmentPrepared(_)) => {
                return Err(BootstrapJournalError::KindConflict);
            }
            None => None,
        };
        let phase = terminal
            .as_ref()
            .is_some()
            .then(|| BootstrapPhaseV1::ResultAvailable)
            .unwrap_or_else(|| derive_bootstrap_phase(&records));
        let baton = records.iter().find_map(|record| match &record.payload {
            JournalRecordPayloadV1::BatonAccepted(baton) => Some(baton.clone()),
            _ => None,
        });
        let admitted_commands = records
            .iter()
            .filter_map(|record| match &record.payload {
                JournalRecordPayloadV1::CommandAdmitted(command) => Some(command.clone()),
                _ => None,
            })
            .collect();
        Ok(Some(BootstrapRecoveryStateV1 {
            activation_id: id.clone(),
            phase,
            head_ordinal: records.last().map(|record| record.ordinal).unwrap_or(0),
            head_record_hash: records
                .last()
                .map(|record| record.record_hash.clone())
                .unwrap_or_default(),
            terminal,
            manual_recovery: self.storage.read_notice()?,
            open_effect: Self::open_effect(&records).cloned(),
            baton,
            admitted_commands,
        }))
    }

    fn read_enrollment_prepared(
        &self,
        id: &StableId,
    ) -> Result<EnrollmentPreparedV1, BootstrapJournalError> {
        let receipt = self
            .storage
            .read_receipt()?
            .ok_or(BootstrapJournalError::TerminalMissing)?;
        match receipt {
            TerminalReceiptV1::EnrollmentPrepared(seal) if seal.receipt.request_id == *id => self
                .load_enrollment_recovery(id)?
                .and_then(|state| state.terminal)
                .ok_or(BootstrapJournalError::TerminalMissing),
            TerminalReceiptV1::EnrollmentPrepared(_) => {
                Err(BootstrapJournalError::IdentityConflict)
            }
            TerminalReceiptV1::BootstrapResult(_) => Err(BootstrapJournalError::KindConflict),
        }
    }

    fn read_bootstrap_result(
        &self,
        id: &StableId,
    ) -> Result<BootstrapResultV1, BootstrapJournalError> {
        let receipt = self
            .storage
            .read_receipt()?
            .ok_or(BootstrapJournalError::TerminalMissing)?;
        match receipt {
            TerminalReceiptV1::BootstrapResult(seal) if seal.receipt.activation_id == *id => self
                .load_activation_recovery(id)?
                .and_then(|state| state.terminal)
                .ok_or(BootstrapJournalError::TerminalMissing),
            TerminalReceiptV1::BootstrapResult(_) => Err(BootstrapJournalError::IdentityConflict),
            TerminalReceiptV1::EnrollmentPrepared(_) => Err(BootstrapJournalError::KindConflict),
        }
    }

    fn seal_terminal(&self) -> Result<(), BootstrapJournalError> {
        self.require_held()?;
        match self.storage.read_header()? {
            Some(JournalHeaderV1::Enrollment(header)) => {
                let state = self
                    .load_enrollment_recovery(&header.enrollment_id)?
                    .ok_or(BootstrapJournalError::TerminalMissing)?;
                if state.terminal.is_none() {
                    return Err(BootstrapJournalError::TerminalMissing);
                }
            }
            Some(JournalHeaderV1::Activation(header)) => {
                let state = self
                    .load_activation_recovery(&header.activation_id)?
                    .ok_or(BootstrapJournalError::TerminalMissing)?;
                if state.terminal.is_none() {
                    return Err(BootstrapJournalError::TerminalMissing);
                }
            }
            None => return Err(BootstrapJournalError::TerminalMissing),
        }
        self.storage.release_lock();
        self.held.store(false, Ordering::SeqCst);
        Ok(())
    }
}
