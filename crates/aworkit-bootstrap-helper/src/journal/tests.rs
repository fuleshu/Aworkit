//! Hermetic tests for the activation journal over the in-memory storage double.

use std::sync::Arc;

use aworkit_protocol::{ProcessGeneration, StableId};
use aworkit_trusted_core::{
    BootstrapDeadlinesV1, BootstrapResultKindV1, BootstrapResultV1, BuildBundleRefV1,
    EnrollmentPreparedV1, FocusedVerificationEvidenceV1, ManagedLocalEnrollmentRequestV1,
    ManualRecoveryNoticeV1, PlatformReasonV1, ReasonCodeV1, RepairArtifactRefV1,
};

use super::ActivationJournalPortV1;
use super::error::BootstrapJournalError;
use super::journal::{ActivationJournal, derive_bootstrap_phase};
use super::model::*;
use super::storage::InMemoryJournalStorage;

fn id(name: &str) -> StableId {
    StableId::parse(name).expect("valid stable id")
}

fn hash(byte: u8) -> String {
    let nibbles = |byte: u8| -> String { format!("{:02x}", byte) };
    "sha256:".to_string() + &nibbles(byte).repeat(32)
}

fn artifact() -> RepairArtifactRefV1 {
    RepairArtifactRefV1 {
        artifact_id: id("artifact.1"),
        content_hash: hash(0x0a),
        byte_size: 4096,
        media_type: "application/octet-stream".to_owned(),
        logical_name: "bundle.tar".to_owned(),
    }
}

fn bundle() -> BuildBundleRefV1 {
    BuildBundleRefV1 {
        artifact: artifact(),
        manifest_relative_entry: "app/manifest.json".to_owned(),
    }
}

fn enrollment_request(request_id: &str) -> ManagedLocalEnrollmentRequestV1 {
    ManagedLocalEnrollmentRequestV1 {
        request_id: id(request_id),
        explicit_user_decision_id: id("decision.1"),
        group_id: id("group.1"),
        candidate_id: id("candidate.1"),
        candidate_version: 7,
        candidate_hash: hash(0x11),
        projected_provenance_hash: hash(0x22),
        whole_bundle: bundle(),
        capability_report_id: id("report.1"),
        capability_digest: hash(0x33),
    }
}

fn identities() -> EnrollmentIdentitiesV1 {
    EnrollmentIdentitiesV1 {
        managed_root_identity_hash: hash(0x44),
        launcher_identity_hash: hash(0x55),
        journal_identity_hash: hash(0x66),
        selector_identity_hash: hash(0x77),
    }
}

fn deadlines() -> BootstrapDeadlinesV1 {
    BootstrapDeadlinesV1 {
        admission_ms: 5000,
        cleanup_ms: 10_000,
        startup_ms: 10_000,
        focused_verification_ms: 10_000,
        rollback_ms: 10_000,
        result_read_ms: 5000,
    }
}

fn baton_accepted(activation_id: &str) -> BatonAcceptedV1 {
    BatonAcceptedV1 {
        activation_id: id(activation_id),
        baton_id: id("baton.1"),
        baton_hash: hash(0xb0),
        command_hash: hash(0xc0),
        challenge_id: id("challenge.1"),
        challenge_hash: hash(0xc1),
        peer_executable_hash: hash(0xc2),
        peer_os_identity_hash: hash(0xc3),
        admission_id: id("admission.1"),
        admission_hash: hash(0xc4),
        management_checkpoint_id: id("checkpoint.1"),
        profile_version: 1,
        provenance_digest: hash(0xd0),
        enrollment_digest: hash(0xe0),
        capability_generation: 3,
        capability_digest: hash(0xf0),
        candidate_slot_hash: hash(0x12),
        previous_slot_hash: hash(0x34),
        verification_plan_hash: hash(0x7a),
        current_process_generation: ProcessGeneration(1),
        candidate_process_generation: ProcessGeneration(2),
        rollback_process_generation: ProcessGeneration(3),
        deadlines: deadlines(),
    }
}

fn effect_mutation(
    activation_id: &str,
    expected_ordinal: u64,
    expected_phase: BootstrapPhaseV1,
    observed: bool,
) -> BootstrapJournalMutationV1 {
    BootstrapJournalMutationV1 {
        activation_id: id(activation_id),
        expected_ordinal,
        expected_phase,
        effect: BootstrapEffectV1 {
            current_slot_hash: hash(0x34),
            target_slot_hash: hash(0x12),
            capability_generation: 3,
            process_generation: ProcessGeneration(2),
            observation_hash: if observed { hash(0x99) } else { String::new() },
        },
    }
}

fn activated_result(activation_id: &str) -> BootstrapResultV1 {
    let mut result = BootstrapResultV1 {
        schema_version: 1,
        receipt_id: id("receipt.1"),
        activation_id: id(activation_id),
        baton_hash: hash(0xb0),
        management_checkpoint_id: id("checkpoint.1"),
        recipient_process_generation: ProcessGeneration(2),
        sealed_at_epoch_ms: 1000,
        result: BootstrapResultKindV1::ActivatedVerified {
            focused_verification: FocusedVerificationEvidenceV1 {
                plan_id: id("plan.1"),
                plan_hash: hash(0x1f),
                results: Vec::new(),
                evidence_hash: hash(0x2f),
            },
        },
        receipt_hash: String::new(),
    };
    result.receipt_hash =
        aworkit_trusted_core::bootstrap_result_hash_v1(&result).expect("receipt hash");
    result
}

fn unsupported_result(activation_id: &str) -> BootstrapResultV1 {
    let mut result = BootstrapResultV1 {
        schema_version: 1,
        receipt_id: id("receipt.1"),
        activation_id: id(activation_id),
        baton_hash: hash(0xb0),
        management_checkpoint_id: id("checkpoint.1"),
        recipient_process_generation: ProcessGeneration(2),
        sealed_at_epoch_ms: 1000,
        result: BootstrapResultKindV1::Unsupported {
            reason: PlatformReasonV1 {
                code: "unsupported_volume".to_owned(),
                message: "selector is not atomic on this volume".to_owned(),
                next_steps: vec!["move installation".to_owned()],
            },
        },
        receipt_hash: String::new(),
    };
    result.receipt_hash =
        aworkit_trusted_core::bootstrap_result_hash_v1(&result).expect("receipt hash");
    result
}

fn notice(activation_id: &str) -> ManualRecoveryNoticeV1 {
    ManualRecoveryNoticeV1 {
        notice_id: id("notice.1"),
        activation_id: id(activation_id),
        reason: ReasonCodeV1::AmbiguousSelectorState,
        observed_slot_state_hash: hash(0x5a),
        diagnostic_id: id("diagnostic.1"),
        instructions: vec!["restore the previous slot".to_owned()],
    }
}

fn enrollment_receipt(request_id: &str) -> EnrollmentPreparedV1 {
    EnrollmentPreparedV1 {
        preparation_id: id("preparation.1"),
        request_id: id(request_id),
        enrollment_digest: hash(0x61),
        stable_launcher: "launcher".to_owned(),
        restart_instructions: vec!["restart through launcher".to_owned()],
    }
}

/// Starts an activation journal in the `AdmittingBaton` phase.
fn start_activation() -> (ActivationJournal, Arc<InMemoryJournalStorage>) {
    let storage: Arc<InMemoryJournalStorage> = Arc::new(InMemoryJournalStorage::default());
    let journal = ActivationJournal::new(Arc::clone(&storage) as _);
    journal.acquire_single_flight().expect("acquire lock");
    journal
        .append_baton_accepted(&baton_accepted("activation.1"))
        .expect("baton accepted");
    (journal, storage)
}

/// Drives one fenced activation phase advance using the live storage state.
fn advance(
    journal: &ActivationJournal,
    storage: &Arc<InMemoryJournalStorage>,
    next: BootstrapPhaseV1,
) -> Result<(), BootstrapJournalError> {
    let ordinal = storage.record_count();
    let current = derive_bootstrap_phase(&storage.records());
    journal.advance_phase(&BootstrapPhaseAdvanceV1 {
        activation_id: id("activation.1"),
        expected_ordinal: ordinal,
        expected_phase: current,
        next_phase: next,
    })?;
    Ok(())
}

#[test]
fn enrollment_walks_to_prepared_and_seals_once() {
    let storage: Arc<InMemoryJournalStorage> = Arc::new(InMemoryJournalStorage::default());
    let journal = ActivationJournal::new(Arc::clone(&storage) as _);
    journal.acquire_single_flight().expect("acquire");

    journal
        .append_enrollment_intent(&enrollment_request("enroll.1"), &identities())
        .expect("intent");
    let observation = EnrollmentJournalMutationV1 {
        enrollment_id: id("enroll.1"),
        expected_ordinal: 1,
        expected_phase: EnrollmentPhaseV1::Intent,
        observation: EnrollmentObservationV1 {
            initial_active_bundle_hash: hash(0x12),
            published_slot_verified: true,
        },
    };
    journal
        .append_enrollment_observation(&observation)
        .expect("observation");
    journal
        .store_enrollment_prepared(&enrollment_receipt("enroll.1"))
        .expect("prepared");

    let recovered = journal
        .load_enrollment_recovery(&id("enroll.1"))
        .expect("recovery");
    let Some(recovered) = recovered else {
        panic!("enrollment recovery present");
    };
    assert_eq!(recovered.phase, EnrollmentPhaseV1::Prepared);
    assert!(recovered.terminal.is_some());
    assert_eq!(recovered.head_ordinal, 1);

    // The terminal receipt is immutable: sealing again is rejected.
    assert!(matches!(
        journal.store_enrollment_prepared(&enrollment_receipt("enroll.1")),
        Err(BootstrapJournalError::TerminalSealed)
    ));

    let read = journal
        .read_enrollment_prepared(&id("enroll.1"))
        .expect("read prepared");
    assert_eq!(read.request_id, id("enroll.1"));
    journal.seal_terminal().expect("seal terminal");
}

#[test]
fn activation_walks_to_verified_and_seals_the_receipt() {
    let (journal, storage) = start_activation();
    for next in [
        BootstrapPhaseV1::BatonDurable,
        BootstrapPhaseV1::SlotsVerified,
        BootstrapPhaseV1::QuiescingCurrent,
        BootstrapPhaseV1::CandidateSelected,
    ] {
        advance(&journal, &storage, next).expect("advance");
    }

    journal
        .append_effect_intent(&effect_mutation(
            "activation.1",
            storage.record_count(),
            BootstrapPhaseV1::CandidateSelected,
            false,
        ))
        .expect("effect intent");
    journal
        .append_observed_effect(&effect_mutation(
            "activation.1",
            storage.record_count(),
            BootstrapPhaseV1::CandidateSelected,
            true,
        ))
        .expect("observed effect");

    for next in [
        BootstrapPhaseV1::CandidateLaunching,
        BootstrapPhaseV1::AwaitingCandidateIdentity,
        BootstrapPhaseV1::CandidateVerifying,
        BootstrapPhaseV1::Verified,
    ] {
        advance(&journal, &storage, next).expect("advance");
    }

    journal
        .store_bootstrap_result(&activated_result("activation.1"))
        .expect("store result");
    let recovered = journal
        .load_activation_recovery(&id("activation.1"))
        .expect("recovery")
        .expect("activation present");
    assert_eq!(recovered.phase, BootstrapPhaseV1::ResultAvailable);
    assert!(matches!(
        recovered.terminal.expect("terminal"),
        BootstrapResultV1 {
            result: BootstrapResultKindV1::ActivatedVerified { .. },
            ..
        }
    ));
}

#[test]
fn replayed_or_stale_mutation_is_rejected() {
    let (journal, storage) = start_activation();
    // A stale ordinal (skipping ahead) must be fenced.
    let stale = BootstrapPhaseAdvanceV1 {
        activation_id: id("activation.1"),
        expected_ordinal: 99,
        expected_phase: BootstrapPhaseV1::AdmittingBaton,
        next_phase: BootstrapPhaseV1::BatonDurable,
    };
    assert!(matches!(
        journal.advance_phase(&stale),
        Err(BootstrapJournalError::StaleOrdinal { expected: 99, .. })
    ));

    // A wrong expected phase must be fenced.
    let wrong_phase = BootstrapPhaseAdvanceV1 {
        activation_id: id("activation.1"),
        expected_ordinal: storage.record_count(),
        expected_phase: BootstrapPhaseV1::Idle,
        next_phase: BootstrapPhaseV1::BatonDurable,
    };
    assert!(matches!(
        journal.advance_phase(&wrong_phase),
        Err(BootstrapJournalError::StalePhase { .. })
    ));

    // An illegal transition must be rejected.
    let illegal = BootstrapPhaseAdvanceV1 {
        activation_id: id("activation.1"),
        expected_ordinal: storage.record_count(),
        expected_phase: BootstrapPhaseV1::AdmittingBaton,
        next_phase: BootstrapPhaseV1::Verified,
    };
    assert!(matches!(
        journal.advance_phase(&illegal),
        Err(BootstrapJournalError::IllegalPhaseTransition { .. })
    ));
}

#[test]
fn effect_observation_requires_an_observation_hash() {
    let (journal, storage) = start_activation();
    advance(&journal, &storage, BootstrapPhaseV1::BatonDurable).expect("advance");
    // An "observed" effect with an empty observation hash is contradictory.
    let bad = effect_mutation(
        "activation.1",
        storage.record_count(),
        BootstrapPhaseV1::BatonDurable,
        true,
    );
    let mut bad = bad;
    bad.effect.observation_hash.clear();
    assert!(matches!(
        journal.append_observed_effect(&bad),
        Err(BootstrapJournalError::Invalid(_))
    ));
}

#[test]
fn identity_mismatch_is_rejected() {
    let (journal, _) = start_activation();
    // The header already fixes activation.1; a mutation for a different id fails.
    let mutation = effect_mutation(
        "activation.other",
        1,
        BootstrapPhaseV1::AdmittingBaton,
        false,
    );
    assert!(matches!(
        journal.append_effect_intent(&mutation),
        Err(BootstrapJournalError::IdentityConflict)
    ));
}

#[test]
fn unsupported_before_quiescence_seals_an_unsupported_receipt() {
    let (journal, storage) = start_activation();
    // The managed-local guarantee is absent before quiescence.
    advance(&journal, &storage, BootstrapPhaseV1::Unsupported).expect("advance");
    journal
        .store_bootstrap_result(&unsupported_result("activation.1"))
        .expect("store unsupported");

    let recovered = journal
        .load_activation_recovery(&id("activation.1"))
        .expect("recovery")
        .expect("present");
    assert_eq!(recovered.phase, BootstrapPhaseV1::ResultAvailable);
    assert!(matches!(
        recovered.terminal.expect("terminal"),
        BootstrapResultV1 {
            result: BootstrapResultKindV1::Unsupported { .. },
            ..
        }
    ));

    // No further phase advance is possible once the receipt is sealed.
    assert!(matches!(
        advance(&journal, &storage, BootstrapPhaseV1::Idle),
        Err(BootstrapJournalError::TerminalImmutable)
    ));
}

#[test]
fn terminal_receipt_must_use_the_shared_trusted_core_hash_contract() {
    let (journal, storage) = start_activation();
    advance(&journal, &storage, BootstrapPhaseV1::Unsupported).expect("advance");
    let mut receipt = unsupported_result("activation.1");
    receipt.receipt_hash = hash(0xff);
    assert!(matches!(
        journal.store_bootstrap_result(&receipt),
        Err(BootstrapJournalError::Invalid(
            "bootstrap receipt hash is invalid"
        ))
    ));
}

#[test]
fn second_single_flight_is_busy() {
    let storage: Arc<InMemoryJournalStorage> = Arc::new(InMemoryJournalStorage::default());
    let first = ActivationJournal::new(Arc::clone(&storage) as _);
    first.acquire_single_flight().expect("first acquires");
    let second = ActivationJournal::new(Arc::clone(&storage) as _);
    assert!(matches!(
        second.acquire_single_flight(),
        Err(BootstrapJournalError::Busy)
    ));
}

#[test]
fn tampered_record_breaks_the_chain_on_recovery() {
    let storage: Arc<InMemoryJournalStorage> = Arc::new(InMemoryJournalStorage::default());
    let journal = ActivationJournal::new(Arc::clone(&storage) as _);
    journal.acquire_single_flight().expect("acquire");
    journal
        .append_baton_accepted(&baton_accepted("activation.1"))
        .expect("baton");

    // Corrupt the next record's hash so the durable chain no longer verifies.
    storage
        .corrupt_next_record_hash
        .store(true, std::sync::atomic::Ordering::SeqCst);
    advance(&journal, &storage, BootstrapPhaseV1::BatonDurable).expect("advance");

    assert!(matches!(
        journal.load_activation_recovery(&id("activation.1")),
        Err(BootstrapJournalError::ChainBroken { .. })
    ));
}

#[test]
fn durability_failure_leaves_no_record() {
    let storage: Arc<InMemoryJournalStorage> = Arc::new(InMemoryJournalStorage::default());
    let journal = ActivationJournal::new(Arc::clone(&storage) as _);
    journal.acquire_single_flight().expect("acquire");
    journal
        .append_baton_accepted(&baton_accepted("activation.1"))
        .expect("baton");
    assert_eq!(storage.record_count(), 1);

    storage
        .fail_next_append
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let outcome = advance(&journal, &storage, BootstrapPhaseV1::BatonDurable);
    assert!(outcome.is_err());
    // The failed append must not have advanced the durable chain.
    assert_eq!(storage.record_count(), 1);
}

#[test]
fn manual_recovery_notice_is_persisted_and_idempotent() {
    let (journal, storage) = start_activation();
    for next in [
        BootstrapPhaseV1::BatonDurable,
        BootstrapPhaseV1::SlotsVerified,
        BootstrapPhaseV1::QuiescingCurrent,
        BootstrapPhaseV1::CandidateSelected,
        BootstrapPhaseV1::RollingBack,
        BootstrapPhaseV1::ManualRecoveryRequired,
    ] {
        advance(&journal, &storage, next).expect("advance");
    }
    let notice = notice("activation.1");
    journal
        .store_manual_recovery_notice(&notice)
        .expect("notice");
    // Rewriting the identical notice is idempotent.
    journal
        .store_manual_recovery_notice(&notice)
        .expect("idempotent notice");

    let recovered = journal
        .load_activation_recovery(&id("activation.1"))
        .expect("recovery")
        .expect("present");
    assert!(recovered.manual_recovery.is_some());
}

#[test]
fn enrollment_observation_cannot_be_repeated() {
    let storage: Arc<InMemoryJournalStorage> = Arc::new(InMemoryJournalStorage::default());
    let journal = ActivationJournal::new(Arc::clone(&storage) as _);
    journal.acquire_single_flight().expect("acquire");
    journal
        .append_enrollment_intent(&enrollment_request("enroll.1"), &identities())
        .expect("intent");
    let first = EnrollmentJournalMutationV1 {
        enrollment_id: id("enroll.1"),
        expected_ordinal: 1,
        expected_phase: EnrollmentPhaseV1::Intent,
        observation: EnrollmentObservationV1 {
            initial_active_bundle_hash: hash(0x12),
            published_slot_verified: true,
        },
    };
    journal
        .append_enrollment_observation(&first)
        .expect("first observation");
    let repeated = EnrollmentJournalMutationV1 {
        expected_ordinal: 2,
        expected_phase: EnrollmentPhaseV1::Published,
        ..first
    };
    assert!(matches!(
        journal.append_enrollment_observation(&repeated),
        Err(BootstrapJournalError::IllegalPhaseTransition { .. })
    ));
}

#[test]
fn observed_effect_must_close_the_exact_open_intent() {
    let (journal, storage) = start_activation();
    let intent = effect_mutation(
        "activation.1",
        storage.record_count(),
        BootstrapPhaseV1::AdmittingBaton,
        false,
    );
    journal.append_effect_intent(&intent).expect("intent");

    let mut mismatch = effect_mutation(
        "activation.1",
        storage.record_count(),
        BootstrapPhaseV1::AdmittingBaton,
        true,
    );
    mismatch.effect.target_slot_hash = hash(0xab);
    assert!(matches!(
        journal.append_observed_effect(&mismatch),
        Err(BootstrapJournalError::Invalid(_))
    ));

    assert!(matches!(
        advance(&journal, &storage, BootstrapPhaseV1::BatonDurable),
        Err(BootstrapJournalError::Invalid(_))
    ));
}

#[test]
fn observation_without_an_intent_is_rejected() {
    let (journal, storage) = start_activation();
    assert!(matches!(
        journal.append_observed_effect(&effect_mutation(
            "activation.1",
            storage.record_count(),
            BootstrapPhaseV1::AdmittingBaton,
            true,
        )),
        Err(BootstrapJournalError::Invalid(_))
    ));
}

#[cfg(unix)]
mod filesystem {
    use super::*;
    use crate::journal::FilesystemJournalStorage;

    #[test]
    fn persists_across_a_helper_restart_and_stops_at_a_torn_tail() {
        let root = tempfile::TempDir::new().expect("temp root");
        let storage: Arc<FilesystemJournalStorage> =
            Arc::new(FilesystemJournalStorage::open(root.path()).expect("open"));
        let journal = ActivationJournal::new(Arc::clone(&storage) as _);
        journal.acquire_single_flight().expect("acquire");
        journal
            .append_baton_accepted(&baton_accepted("activation.1"))
            .expect("baton");

        // Advance to a nonterminal phase so recovery is interesting.
        journal
            .advance_phase(&BootstrapPhaseAdvanceV1 {
                activation_id: id("activation.1"),
                expected_ordinal: 1,
                expected_phase: BootstrapPhaseV1::AdmittingBaton,
                next_phase: BootstrapPhaseV1::BatonDurable,
            })
            .expect("advance");

        // Simulate a torn tail: a corrupt record at the next ordinal.
        std::fs::write(
            root.path().join("records").join("0000000002.json"),
            b"torn, not json",
        )
        .expect("write torn record");

        // A fresh helper process opens the same root and recovers the durable
        // prefix, ignoring the torn record.
        let storage2: Arc<FilesystemJournalStorage> =
            Arc::new(FilesystemJournalStorage::open(root.path()).expect("reopen"));
        let journal2 = ActivationJournal::new(Arc::clone(&storage2) as _);
        let recovered = journal2
            .load_activation_recovery(&id("activation.1"))
            .expect("recovery")
            .expect("present");
        assert_eq!(recovered.phase, BootstrapPhaseV1::BatonDurable);
        assert_eq!(recovered.head_ordinal, 1);

        // Loading the chain discards the torn tail, so after the prior helper
        // releases its OS lock a fresh helper can resume at the same ordinal.
        drop(journal);
        let journal3 = ActivationJournal::new(Arc::clone(&storage2) as _);
        journal3
            .acquire_single_flight()
            .expect("reacquire after restart");
        journal3
            .advance_phase(&BootstrapPhaseAdvanceV1 {
                activation_id: id("activation.1"),
                expected_ordinal: 2,
                expected_phase: BootstrapPhaseV1::BatonDurable,
                next_phase: BootstrapPhaseV1::SlotsVerified,
            })
            .expect("resume after torn tail");
    }

    #[test]
    fn malformed_middle_record_is_not_downgraded_to_a_torn_tail() {
        let root = tempfile::TempDir::new().expect("temp root");
        let storage: Arc<FilesystemJournalStorage> =
            Arc::new(FilesystemJournalStorage::open(root.path()).expect("open"));
        let journal = ActivationJournal::new(Arc::clone(&storage) as _);
        journal.acquire_single_flight().expect("acquire");
        journal
            .append_baton_accepted(&baton_accepted("activation.1"))
            .expect("baton");
        journal
            .advance_phase(&BootstrapPhaseAdvanceV1 {
                activation_id: id("activation.1"),
                expected_ordinal: 1,
                expected_phase: BootstrapPhaseV1::AdmittingBaton,
                next_phase: BootstrapPhaseV1::BatonDurable,
            })
            .expect("advance 1");
        journal
            .advance_phase(&BootstrapPhaseAdvanceV1 {
                activation_id: id("activation.1"),
                expected_ordinal: 2,
                expected_phase: BootstrapPhaseV1::BatonDurable,
                next_phase: BootstrapPhaseV1::SlotsVerified,
            })
            .expect("advance 2");

        std::fs::write(
            root.path().join("records").join("0000000001.json"),
            b"corrupt middle",
        )
        .expect("corrupt middle record");
        assert!(matches!(
            journal.load_activation_recovery(&id("activation.1")),
            Err(BootstrapJournalError::ChainBroken { ordinal: 1 })
        ));
    }
}
