//! Fail-closed native repair port assembly.

use std::sync::Arc;

use aworkit_protocol::StableId;
use aworkit_trusted_core::{
    ActivationCapabilityQueryV1, AuthenticatedBootstrapResultV1,
    AuthenticatedInvestigationExecutionReceiptV1, BootstrapAdmissionV1, BootstrapResultQueryV1,
    CommittedRepairEventV1, CoreQuiescenceFactsV1, CoreQuiescencePortV1, CoreQuiescenceRequestV1,
    EnrollmentPreparedV1, FocusedVerificationEvidenceV1, InvestigationExecutionReceiptQueryV1,
    ManagedLocalEnrollmentRequestV1, ManagementCheckpointPortV1, ManagementCheckpointRefV1,
    ManagementCheckpointRequestV1, ManagementResumeRequestV1, PlatformCapabilityReportV1,
    RepairArtifactIntegrityPortV1, RepairArtifactReadinessV1, RepairArtifactVerificationRequestV1,
    RepairBootstrapPortV1, RepairInvestigationDispatchV1, RepairInvestigationPortV1,
    RepairLedgerAppendOutcomeV1, RepairLedgerAppendV1, RepairLedgerPortV1, RepairOrchestratorV1,
    RepairPortErrorV1,
};

/// Durable core event paired with the store-owned application-wide cursor.
#[derive(Clone, Debug)]
pub struct GloballyCommittedRepairEventV1 {
    pub global_sequence: u64,
    pub committed: CommittedRepairEventV1,
}

/// Read model extension implemented by the durable native ledger adapter.
/// The trusted-core port remains group-local; this interface exposes only the
/// storage-assigned global cursor needed by the multi-group UI projection.
pub trait ManagementRepairProjectionPortV1: Send + Sync {
    fn group_ids(&self) -> Result<Vec<StableId>, RepairPortErrorV1>;

    fn load_all_global_events(
        &self,
    ) -> Result<Vec<GloballyCommittedRepairEventV1>, RepairPortErrorV1>;

    fn current_global_version(&self) -> Result<u64, RepairPortErrorV1>;
}

/// Read-only empty fallback used by tests and non-native composition. It can
/// disclose that no durable facts exist, but every attempted write fails.
struct UnavailableRepairLedger;

impl RepairLedgerPortV1 for UnavailableRepairLedger {
    fn load_group(
        &self,
        group_id: &StableId,
    ) -> Result<Vec<CommittedRepairEventV1>, RepairPortErrorV1> {
        let _ = group_id;
        Ok(Vec::new())
    }

    fn append(
        &self,
        request: RepairLedgerAppendV1,
    ) -> Result<RepairLedgerAppendOutcomeV1, RepairPortErrorV1> {
        let _ = request;
        Err(unavailable(
            "repair_ledger_unavailable",
            "durable Management repair persistence is unavailable",
        ))
    }
}

impl ManagementRepairProjectionPortV1 for UnavailableRepairLedger {
    fn group_ids(&self) -> Result<Vec<StableId>, RepairPortErrorV1> {
        Ok(Vec::new())
    }

    fn load_all_global_events(
        &self,
    ) -> Result<Vec<GloballyCommittedRepairEventV1>, RepairPortErrorV1> {
        Ok(Vec::new())
    }

    fn current_global_version(&self) -> Result<u64, RepairPortErrorV1> {
        Ok(0)
    }
}

struct UnavailableBootstrapPort;

impl RepairBootstrapPortV1 for UnavailableBootstrapPort {
    fn query_activation_capability(
        &self,
        _query: ActivationCapabilityQueryV1,
    ) -> Result<PlatformCapabilityReportV1, RepairPortErrorV1> {
        Err(bootstrap_unavailable())
    }

    fn prepare_managed_local_enrollment(
        &self,
        _request: ManagedLocalEnrollmentRequestV1,
    ) -> Result<EnrollmentPreparedV1, RepairPortErrorV1> {
        Err(bootstrap_unavailable())
    }

    fn admit_activation(
        &self,
        _baton: aworkit_trusted_core::RepairActivationBatonV1,
    ) -> Result<BootstrapAdmissionV1, RepairPortErrorV1> {
        Err(bootstrap_unavailable())
    }

    fn record_core_quiescence(
        &self,
        _admission_id: &StableId,
        _facts: CoreQuiescenceFactsV1,
    ) -> Result<(), RepairPortErrorV1> {
        Err(bootstrap_unavailable())
    }

    fn submit_focused_verification(
        &self,
        _activation_id: &StableId,
        _evidence: FocusedVerificationEvidenceV1,
    ) -> Result<(), RepairPortErrorV1> {
        Err(bootstrap_unavailable())
    }

    fn read_result(
        &self,
        _query: BootstrapResultQueryV1,
    ) -> Result<Option<AuthenticatedBootstrapResultV1>, RepairPortErrorV1> {
        Err(bootstrap_unavailable())
    }
}

struct UnavailableInvestigationPort;

impl RepairInvestigationPortV1 for UnavailableInvestigationPort {
    fn dispatch(&self, _request: RepairInvestigationDispatchV1) -> Result<(), RepairPortErrorV1> {
        Err(unavailable(
            "repair_investigation_unavailable",
            "bounded Management investigation dispatch is unavailable",
        ))
    }

    fn read_execution_receipt(
        &self,
        _query: InvestigationExecutionReceiptQueryV1,
    ) -> Result<AuthenticatedInvestigationExecutionReceiptV1, RepairPortErrorV1> {
        Err(unavailable(
            "repair_investigation_receipt_unavailable",
            "authenticated investigation execution receipts are unavailable",
        ))
    }
}

pub(super) struct UnavailableArtifactIntegrityPort;

impl RepairArtifactIntegrityPortV1 for UnavailableArtifactIntegrityPort {
    fn verify_ready(
        &self,
        request: RepairArtifactVerificationRequestV1,
    ) -> Result<RepairArtifactReadinessV1, RepairPortErrorV1> {
        Ok(RepairArtifactReadinessV1::Unavailable {
            artifact_id: request.artifact.artifact_id,
            reason: "Hash-verified repair artifact reads are unavailable.".to_owned(),
        })
    }
}

struct UnavailableManagementPort;

impl ManagementCheckpointPortV1 for UnavailableManagementPort {
    fn create_checkpoint(
        &self,
        _request: ManagementCheckpointRequestV1,
    ) -> Result<ManagementCheckpointRefV1, RepairPortErrorV1> {
        Err(unavailable(
            "management_checkpoint_unavailable",
            "Management checkpoint persistence is unavailable",
        ))
    }

    fn resume_same_chat(
        &self,
        _request: ManagementResumeRequestV1,
    ) -> Result<(), RepairPortErrorV1> {
        Err(unavailable(
            "management_resume_unavailable",
            "Management same-Chat resume is unavailable",
        ))
    }
}

struct UnavailableQuiescencePort;

impl CoreQuiescencePortV1 for UnavailableQuiescencePort {
    fn quiesce_current_generation(
        &self,
        _request: CoreQuiescenceRequestV1,
    ) -> Result<CoreQuiescenceFactsV1, RepairPortErrorV1> {
        Err(unavailable(
            "core_quiescence_unavailable",
            "current-generation quiescence is unavailable",
        ))
    }
}

pub(super) fn unavailable_repair_service() -> (
    RepairOrchestratorV1,
    Arc<dyn RepairLedgerPortV1>,
    Arc<dyn ManagementRepairProjectionPortV1>,
) {
    let ledger = Arc::new(UnavailableRepairLedger);
    let service = RepairOrchestratorV1::new(
        ledger.clone(),
        Arc::new(UnavailableBootstrapPort),
        Arc::new(UnavailableInvestigationPort),
        Arc::new(UnavailableManagementPort),
        Arc::new(UnavailableQuiescencePort),
        Arc::new(UnavailableArtifactIntegrityPort),
    );
    (service, ledger.clone(), ledger)
}

/// Composes durable event persistence with deliberately unavailable helper,
/// investigation, checkpoint, quiescence, and artifact adapters. Reads and
/// locally valid decisions remain durable; privileged transitions stay closed
/// until their real process-bound ports and authority context are supplied.
pub(super) fn durable_ledger_repair_service<L>(
    ledger: Arc<L>,
) -> (
    RepairOrchestratorV1,
    Arc<dyn RepairLedgerPortV1>,
    Arc<dyn ManagementRepairProjectionPortV1>,
)
where
    L: RepairLedgerPortV1 + ManagementRepairProjectionPortV1 + 'static,
{
    let core_ledger: Arc<dyn RepairLedgerPortV1> = ledger.clone();
    let projection: Arc<dyn ManagementRepairProjectionPortV1> = ledger.clone();
    let service = RepairOrchestratorV1::new(
        core_ledger.clone(),
        Arc::new(UnavailableBootstrapPort),
        Arc::new(UnavailableInvestigationPort),
        Arc::new(UnavailableManagementPort),
        Arc::new(UnavailableQuiescencePort),
        Arc::new(UnavailableArtifactIntegrityPort),
    );
    (service, core_ledger, projection)
}

fn bootstrap_unavailable() -> RepairPortErrorV1 {
    unavailable(
        "bootstrap_ipc_degraded",
        "Bootstrap helper IPC is unavailable. Self-activation is disabled; the current build remains running.",
    )
}

fn unavailable(code: &str, message: &str) -> RepairPortErrorV1 {
    RepairPortErrorV1 {
        code: code.to_owned(),
        message: message.to_owned(),
        retryable: false,
    }
}
