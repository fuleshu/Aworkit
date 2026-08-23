//! The authenticated, versioned bootstrap protocol gateway.
//!
//! [`BootstrapGateway`] is the helper's sole core-facing boundary. It issues
//! one-use challenges, bounds and schema-validates every wire DTO, deduplicates
//! command IDs, fences the current, candidate, and rollback application
//! generations, and writes accepted identity and command facts to the
//! activation journal *before* any acknowledgement is returned. It owns no
//! independent durable file; its session is rebuilt from the journal after a
//! helper restart.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use aworkit_protocol::{ProcessGeneration, StableId};
use aworkit_trusted_core::{
    ActivationEligibilityV1, AuthenticatedBootstrapResultV1, BootstrapAcceptedAdmissionV1,
    BootstrapAdmissionV1, BootstrapPeerProofV1, BootstrapResultKindV1, BootstrapResultV1,
    BuildBundleRefV1, BuildProvenanceV1, EnrollmentPreparedV1, ManagedLocalEnrollmentRequestV1,
    PlatformCapabilityReportV1, PlatformReasonV1, RepairActivationBatonV1,
    bootstrap_result_hash_v1,
};

use crate::journal::{
    ActivationJournalPortV1, BatonAcceptedV1, BootstrapPhaseAdvanceV1, BootstrapPhaseV1,
    CommandAdmittedV1, EnrollmentJournalMutationV1, EnrollmentPhaseV1, canonical_hash,
};

use super::error::GatewayError;
use super::model::*;
use super::ports::{BootstrapEnrollmentPortV1, BootstrapPreflightPortV1, BootstrapProtocolPortV1};

/// How long an issued challenge stays consumable.
pub const CHALLENGE_TTL_MS: u64 = 300_000;

/// Bounded in-memory gateway state. All of it is reconstructible from the
/// activation journal plus the helper's own controlled identities.
#[derive(Default)]
struct GatewayState {
    challenge: Option<BootstrapChallengeV1>,
    consumed_challenge: Option<StableId>,
    active: Option<ActiveTransactionV1>,
    session: Option<BootstrapSessionV1>,
    baton_admissions: HashMap<StableId, (String, BootstrapAdmissionV1)>,
    command_seen: HashMap<StableId, (String, BootstrapCommandAckV1)>,
}

/// The platform-neutral bootstrap protocol gateway.
pub struct BootstrapGateway {
    journal: Arc<dyn ActivationJournalPortV1>,
    preflight: Arc<dyn BootstrapPreflightPortV1>,
    enrollment: Arc<dyn BootstrapEnrollmentPortV1>,
    helper: HelperIdentityV1,
    state: Mutex<GatewayState>,
    id_counter: AtomicU64,
}

impl BootstrapGateway {
    /// Wraps the journal and the two unprivileged ports the gateway routes to.
    #[must_use]
    pub fn new(
        journal: Arc<dyn ActivationJournalPortV1>,
        preflight: Arc<dyn BootstrapPreflightPortV1>,
        enrollment: Arc<dyn BootstrapEnrollmentPortV1>,
        helper: HelperIdentityV1,
    ) -> Self {
        Self {
            journal,
            preflight,
            enrollment,
            helper,
            state: Mutex::new(GatewayState::default()),
            id_counter: AtomicU64::new(0),
        }
    }

    fn lock(&self) -> MutexGuard<'_, GatewayState> {
        self.state
            .lock()
            .expect("gateway state lock is never poisoned")
    }

    fn next_id(prefix: &str, counter: &AtomicU64) -> Result<StableId, GatewayError> {
        let value = counter.fetch_add(1, Ordering::SeqCst);
        StableId::parse(format!("{prefix}.{value}")).map_err(|_| GatewayError::Bounded("stable id"))
    }

    fn reason(code: &str, message: &str) -> PlatformReasonV1 {
        PlatformReasonV1 {
            code: code.to_owned(),
            message: message.to_owned(),
            next_steps: vec!["restart through the stable launcher".to_owned()],
        }
    }

    /// Builds the baton-acceptance record that carries every fence.
    fn baton_record(
        &self,
        baton: &RepairActivationBatonV1,
        command_hash: &str,
        challenge: &BootstrapChallengeV1,
        peer: &PeerIdentityV1,
        admission_id: StableId,
        admission_hash: String,
    ) -> BatonAcceptedV1 {
        BatonAcceptedV1 {
            activation_id: baton.activation_id.clone(),
            baton_id: baton.baton_id.clone(),
            baton_hash: baton.baton_hash.clone(),
            command_hash: command_hash.to_owned(),
            challenge_id: challenge.challenge_id.clone(),
            challenge_hash: challenge.challenge_hash.clone(),
            peer_executable_hash: peer.peer_executable_hash.clone(),
            peer_os_identity_hash: peer.peer_os_identity_hash.clone(),
            admission_id,
            admission_hash,
            management_checkpoint_id: baton.management_checkpoint.checkpoint_id.clone(),
            profile_version: self.helper.profile_version,
            provenance_digest: baton.provenance_hash.clone(),
            enrollment_digest: baton.enrollment_digest.clone(),
            capability_generation: baton.capability_generation,
            capability_digest: baton.capability_digest.clone(),
            candidate_slot_hash: baton.candidate_hash.clone(),
            previous_slot_hash: baton.previous_working_build.artifact.content_hash.clone(),
            verification_plan_hash: baton.verification_plan.plan_hash.clone(),
            current_process_generation: baton.current_process_generation,
            candidate_process_generation: baton.candidate_process_generation,
            rollback_process_generation: baton.rollback_process_generation,
            deadlines: baton.deadlines.clone(),
        }
    }

    /// Journals an admitted baton, advances to `Unsupported`, and seals the
    /// protected receipt that stays active on the current generation.
    fn seal_unsupported(
        &self,
        state: &mut GatewayState,
        now_epoch_ms: u64,
        peer: &PeerIdentityV1,
        baton: &RepairActivationBatonV1,
        challenge: &BootstrapChallengeV1,
        command_hash: &str,
        reason: PlatformReasonV1,
    ) -> Result<BootstrapAdmissionV1, GatewayError> {
        let receipt = self.unsupported_receipt(now_epoch_ms, peer, baton, reason);
        self.journal.acquire_single_flight()?;
        state.active = Some(ActiveTransactionV1::Activation {
            activation_id: baton.activation_id.clone(),
        });
        let record = self.baton_record(
            baton,
            command_hash,
            challenge,
            peer,
            receipt.receipt.receipt_id.clone(),
            receipt.receipt.receipt_hash.clone(),
        );
        self.journal.append_baton_accepted(&record)?;
        self.journal.advance_phase(&BootstrapPhaseAdvanceV1 {
            activation_id: baton.activation_id.clone(),
            expected_ordinal: 1,
            expected_phase: BootstrapPhaseV1::AdmittingBaton,
            next_phase: BootstrapPhaseV1::Unsupported,
        })?;
        self.journal.store_bootstrap_result(&receipt.receipt)?;
        self.journal.seal_terminal()?;
        state.session = Some(BootstrapSessionV1 {
            activation_id: baton.activation_id.clone(),
            protocol_version: BOOTSTRAP_PROTOCOL_VERSION_V1,
            peer: peer.clone(),
            challenge_id: challenge.challenge_id.clone(),
            challenge_nonce: challenge.nonce.clone(),
            challenge_hash: challenge.challenge_hash.clone(),
            helper_identity_hash: self.helper.helper_identity_hash.clone(),
            provenance_digest: baton.provenance_hash.clone(),
            enrollment_digest: baton.enrollment_digest.clone(),
            capability_generation: baton.capability_generation,
            capability_digest: baton.capability_digest.clone(),
            accepted_baton_hash: baton.baton_hash.clone(),
            verification_plan_hash: baton.verification_plan.plan_hash.clone(),
            current_process_generation: baton.current_process_generation,
            candidate_process_generation: baton.candidate_process_generation,
            rollback_process_generation: baton.rollback_process_generation,
        });
        state.active = None;
        let admission = BootstrapAdmissionV1::Unsupported(receipt);
        state.baton_admissions.insert(
            baton.baton_id.clone(),
            (baton.baton_hash.clone(), admission.clone()),
        );
        Ok(admission)
    }

    fn unsupported_receipt(
        &self,
        now_epoch_ms: u64,
        peer: &PeerIdentityV1,
        baton: &RepairActivationBatonV1,
        reason: PlatformReasonV1,
    ) -> AuthenticatedBootstrapResultV1 {
        let receipt_id =
            Self::next_id("receipt", &self.id_counter).expect("receipt id is always parseable");
        let mut receipt = BootstrapResultV1 {
            schema_version: 1,
            receipt_id,
            activation_id: baton.activation_id.clone(),
            baton_hash: baton.baton_hash.clone(),
            management_checkpoint_id: baton.management_checkpoint.checkpoint_id.clone(),
            recipient_process_generation: baton.current_process_generation,
            sealed_at_epoch_ms: now_epoch_ms,
            result: BootstrapResultKindV1::Unsupported { reason },
            receipt_hash: String::new(),
        };
        receipt.receipt_hash = bootstrap_result_hash_v1(&receipt).expect("receipt is serializable");
        let peer_proof = BootstrapPeerProofV1 {
            same_user_authenticated: true,
            recipient_process_generation: baton.current_process_generation,
            ownership_hash: self.helper.helper_identity_hash.clone(),
            channel_binding_hash: peer.peer_os_identity_hash.clone(),
        };
        AuthenticatedBootstrapResultV1 {
            receipt,
            peer: peer_proof,
        }
    }

    /// Rebuilds the in-memory activation session from the journal after a
    /// helper restart. Only recovery and result-read traffic is valid until
    /// this is called.
    fn rebuild_session(
        &self,
        state: &mut GatewayState,
        activation_id: &StableId,
        peer: &PeerIdentityV1,
    ) -> Result<BootstrapSessionV1, GatewayError> {
        let recovery = self.journal.load_activation_recovery(activation_id)?;
        let Some(recovery) = recovery.as_ref() else {
            return Err(GatewayError::NoActiveTransaction);
        };
        let Some(baton) = recovery.baton.as_ref() else {
            return Err(GatewayError::NoActiveTransaction);
        };
        let accepted = BootstrapAcceptedAdmissionV1 {
            admission_id: baton.admission_id.clone(),
            activation_id: baton.activation_id.clone(),
            baton_hash: baton.baton_hash.clone(),
            candidate_process_generation: baton.candidate_process_generation,
            rollback_process_generation: baton.rollback_process_generation,
            admission_hash: baton.admission_hash.clone(),
        };
        let expected_admission_hash = canonical_hash(&(
            &accepted.admission_id,
            &accepted.activation_id,
            &accepted.baton_hash,
            &accepted.candidate_process_generation,
            &accepted.rollback_process_generation,
        ))?;
        if accepted.admission_hash != expected_admission_hash {
            return Err(GatewayError::Journal(
                crate::journal::BootstrapJournalError::ChainBroken { ordinal: 0 },
            ));
        }
        let recovered_admission =
            if let Some(receipt) = recovery.terminal.clone().filter(|receipt| {
                matches!(&receipt.result, BootstrapResultKindV1::Unsupported { .. })
            }) {
                BootstrapAdmissionV1::Unsupported(AuthenticatedBootstrapResultV1 {
                    peer: BootstrapPeerProofV1 {
                        same_user_authenticated: true,
                        recipient_process_generation: receipt.recipient_process_generation,
                        ownership_hash: self.helper.helper_identity_hash.clone(),
                        channel_binding_hash: baton.challenge_hash.clone(),
                    },
                    receipt,
                })
            } else {
                BootstrapAdmissionV1::Accepted(accepted)
            };
        state.baton_admissions.insert(
            baton.baton_id.clone(),
            (baton.baton_hash.clone(), recovered_admission),
        );
        state.active = recovery
            .terminal
            .is_none()
            .then(|| ActiveTransactionV1::Activation {
                activation_id: baton.activation_id.clone(),
            });
        let session = BootstrapSessionV1 {
            activation_id: baton.activation_id.clone(),
            protocol_version: BOOTSTRAP_PROTOCOL_VERSION_V1,
            peer: peer.clone(),
            challenge_id: baton.challenge_id.clone(),
            challenge_nonce: String::new(),
            challenge_hash: baton.challenge_hash.clone(),
            helper_identity_hash: self.helper.helper_identity_hash.clone(),
            provenance_digest: baton.provenance_digest.clone(),
            enrollment_digest: baton.enrollment_digest.clone(),
            capability_generation: baton.capability_generation,
            capability_digest: baton.capability_digest.clone(),
            accepted_baton_hash: baton.baton_hash.clone(),
            verification_plan_hash: baton.verification_plan_hash.clone(),
            current_process_generation: baton.current_process_generation,
            candidate_process_generation: baton.candidate_process_generation,
            rollback_process_generation: baton.rollback_process_generation,
        };
        state.command_seen.clear();
        if recovery.admitted_commands.len() > MAX_SEEN_COMMAND_IDS {
            return Err(GatewayError::Bounded(
                "durable command deduplication bound exceeded",
            ));
        }
        for command in &recovery.admitted_commands {
            let ack = BootstrapCommandAckV1 {
                command_id: command.command_id.clone(),
                activation_id: command.activation_id.clone(),
                command_hash: command.command_hash.clone(),
                phase: command.durable_phase,
                durable: true,
            };
            state.command_seen.insert(
                command.command_id.clone(),
                (command.command_hash.clone(), ack),
            );
        }
        state.session = Some(session.clone());
        Ok(session)
    }

    fn command_binding(
        command: &BootstrapCommandV1,
    ) -> Result<(StableId, StableId, Option<ProcessGeneration>, &'static str), GatewayError> {
        Ok(match command {
            BootstrapCommandV1::BeginActivation {
                command_id,
                activation_id,
                ..
            } => (
                command_id.clone(),
                activation_id.clone(),
                None,
                "begin_activation",
            ),
            BootstrapCommandV1::CandidateGenerationReady {
                command_id,
                activation_id,
                generation,
            } => (
                command_id.clone(),
                activation_id.clone(),
                Some(*generation),
                "candidate_generation_ready",
            ),
            BootstrapCommandV1::FocusedVerificationCompleted {
                command_id,
                activation_id,
                generation,
                ..
            } => (
                command_id.clone(),
                activation_id.clone(),
                Some(*generation),
                "focused_verification_completed",
            ),
            BootstrapCommandV1::ReadResult {
                command_id,
                activation_id,
                recipient_process_generation,
            } => (
                command_id.clone(),
                activation_id.clone(),
                Some(*recipient_process_generation),
                "read_result",
            ),
        })
    }

    fn command_legal_in_phase(kind: &str, phase: BootstrapPhaseV1) -> bool {
        match kind {
            "begin_activation" => matches!(phase, BootstrapPhaseV1::BatonDurable),
            "candidate_generation_ready" => matches!(
                phase,
                BootstrapPhaseV1::AwaitingCandidateIdentity | BootstrapPhaseV1::PreviousRelaunching
            ),
            "focused_verification_completed" => {
                matches!(phase, BootstrapPhaseV1::CandidateVerifying)
            }
            "read_result" => matches!(
                phase,
                BootstrapPhaseV1::ResultAvailable
                    | BootstrapPhaseV1::Verified
                    | BootstrapPhaseV1::RolledBack
                    | BootstrapPhaseV1::ManualRecoveryRequired
            ),
            _ => false,
        }
    }

    fn generation_matches(
        session: &BootstrapSessionV1,
        kind: &str,
        phase: BootstrapPhaseV1,
        generation: Option<ProcessGeneration>,
    ) -> bool {
        let Some(generation) = generation else {
            return true;
        };
        match kind {
            "candidate_generation_ready" if phase == BootstrapPhaseV1::PreviousRelaunching => {
                generation == session.rollback_process_generation
            }
            "candidate_generation_ready" | "focused_verification_completed" => {
                generation == session.candidate_process_generation
            }
            "read_result" => {
                generation == session.current_process_generation
                    || generation == session.candidate_process_generation
                    || generation == session.rollback_process_generation
            }
            _ => true,
        }
    }

    fn validate_peer(peer: &PeerIdentityV1) -> Result<(), GatewayError> {
        if peer.peer_process_generation.0 == 0
            || !Self::is_hash(&peer.peer_executable_hash)
            || !Self::is_hash(&peer.peer_os_identity_hash)
        {
            return Err(GatewayError::Bounded(
                "peer identity must carry executable and OS hashes",
            ));
        }
        Ok(())
    }

    fn validate_baton(baton: &RepairActivationBatonV1) -> Result<(), GatewayError> {
        Self::validate_dto_size(baton)?;
        if !Self::is_hash(&baton.baton_hash)
            || !Self::is_hash(&baton.candidate_hash)
            || !Self::is_hash(&baton.disclosure_hash)
            || !Self::is_hash(&baton.provenance_hash)
            || !Self::is_hash(&baton.enrollment_digest)
            || !Self::is_hash(&baton.capability_digest)
            || !Self::is_hash(&baton.verification_plan.plan_hash)
            || baton.candidate_version == 0
            || baton.schema_version != BOOTSTRAP_PROTOCOL_VERSION_V1
            || baton.current_process_generation.0 == 0
            || baton.candidate_process_generation.0 == 0
            || baton.rollback_process_generation.0 == 0
            || baton.current_process_generation == baton.candidate_process_generation
            || baton.current_process_generation == baton.rollback_process_generation
            || baton.candidate_process_generation == baton.rollback_process_generation
            || [
                baton.deadlines.admission_ms,
                baton.deadlines.cleanup_ms,
                baton.deadlines.startup_ms,
                baton.deadlines.focused_verification_ms,
                baton.deadlines.rollback_ms,
                baton.deadlines.result_read_ms,
            ]
            .into_iter()
            .any(|deadline| deadline == 0 || deadline > MAX_BOOTSTRAP_DEADLINE_MS)
        {
            return Err(GatewayError::Bounded(
                "baton is missing a required bound value",
            ));
        }
        let mut unhashed = baton.clone();
        unhashed.baton_hash.clear();
        if canonical_hash(&unhashed)? != baton.baton_hash {
            return Err(GatewayError::Bounded(
                "baton hash does not cover the baton bytes",
            ));
        }
        Ok(())
    }

    fn validate_enrollment_request(
        request: &ManagedLocalEnrollmentRequestV1,
    ) -> Result<(), GatewayError> {
        Self::validate_dto_size(request)?;
        if !Self::is_hash(&request.candidate_hash)
            || !Self::is_hash(&request.capability_digest)
            || !Self::is_hash(&request.projected_provenance_hash)
            || !Self::is_hash(&request.whole_bundle.artifact.content_hash)
            || request.whole_bundle.artifact.byte_size == 0
        {
            return Err(GatewayError::Bounded(
                "enrollment request is missing a bound value",
            ));
        }
        Ok(())
    }

    fn validate_enrollment(enrollment: &LocalBuildEnrollmentV1) -> Result<(), GatewayError> {
        Self::validate_dto_size(enrollment)?;
        if enrollment.profile_version != BOOTSTRAP_PROTOCOL_VERSION_V1
            || !Self::is_hash(&enrollment.enrollment_digest)
            || !Self::is_hash(&enrollment.active_slot_hash)
            || !Self::is_hash(&enrollment.current_bundle_hash)
            || !Self::is_hash(&enrollment.selector_identity_hash)
            || !Self::is_hash(&enrollment.helper_identity_hash)
            || !Self::is_hash(&enrollment.launcher_identity_hash)
            || !Self::is_hash(&enrollment.journal_identity_hash)
        {
            return Err(GatewayError::Bounded(
                "local build enrollment is missing a bound value",
            ));
        }
        Ok(())
    }

    fn validate_dto_size<T: serde::Serialize>(value: &T) -> Result<(), GatewayError> {
        let size = serde_json::to_vec(value)
            .map_err(|_| GatewayError::Bounded("DTO cannot be encoded"))?
            .len();
        if size > MAX_BOOTSTRAP_DTO_BYTES {
            return Err(GatewayError::Bounded("DTO exceeds bootstrap byte bound"));
        }
        Ok(())
    }

    fn is_hash(value: &str) -> bool {
        value.len() == 71
            && value.starts_with("sha256:")
            && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
    }

    /// Rebuilds the activation session from the journal after a helper
    /// restart. Until this is called, a fresh gateway accepts only
    /// result-read traffic.
    #[must_use]
    pub fn recover_activation(
        &self,
        activation_id: &StableId,
        peer: &PeerIdentityV1,
    ) -> Result<BootstrapSessionV1, GatewayError> {
        Self::validate_peer(peer)?;
        let mut state = self.lock();
        self.rebuild_session(&mut state, activation_id, peer)
    }
}

impl BootstrapProtocolPortV1 for BootstrapGateway {
    fn begin_bootstrap_challenge(
        &self,
        now_epoch_ms: u64,
        peer: &PeerIdentityV1,
    ) -> Result<BootstrapChallengeV1, GatewayError> {
        Self::validate_peer(peer)?;
        let mut state = self.lock();
        if state.active.is_some() {
            return Err(GatewayError::TransactionActive);
        }
        let challenge_id = Self::next_id("bch", &self.id_counter)?;
        let mut random = [0_u8; 32];
        getrandom::fill(&mut random)
            .map_err(|_| GatewayError::Port("OS challenge entropy is unavailable".to_owned()))?;
        let nonce = canonical_hash(&random)?;
        let mut challenge = BootstrapChallengeV1 {
            challenge_id,
            protocol_version: BOOTSTRAP_PROTOCOL_VERSION_V1,
            nonce,
            helper_identity_hash: self.helper.helper_identity_hash.clone(),
            expected_peer: peer.clone(),
            issued_at_epoch_ms: now_epoch_ms,
            expires_at_epoch_ms: now_epoch_ms.saturating_add(CHALLENGE_TTL_MS),
            challenge_hash: String::new(),
        };
        challenge.challenge_hash = canonical_hash(&(
            &challenge.challenge_id,
            &challenge.protocol_version,
            &challenge.nonce,
            &challenge.helper_identity_hash,
            &challenge.expected_peer,
            &challenge.issued_at_epoch_ms,
            &challenge.expires_at_epoch_ms,
        ))?;
        state.challenge = Some(challenge.clone());
        state.consumed_challenge = None;
        Ok(challenge)
    }

    fn query_activation_capability(
        &self,
        provenance: &BuildProvenanceV1,
        enrollment: &LocalBuildEnrollmentV1,
        candidate: &BuildBundleRefV1,
        previous: Option<&BuildBundleRefV1>,
    ) -> Result<PlatformCapabilityReportV1, GatewayError> {
        Self::validate_dto_size(provenance)?;
        Self::validate_dto_size(candidate)?;
        if let Some(previous) = previous {
            Self::validate_dto_size(previous)?;
        }
        Self::validate_enrollment(enrollment)?;
        let report = self
            .preflight
            .capability_report(provenance, enrollment, candidate, previous)
            .map_err(GatewayError::Port)?;
        Self::validate_dto_size(&report)?;
        if !Self::is_hash(&report.capability_digest)
            || report.candidate_id.as_str().is_empty()
            || report.candidate_version == 0
            || !Self::is_hash(&report.candidate_hash)
            || report.valid_from_epoch_ms >= report.expires_at_epoch_ms
        {
            return Err(GatewayError::Bounded("capability report lacks a digest"));
        }
        let mut unhashed = report.clone();
        unhashed.capability_digest.clear();
        if canonical_hash(&unhashed)? != report.capability_digest {
            return Err(GatewayError::Bounded(
                "capability digest does not cover the report bytes",
            ));
        }
        Ok(report)
    }

    fn prepare_managed_local_enrollment(
        &self,
        request: &ManagedLocalEnrollmentRequestV1,
    ) -> Result<EnrollmentPreparedV1, GatewayError> {
        Self::validate_enrollment_request(request)?;
        let mut state = self.lock();
        if state.active.is_some() {
            return Err(GatewayError::TransactionActive);
        }
        let plan = self
            .preflight
            .enrollment_plan(request)
            .map_err(GatewayError::Port)?;
        Self::validate_dto_size(&plan)?;
        if plan.profile_version != BOOTSTRAP_PROTOCOL_VERSION_V1
            || !Self::is_hash(&plan.helper_root_identity_hash)
            || !Self::is_hash(&plan.initial_active_slot_root_hash)
            || !Self::is_hash(&plan.selector_identity_hash)
            || !Self::is_hash(&plan.journal_identity_hash)
            || !Self::is_hash(&plan.plan_hash)
        {
            return Err(GatewayError::Bounded(
                "enrollment plan is not a bounded v1 plan",
            ));
        }
        let mut unhashed_plan = plan.clone();
        unhashed_plan.plan_hash.clear();
        if canonical_hash(&unhashed_plan)? != plan.plan_hash {
            return Err(GatewayError::Bounded(
                "enrollment plan hash does not cover the plan bytes",
            ));
        }
        // Journal the intent before any effect or acknowledgement.
        self.journal.acquire_single_flight()?;
        state.active = Some(ActiveTransactionV1::Enrollment {
            enrollment_id: request.request_id.clone(),
        });
        self.journal
            .append_enrollment_intent(request, &self.helper.enrollment_identities)?;
        let preparation = self
            .enrollment
            .materialize(request, &plan)
            .map_err(GatewayError::Port)?;
        Self::validate_dto_size(&preparation)?;
        if preparation.prepared.request_id != request.request_id
            || !preparation.observation.published_slot_verified
            || !Self::is_hash(&preparation.observation.initial_active_bundle_hash)
            || !Self::is_hash(&preparation.prepared.enrollment_digest)
        {
            return Err(GatewayError::Bounded(
                "enrollment preparation does not match the admitted request",
            ));
        }
        self.journal
            .append_enrollment_observation(&EnrollmentJournalMutationV1 {
                enrollment_id: request.request_id.clone(),
                expected_ordinal: 1,
                expected_phase: EnrollmentPhaseV1::Intent,
                observation: preparation.observation.clone(),
            })?;
        self.journal
            .store_enrollment_prepared(&preparation.prepared)?;
        self.journal.seal_terminal()?;
        state.active = None;
        Ok(preparation.prepared)
    }

    fn submit_repair_activation_baton(
        &self,
        now_epoch_ms: u64,
        peer: &PeerIdentityV1,
        baton: &RepairActivationBatonV1,
    ) -> Result<BootstrapAdmissionV1, GatewayError> {
        Self::validate_baton(baton)?;
        Self::validate_peer(peer)?;
        let mut state = self.lock();
        let baton_hash = baton.baton_hash.clone();
        match state.baton_admissions.get(&baton.baton_id) {
            Some((seen, _)) if *seen != baton.baton_hash => {
                return Err(GatewayError::CommandReplayed);
            }
            Some((_, admission)) => return Ok(admission.clone()),
            None => {}
        }
        if state.active.is_some() {
            return Err(GatewayError::TransactionActive);
        }
        let challenge = match state.challenge.take() {
            Some(challenge) => challenge,
            None if state.consumed_challenge.is_some() => {
                return Err(GatewayError::ChallengeConsumed);
            }
            None => return Err(GatewayError::ChallengeInvalid),
        };
        if challenge.expected_peer != *peer
            || challenge.protocol_version != BOOTSTRAP_PROTOCOL_VERSION_V1
        {
            return Err(GatewayError::PeerMismatch);
        }
        if now_epoch_ms < challenge.issued_at_epoch_ms
            || now_epoch_ms >= challenge.expires_at_epoch_ms
        {
            return Err(GatewayError::ChallengeInvalid);
        }
        state.consumed_challenge = Some(challenge.challenge_id.clone());
        let command_hash = canonical_hash(&(
            baton,
            &challenge.challenge_hash,
            &peer.peer_executable_hash,
            &peer.peer_os_identity_hash,
        ))?;
        if peer.peer_process_generation != baton.current_process_generation {
            return Err(GatewayError::StaleGeneration);
        }
        if now_epoch_ms >= baton.expires_at_epoch_ms {
            return self.seal_unsupported(
                &mut state,
                now_epoch_ms,
                peer,
                baton,
                &challenge,
                &command_hash,
                Self::reason(
                    "capability_drift",
                    "activation baton expired before admission",
                ),
            );
        }
        let fresh = self
            .preflight
            .revalidate_baton_binding(baton)
            .map_err(GatewayError::Port)?;
        Self::validate_dto_size(&fresh)?;
        let mut unhashed_fresh = fresh.clone();
        unhashed_fresh.capability_digest.clear();
        let drifted = fresh.eligibility != ActivationEligibilityV1::SupportedManagedLocal
            || fresh.capability_digest != baton.capability_digest
            || fresh.capability_generation != baton.capability_generation
            || fresh.candidate_id != baton.candidate_id
            || fresh.candidate_version != baton.candidate_version
            || fresh.candidate_hash != baton.candidate_hash
            || now_epoch_ms < fresh.valid_from_epoch_ms
            || now_epoch_ms >= fresh.expires_at_epoch_ms
            || canonical_hash(&unhashed_fresh)? != fresh.capability_digest;
        if drifted {
            return self.seal_unsupported(
                &mut state,
                now_epoch_ms,
                peer,
                baton,
                &challenge,
                &command_hash,
                Self::reason(
                    "capability_drift",
                    "capability generation changed or expired",
                ),
            );
        }
        // Journal the baton acceptance before any acknowledgement.
        self.journal.acquire_single_flight()?;
        state.active = Some(ActiveTransactionV1::Activation {
            activation_id: baton.activation_id.clone(),
        });
        let mut admission = BootstrapAcceptedAdmissionV1 {
            admission_id: Self::next_id("adm", &self.id_counter)?,
            activation_id: baton.activation_id.clone(),
            baton_hash: baton.baton_hash.clone(),
            candidate_process_generation: baton.candidate_process_generation,
            rollback_process_generation: baton.rollback_process_generation,
            admission_hash: String::new(),
        };
        admission.admission_hash = canonical_hash(&(
            &admission.admission_id,
            &admission.activation_id,
            &admission.baton_hash,
            &admission.candidate_process_generation,
            &admission.rollback_process_generation,
        ))?;
        let record = self.baton_record(
            baton,
            &command_hash,
            &challenge,
            peer,
            admission.admission_id.clone(),
            admission.admission_hash.clone(),
        );
        self.journal.append_baton_accepted(&record)?;
        self.journal.advance_phase(&BootstrapPhaseAdvanceV1 {
            activation_id: baton.activation_id.clone(),
            expected_ordinal: 1,
            expected_phase: BootstrapPhaseV1::AdmittingBaton,
            next_phase: BootstrapPhaseV1::BatonDurable,
        })?;
        let admitted = BootstrapAdmissionV1::Accepted(admission.clone());
        state
            .baton_admissions
            .insert(baton.baton_id.clone(), (baton_hash, admitted.clone()));
        state.session = Some(BootstrapSessionV1 {
            activation_id: baton.activation_id.clone(),
            protocol_version: BOOTSTRAP_PROTOCOL_VERSION_V1,
            peer: peer.clone(),
            challenge_id: challenge.challenge_id,
            challenge_nonce: challenge.nonce,
            challenge_hash: challenge.challenge_hash,
            helper_identity_hash: self.helper.helper_identity_hash.clone(),
            provenance_digest: baton.provenance_hash.clone(),
            enrollment_digest: baton.enrollment_digest.clone(),
            capability_generation: baton.capability_generation,
            capability_digest: baton.capability_digest.clone(),
            accepted_baton_hash: baton.baton_hash.clone(),
            verification_plan_hash: baton.verification_plan.plan_hash.clone(),
            current_process_generation: baton.current_process_generation,
            candidate_process_generation: baton.candidate_process_generation,
            rollback_process_generation: baton.rollback_process_generation,
        });
        Ok(admitted)
    }

    fn submit_bootstrap_command(
        &self,
        command: &BootstrapCommandV1,
    ) -> Result<BootstrapCommandAckV1, GatewayError> {
        Self::validate_dto_size(command)?;
        let (command_id, activation_id, generation, kind) = Self::command_binding(command)?;
        let mut state = self.lock();
        let command_hash = canonical_hash(command)?;
        match state.command_seen.get(&command_id) {
            Some((seen, _)) if *seen != command_hash => return Err(GatewayError::CommandReplayed),
            Some((_, ack)) => return Ok(ack.clone()),
            None => {}
        }
        if state.command_seen.len() >= MAX_SEEN_COMMAND_IDS {
            return Err(GatewayError::Bounded(
                "command deduplication bound exceeded",
            ));
        }
        let session = state
            .session
            .clone()
            .ok_or(GatewayError::NoActiveTransaction)?;
        if session.activation_id != activation_id {
            return Err(GatewayError::PeerMismatch);
        }
        let Some(recovery) = self.journal.load_activation_recovery(&activation_id)? else {
            return Err(GatewayError::NoActiveTransaction);
        };
        let durable_phase = recovery.phase;
        if !Self::command_legal_in_phase(kind, durable_phase) {
            return Err(GatewayError::IllegalPhase);
        }
        if !Self::generation_matches(&session, kind, durable_phase, generation) {
            return Err(GatewayError::StaleGeneration);
        }
        if let BootstrapCommandV1::BeginActivation { challenge_id, .. } = command {
            if session.challenge_id != *challenge_id {
                return Err(GatewayError::ChallengeInvalid);
            }
            if let BootstrapCommandV1::BeginActivation { expected_phase, .. } = command {
                if *expected_phase != durable_phase {
                    return Err(GatewayError::IllegalPhase);
                }
            }
        }
        if let BootstrapCommandV1::FocusedVerificationCompleted {
            verification_plan_hash,
            ..
        } = command
        {
            if verification_plan_hash != &session.verification_plan_hash {
                return Err(GatewayError::Bounded(
                    "verification plan hash does not match the admitted baton",
                ));
            }
        }
        if let BootstrapCommandV1::ReadResult {
            recipient_process_generation,
            ..
        } = command
        {
            let receipt = self.journal.read_bootstrap_result(&activation_id)?;
            if receipt.recipient_process_generation != *recipient_process_generation {
                return Err(GatewayError::RecipientMismatch);
            }
            // Result reads are non-actionable and occur after the immutable
            // journal head is sealed. The protected receipt is their durable
            // authority; appending after that head would invalidate its seal.
            let ack = BootstrapCommandAckV1 {
                command_id: command_id.clone(),
                activation_id,
                command_hash,
                phase: durable_phase,
                durable: false,
            };
            state
                .command_seen
                .insert(command_id, (ack.command_hash.clone(), ack.clone()));
            return Ok(ack);
        }
        // Journal the command hash before the acknowledgement.
        self.journal.append_command_admitted(&CommandAdmittedV1 {
            activation_id: activation_id.clone(),
            command_id: command_id.clone(),
            command_kind: kind.to_owned(),
            command_hash: command_hash.clone(),
            process_generation: generation.unwrap_or(session.current_process_generation),
            durable_phase,
        })?;
        let ack = BootstrapCommandAckV1 {
            command_id: command_id.clone(),
            activation_id: activation_id.clone(),
            command_hash,
            phase: durable_phase,
            durable: true,
        };
        state
            .command_seen
            .insert(command_id, (ack.command_hash.clone(), ack.clone()));
        Ok(ack)
    }

    fn read_bootstrap_result(
        &self,
        recipient: &ProcessGeneration,
    ) -> Result<AuthenticatedBootstrapResultV1, GatewayError> {
        let state = self.lock();
        let session = state
            .session
            .clone()
            .ok_or(GatewayError::NoActiveTransaction)?;
        let receipt = self.journal.read_bootstrap_result(&session.activation_id)?;
        if receipt.recipient_process_generation != *recipient {
            return Err(GatewayError::RecipientMismatch);
        }
        let peer_proof = BootstrapPeerProofV1 {
            same_user_authenticated: true,
            recipient_process_generation: receipt.recipient_process_generation,
            ownership_hash: self.helper.helper_identity_hash.clone(),
            channel_binding_hash: session.accepted_baton_hash.clone(),
        };
        Ok(AuthenticatedBootstrapResultV1 {
            receipt,
            peer: peer_proof,
        })
    }
}
