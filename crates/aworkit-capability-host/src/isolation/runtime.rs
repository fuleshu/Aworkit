//! Fail-closed orchestration for one explicitly selected isolation backend.

use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::CancellationToken;

use super::contract::{is_bounded_evidence, is_bounded_identity};
use super::{
    ArtifactTransferV1, BackendAvailabilityV1, BackendDispatchV1, BackendExecutionFailureV1,
    BackendTerminalV1, BackendUnavailableReasonV1, BoundedResultTransferV1, EnforcementCategoryV1,
    EnforcementReportV1, IsolatedExecutionV1, IsolationBackendManifestV1, IsolationCleanupV1,
    IsolationEventErrorV1, IsolationOutcomeV1, IsolationProfileV1, IsolationRawEventV1,
    IsolationRequirementV1, IsolationRunReportV1, IsolationStrengthV1,
};

/// Stable backend boundary. Native handles and backend-specific protocol types
/// remain behind this interface.
pub trait IsolationBackendPortV1: Send + Sync {
    fn manifest(&self) -> IsolationBackendManifestV1;

    fn availability(&self) -> BackendAvailabilityV1;

    fn verify_enforcement(
        &self,
        profile: &IsolationProfileV1,
        deadline_epoch_millis: u64,
        cancellation: &CancellationToken,
    ) -> Result<EnforcementReportV1, BackendExecutionFailureV1>;

    fn execute_isolated(
        &self,
        enforcement: &EnforcementReportV1,
        execution: &IsolatedExecutionV1,
        cancellation: &CancellationToken,
        emit: &mut dyn FnMut(IsolationRawEventV1) -> Result<(), IsolationEventErrorV1>,
    ) -> Result<BackendTerminalV1, BackendExecutionFailureV1>;

    /// Cleanup is unconditional once a session exists and must return evidence
    /// even when the backend cannot prove cleanup completed.
    fn cleanup(&self, session_id: &str) -> IsolationCleanupV1;
}

/// Runs one exact profile. This type has no host process execution dependency,
/// so it cannot silently downgrade an isolation request.
pub struct IsolationRuntime<B> {
    backend: B,
}

impl<B: IsolationBackendPortV1> IsolationRuntime<B> {
    #[must_use]
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    #[must_use]
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Executes using the system's current Unix-epoch time for pre-dispatch
    /// deadline fencing. The backend is contractually responsible for enforcing
    /// the absolute deadline after dispatch.
    pub fn execute(
        &self,
        profile: &IsolationProfileV1,
        execution: &IsolatedExecutionV1,
        cancellation: &CancellationToken,
    ) -> Result<IsolationRunReportV1, IsolationRuntimeError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| IsolationRuntimeError::ClockUnavailable)?
            .as_millis()
            .try_into()
            .map_err(|_| IsolationRuntimeError::ClockUnavailable)?;
        self.execute_at(profile, execution, cancellation, now)
    }

    /// Deterministic entrypoint used by trusted callers with an authenticated
    /// clock and by hermetic conformance tests.
    pub fn execute_at(
        &self,
        profile: &IsolationProfileV1,
        execution: &IsolatedExecutionV1,
        cancellation: &CancellationToken,
        now_epoch_millis: u64,
    ) -> Result<IsolationRunReportV1, IsolationRuntimeError> {
        if !profile.is_valid() {
            return Err(IsolationRuntimeError::InvalidProfile);
        }
        let manifest = self.backend.manifest();
        validate_manifest(&manifest, profile)?;
        if !manifest.matches_pin(&profile.backend) {
            return Err(IsolationRuntimeError::Unavailable {
                requirement: profile.requirement,
                reason: BackendUnavailableReasonV1::IdentityDrift {
                    field: "backend adapter identity".to_owned(),
                },
            });
        }
        match self.backend.availability() {
            BackendAvailabilityV1::Available => {}
            BackendAvailabilityV1::Unavailable(reason) => {
                return Err(IsolationRuntimeError::Unavailable {
                    requirement: profile.requirement,
                    reason,
                });
            }
        }
        if cancellation.is_cancelled() {
            return Err(IsolationRuntimeError::Unavailable {
                requirement: profile.requirement,
                reason: BackendUnavailableReasonV1::CancelledBeforeDispatch,
            });
        }
        if now_epoch_millis >= execution.deadline_epoch_millis {
            return Err(IsolationRuntimeError::Unavailable {
                requirement: profile.requirement,
                reason: BackendUnavailableReasonV1::DeadlineElapsed,
            });
        }
        if !execution.validate(manifest.maximum_transfer_bytes)
            || execution.profile_id != profile.profile_id
            || execution.profile_hash != profile.profile_hash
            || execution.workspace_id != profile.workspace_id
        {
            return Err(IsolationRuntimeError::InvalidExecution);
        }

        let enforcement = self
            .backend
            .verify_enforcement(profile, execution.deadline_epoch_millis, cancellation)
            .map_err(|failure| IsolationRuntimeError::VerificationFailed { failure })?;
        if !enforcement.is_verified_for(profile) {
            let cleanup = cleanup_if_possible(&self.backend, &enforcement.session_id);
            return Err(IsolationRuntimeError::EnforcementRejected {
                detail: rejected_enforcement_detail(&enforcement, profile),
                cleanup,
            });
        }

        let mut transfers = TransferAccumulator::new(execution);
        let backend_result = {
            let mut emit = |event| transfers.accept(event);
            self.backend
                .execute_isolated(&enforcement, execution, cancellation, &mut emit)
        };
        let (events, violation, observed_dispatch) = transfers.finish();
        let mut terminal = match backend_result {
            Ok(terminal) => terminal,
            Err(failure) => BackendTerminalV1::BackendFailed {
                dispatch: failure.dispatch,
                detail: bounded_failure_detail(failure),
            },
        };
        let mut contract_violation = violation;
        if !terminal.validate() {
            contract_violation.get_or_insert(IsolationEventErrorV1::MalformedMetadata);
        }
        if let Some(error) = &contract_violation {
            terminal = BackendTerminalV1::BackendFailed {
                dispatch: conservative_dispatch(&terminal, observed_dispatch),
                detail: format!("backend contract violation: {error}"),
            };
        }

        let cleanup = self.backend.cleanup(&enforcement.session_id);
        let execution_outcome = terminal.execution_outcome();
        let overall_outcome =
            if cleanup.is_verified_for(&enforcement.session_id, manifest.execution_location) {
                execution_outcome
            } else {
                IsolationOutcomeV1::OutcomeUncertain
            };
        Ok(IsolationRunReportV1 {
            invocation_id: execution.invocation_id.clone(),
            strength: IsolationStrengthV1::VerifiedSecurityBoundary,
            enforcement,
            events,
            terminal,
            execution_outcome,
            overall_outcome,
            cleanup,
            contract_violation,
        })
    }
}

fn validate_manifest(
    manifest: &IsolationBackendManifestV1,
    profile: &IsolationProfileV1,
) -> Result<(), IsolationRuntimeError> {
    for category in EnforcementCategoryV1::ALL {
        if !manifest.verifiable_enforcement.contains(&category) {
            return Err(IsolationRuntimeError::Unavailable {
                requirement: profile.requirement,
                reason: BackendUnavailableReasonV1::ProfileUnsupported { category },
            });
        }
    }
    if !manifest.supported_hosts.contains(&profile.host_platform) {
        return Err(IsolationRuntimeError::Unavailable {
            requirement: profile.requirement,
            reason: BackendUnavailableReasonV1::UnsupportedHost {
                host: profile.host_platform.clone(),
            },
        });
    }
    if !manifest.enforces_deadlines || !manifest.supports_cancellation || !manifest.verifies_cleanup
    {
        return Err(IsolationRuntimeError::IncompleteLifecycleEnforcement);
    }
    if !manifest.is_well_formed() {
        return Err(IsolationRuntimeError::InvalidManifest);
    }
    Ok(())
}

fn cleanup_if_possible<B: IsolationBackendPortV1>(
    backend: &B,
    session_id: &str,
) -> Option<IsolationCleanupV1> {
    is_bounded_identity(session_id).then(|| backend.cleanup(session_id))
}

fn rejected_enforcement_detail(
    report: &EnforcementReportV1,
    profile: &IsolationProfileV1,
) -> String {
    if report.backend != profile.backend {
        return "backend or environment identity drift".to_owned();
    }
    if report.profile_id != profile.profile_id || report.profile_hash != profile.profile_hash {
        return "isolation profile identity drift".to_owned();
    }
    let expected = profile.expected_realizations();
    for category in EnforcementCategoryV1::ALL {
        let claim = report
            .claims
            .iter()
            .find(|claim| claim.realization.category() == category);
        match claim {
            None => return format!("missing {category:?} enforcement claim"),
            Some(claim) if claim.verification != super::EnforcementVerificationV1::Verified => {
                return format!("{category:?} enforcement is not verified");
            }
            Some(claim) if !expected.contains(&claim.realization) => {
                return format!("{category:?} enforcement realization drift");
            }
            Some(claim) if !is_bounded_evidence(&claim.evidence) => {
                return format!("{category:?} enforcement evidence is malformed");
            }
            Some(_) => {}
        }
    }
    "duplicate or malformed enforcement claims".to_owned()
}

fn bounded_failure_detail(failure: BackendExecutionFailureV1) -> String {
    if is_bounded_evidence(&failure.detail) {
        format!("{:?}: {}", failure.stage, failure.detail)
    } else {
        "backend returned malformed failure evidence".to_owned()
    }
}

fn conservative_dispatch(
    terminal: &BackendTerminalV1,
    observed: Option<BackendDispatchV1>,
) -> BackendDispatchV1 {
    if observed == Some(BackendDispatchV1::Accepted) {
        return BackendDispatchV1::Accepted;
    }
    match terminal {
        BackendTerminalV1::Exited { .. } => BackendDispatchV1::Accepted,
        BackendTerminalV1::Rejected { .. } => BackendDispatchV1::DefinitelyNotDispatched,
        BackendTerminalV1::Cancelled { dispatch, .. }
        | BackendTerminalV1::DeadlineExceeded { dispatch, .. }
        | BackendTerminalV1::RemoteLost { dispatch, .. }
        | BackendTerminalV1::BackendFailed { dispatch, .. } => *dispatch,
    }
}

struct TransferAccumulator<'a> {
    execution: &'a IsolatedExecutionV1,
    events: Vec<IsolationRawEventV1>,
    stream_bytes: usize,
    artifact_count: usize,
    artifact_bytes: usize,
    result_seen: bool,
    violation: Option<IsolationEventErrorV1>,
    observed_dispatch: Option<BackendDispatchV1>,
}

impl<'a> TransferAccumulator<'a> {
    fn new(execution: &'a IsolatedExecutionV1) -> Self {
        Self {
            execution,
            events: Vec::new(),
            stream_bytes: 0,
            artifact_count: 0,
            artifact_bytes: 0,
            result_seen: false,
            violation: None,
            observed_dispatch: None,
        }
    }

    fn accept(&mut self, event: IsolationRawEventV1) -> Result<(), IsolationEventErrorV1> {
        if let Some(error) = &self.violation {
            return Err(error.clone());
        }
        let result = self.validate_event(&event);
        if let Err(error) = result {
            self.violation = Some(error.clone());
            return Err(error);
        }
        if matches!(event, IsolationRawEventV1::DispatchAccepted { .. }) {
            self.observed_dispatch = Some(BackendDispatchV1::Accepted);
        }
        self.events.push(event);
        Ok(())
    }

    fn validate_event(&mut self, event: &IsolationRawEventV1) -> Result<(), IsolationEventErrorV1> {
        let limits = &self.execution.transfer_limits;
        if self.events.len() >= limits.maximum_event_count {
            return Err(IsolationEventErrorV1::EventCountExceeded);
        }
        if event.payload_bytes() > limits.maximum_event_bytes {
            return Err(IsolationEventErrorV1::EventBytesExceeded);
        }
        match event {
            IsolationRawEventV1::DispatchAccepted { receipt }
            | IsolationRawEventV1::Progress(receipt) => {
                if !is_bounded_evidence(receipt) {
                    return Err(IsolationEventErrorV1::MalformedMetadata);
                }
                self.add_stream_bytes(receipt.len())?;
            }
            IsolationRawEventV1::StandardOutput(bytes)
            | IsolationRawEventV1::StandardError(bytes) => self.add_stream_bytes(bytes.len())?,
            IsolationRawEventV1::Artifact(artifact) => self.accept_artifact(artifact)?,
            IsolationRawEventV1::Result(result) => self.accept_result(result)?,
        }
        Ok(())
    }

    fn add_stream_bytes(&mut self, count: usize) -> Result<(), IsolationEventErrorV1> {
        self.stream_bytes = self.stream_bytes.saturating_add(count);
        if self.stream_bytes > self.execution.transfer_limits.maximum_stream_bytes {
            return Err(IsolationEventErrorV1::StreamBytesExceeded);
        }
        Ok(())
    }

    fn accept_artifact(
        &mut self,
        artifact: &ArtifactTransferV1,
    ) -> Result<(), IsolationEventErrorV1> {
        if self.artifact_count >= self.execution.transfer_limits.maximum_artifact_count {
            return Err(IsolationEventErrorV1::ArtifactCountExceeded);
        }
        if artifact.content.len() > self.execution.transfer_limits.maximum_artifact_bytes {
            return Err(IsolationEventErrorV1::ArtifactBytesExceeded);
        }
        if !artifact.has_valid_identity_and_hash() {
            return Err(IsolationEventErrorV1::ArtifactIntegrity);
        }
        self.artifact_count = self.artifact_count.saturating_add(1);
        self.artifact_bytes = self.artifact_bytes.saturating_add(artifact.content.len());
        if self.artifact_bytes > self.execution.transfer_limits.maximum_total_artifact_bytes {
            return Err(IsolationEventErrorV1::AggregateArtifactBytesExceeded);
        }
        Ok(())
    }

    fn accept_result(
        &mut self,
        result: &BoundedResultTransferV1,
    ) -> Result<(), IsolationEventErrorV1> {
        if self.result_seen {
            return Err(IsolationEventErrorV1::DuplicateResult);
        }
        if result.content.len() > self.execution.transfer_limits.maximum_result_bytes {
            return Err(IsolationEventErrorV1::ResultBytesExceeded);
        }
        if !result.has_valid_identity_and_hash() {
            return Err(IsolationEventErrorV1::ResultIntegrity);
        }
        self.result_seen = true;
        Ok(())
    }

    fn finish(
        self,
    ) -> (
        Vec<IsolationRawEventV1>,
        Option<IsolationEventErrorV1>,
        Option<BackendDispatchV1>,
    ) {
        (self.events, self.violation, self.observed_dispatch)
    }
}

/// Pre-dispatch failures never authorize a host fallback.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum IsolationRuntimeError {
    #[error("system clock is unavailable")]
    ClockUnavailable,
    #[error("isolation backend manifest is malformed")]
    InvalidManifest,
    #[error("isolation backend lacks required deadline, cancellation, or cleanup enforcement")]
    IncompleteLifecycleEnforcement,
    #[error("pinned isolation profile is malformed or drifted")]
    InvalidProfile,
    #[error("isolated execution does not match its pinned profile or transfer bounds")]
    InvalidExecution,
    #[error("selected isolation backend is unavailable: {reason:?}")]
    Unavailable {
        requirement: IsolationRequirementV1,
        reason: BackendUnavailableReasonV1,
    },
    #[error("backend failed before enforcement could be verified: {failure:?}")]
    VerificationFailed { failure: BackendExecutionFailureV1 },
    #[error("backend enforcement was rejected: {detail}")]
    EnforcementRejected {
        detail: String,
        cleanup: Option<IsolationCleanupV1>,
    },
}

impl IsolationRuntimeError {
    /// Isolation failures are observations, never permission to run on-host.
    #[must_use]
    pub const fn host_fallback_permitted(&self) -> bool {
        false
    }

    /// Returns true only when available evidence proves command dispatch did
    /// not occur. A backend contract violation during verification is treated
    /// conservatively.
    #[must_use]
    pub fn definitely_not_started(&self) -> bool {
        !matches!(
            self,
            Self::VerificationFailed { failure }
                if failure.dispatch != BackendDispatchV1::DefinitelyNotDispatched
        )
    }
}
