//! Scripted isolation adapter for deterministic lifecycle conformance tests.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};

use thiserror::Error;

use crate::CancellationToken;

use super::{
    BackendAvailabilityV1, BackendDispatchV1, BackendExecutionFailureV1,
    BackendExecutionLocationV1, BackendStageV1, BackendTerminalV1, CancellationEvidenceV1,
    CleanupVerificationV1, EnforcementClaimV1, EnforcementReportV1, EnforcementVerificationV1,
    IsolatedExecutionV1, IsolationBackendManifestV1, IsolationBackendPortV1, IsolationCleanupV1,
    IsolationEventErrorV1, IsolationProfileV1, IsolationRawEventV1, PinnedBackendIdentityV1,
};

/// Verification behavior for the next prepared hermetic session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HermeticVerificationV1 {
    /// Derive exact verified claims from the supplied pinned profile.
    Exact,
    /// Return a caller-supplied report to test drift and overclaim handling.
    Report(EnforcementReportV1),
    Failure(BackendExecutionFailureV1),
}

/// Scripted cleanup outcome, converted into exact cleanup evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HermeticCleanupV1 {
    Verified,
    Unverified(String),
    Failed(String),
}

/// One execution and cleanup script consumed by one verification session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HermeticIsolationRunV1 {
    pub events: Vec<IsolationRawEventV1>,
    pub terminal: Result<BackendTerminalV1, BackendExecutionFailureV1>,
    pub cleanup: HermeticCleanupV1,
}

impl HermeticIsolationRunV1 {
    #[must_use]
    pub fn successful(events: Vec<IsolationRawEventV1>) -> Self {
        Self {
            events,
            terminal: Ok(BackendTerminalV1::Exited { exit_code: 0 }),
            cleanup: HermeticCleanupV1::Verified,
        }
    }
}

/// Calls observed at the backend boundary.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HermeticIsolationObservationV1 {
    pub verified_profiles: Vec<IsolationProfileV1>,
    pub executions: Vec<IsolatedExecutionV1>,
    pub cleanup_sessions: Vec<String>,
}

#[derive(Clone)]
pub struct HermeticIsolationBackend {
    manifest: IsolationBackendManifestV1,
    environment_id: String,
    environment_hash: String,
    state: Arc<Mutex<HermeticState>>,
}

struct HermeticState {
    availability: BackendAvailabilityV1,
    verification: HermeticVerificationV1,
    scripts: VecDeque<HermeticIsolationRunV1>,
    active_scripts: BTreeMap<String, HermeticIsolationRunV1>,
    cleanup_scripts: BTreeMap<String, HermeticCleanupV1>,
    next_session: u64,
    observed: HermeticIsolationObservationV1,
}

impl HermeticIsolationBackend {
    #[must_use]
    pub fn new(
        manifest: IsolationBackendManifestV1,
        environment_id: impl Into<String>,
        environment_hash: impl Into<String>,
    ) -> Self {
        Self {
            manifest,
            environment_id: environment_id.into(),
            environment_hash: environment_hash.into(),
            state: Arc::new(Mutex::new(HermeticState {
                availability: BackendAvailabilityV1::Available,
                verification: HermeticVerificationV1::Exact,
                scripts: VecDeque::new(),
                active_scripts: BTreeMap::new(),
                cleanup_scripts: BTreeMap::new(),
                next_session: 1,
                observed: HermeticIsolationObservationV1::default(),
            })),
        }
    }

    pub fn set_availability(
        &self,
        availability: BackendAvailabilityV1,
    ) -> Result<(), HermeticIsolationError> {
        self.state
            .lock()
            .map_err(|_| HermeticIsolationError::StateUnavailable)?
            .availability = availability;
        Ok(())
    }

    pub fn set_verification(
        &self,
        verification: HermeticVerificationV1,
    ) -> Result<(), HermeticIsolationError> {
        self.state
            .lock()
            .map_err(|_| HermeticIsolationError::StateUnavailable)?
            .verification = verification;
        Ok(())
    }

    pub fn push_run(&self, run: HermeticIsolationRunV1) -> Result<(), HermeticIsolationError> {
        self.state
            .lock()
            .map_err(|_| HermeticIsolationError::StateUnavailable)?
            .scripts
            .push_back(run);
        Ok(())
    }

    pub fn observed(&self) -> Result<HermeticIsolationObservationV1, HermeticIsolationError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| HermeticIsolationError::StateUnavailable)?
            .observed
            .clone())
    }

    fn actual_identity(&self) -> PinnedBackendIdentityV1 {
        PinnedBackendIdentityV1 {
            backend_id: self.manifest.backend_id.clone(),
            adapter_version: self.manifest.adapter_version.clone(),
            adapter_hash: self.manifest.adapter_hash.clone(),
            environment_id: self.environment_id.clone(),
            environment_hash: self.environment_hash.clone(),
        }
    }
}

impl IsolationBackendPortV1 for HermeticIsolationBackend {
    fn manifest(&self) -> IsolationBackendManifestV1 {
        self.manifest.clone()
    }

    fn availability(&self) -> BackendAvailabilityV1 {
        self.state.lock().map_or_else(
            |_| {
                BackendAvailabilityV1::Unavailable(super::BackendUnavailableReasonV1::Unhealthy {
                    detail: "hermetic backend state is unavailable".to_owned(),
                })
            },
            |state| state.availability.clone(),
        )
    }

    fn verify_enforcement(
        &self,
        profile: &IsolationProfileV1,
        _deadline_epoch_millis: u64,
        cancellation: &CancellationToken,
    ) -> Result<EnforcementReportV1, BackendExecutionFailureV1> {
        if cancellation.is_cancelled() {
            return Err(failure(
                BackendStageV1::Verification,
                BackendDispatchV1::DefinitelyNotDispatched,
                "cancelled before hermetic verification",
            ));
        }
        let mut state = self.state.lock().map_err(|_| {
            failure(
                BackendStageV1::Verification,
                BackendDispatchV1::DefinitelyNotDispatched,
                "hermetic backend state is unavailable",
            )
        })?;
        state.observed.verified_profiles.push(profile.clone());
        let report = match &state.verification {
            HermeticVerificationV1::Exact => {
                let session_id = format!("hermetic-session-{}", state.next_session);
                state.next_session = state.next_session.saturating_add(1);
                EnforcementReportV1 {
                    session_id,
                    backend: self.actual_identity(),
                    profile_id: profile.profile_id.clone(),
                    profile_hash: profile.profile_hash.clone(),
                    claims: profile
                        .expected_realizations()
                        .into_iter()
                        .map(|realization| EnforcementClaimV1 {
                            evidence: format!(
                                "hermetic adapter verified {:?}",
                                realization.category()
                            ),
                            realization,
                            verification: EnforcementVerificationV1::Verified,
                        })
                        .collect(),
                }
            }
            HermeticVerificationV1::Report(report) => report.clone(),
            HermeticVerificationV1::Failure(failure) => return Err(failure.clone()),
        };
        if let Some(script) = state.scripts.pop_front() {
            state
                .active_scripts
                .insert(report.session_id.clone(), script);
        }
        Ok(report)
    }

    fn execute_isolated(
        &self,
        enforcement: &EnforcementReportV1,
        execution: &IsolatedExecutionV1,
        cancellation: &CancellationToken,
        emit: &mut dyn FnMut(IsolationRawEventV1) -> Result<(), IsolationEventErrorV1>,
    ) -> Result<BackendTerminalV1, BackendExecutionFailureV1> {
        let script = {
            let mut state = self.state.lock().map_err(|_| {
                failure(
                    BackendStageV1::Dispatch,
                    BackendDispatchV1::DefinitelyNotDispatched,
                    "hermetic backend state is unavailable",
                )
            })?;
            state.observed.executions.push(execution.clone());
            let script = state
                .active_scripts
                .remove(&enforcement.session_id)
                .ok_or_else(|| {
                    failure(
                        BackendStageV1::Dispatch,
                        BackendDispatchV1::DefinitelyNotDispatched,
                        "hermetic execution script is exhausted",
                    )
                })?;
            state
                .cleanup_scripts
                .insert(enforcement.session_id.clone(), script.cleanup.clone());
            script
        };
        if cancellation.is_cancelled() {
            return Ok(BackendTerminalV1::Cancelled {
                dispatch: BackendDispatchV1::DefinitelyNotDispatched,
                cancellation: CancellationEvidenceV1 {
                    requested: true,
                    backend_acknowledged: true,
                    terminal_confirmed: true,
                    evidence: "hermetic cancellation confirmed before dispatch".to_owned(),
                },
            });
        }
        let mut dispatch = BackendDispatchV1::Unknown;
        for event in script.events {
            if matches!(event, IsolationRawEventV1::DispatchAccepted { .. }) {
                dispatch = BackendDispatchV1::Accepted;
            }
            emit(event).map_err(|error| {
                failure(
                    BackendStageV1::Execution,
                    dispatch,
                    &format!("event rejected: {error}"),
                )
            })?;
        }
        script.terminal
    }

    fn cleanup(&self, session_id: &str) -> IsolationCleanupV1 {
        let (cleanup, location) = self.state.lock().map_or_else(
            |_| {
                (
                    HermeticCleanupV1::Failed("hermetic backend state is unavailable".to_owned()),
                    self.manifest.execution_location,
                )
            },
            |mut state| {
                state.observed.cleanup_sessions.push(session_id.to_owned());
                let cleanup = state
                    .cleanup_scripts
                    .remove(session_id)
                    .or_else(|| {
                        state
                            .active_scripts
                            .remove(session_id)
                            .map(|script| script.cleanup)
                    })
                    .unwrap_or(HermeticCleanupV1::Verified);
                (cleanup, self.manifest.execution_location)
            },
        );
        cleanup_evidence(session_id, cleanup, location)
    }
}

fn cleanup_evidence(
    session_id: &str,
    cleanup: HermeticCleanupV1,
    location: BackendExecutionLocationV1,
) -> IsolationCleanupV1 {
    let remote_verified = match location {
        BackendExecutionLocationV1::Local => CleanupVerificationV1::NotApplicable,
        BackendExecutionLocationV1::Remote => CleanupVerificationV1::Verified,
    };
    match cleanup {
        HermeticCleanupV1::Verified => IsolationCleanupV1 {
            session_id: session_id.to_owned(),
            process_tree_terminated: CleanupVerificationV1::Verified,
            environment_state_removed: CleanupVerificationV1::Verified,
            remote_session_closed: remote_verified,
            evidence: "hermetic session cleanup verified".to_owned(),
        },
        HermeticCleanupV1::Unverified(detail) => IsolationCleanupV1 {
            session_id: session_id.to_owned(),
            process_tree_terminated: CleanupVerificationV1::Unverified,
            environment_state_removed: CleanupVerificationV1::Unverified,
            remote_session_closed: CleanupVerificationV1::Unverified,
            evidence: detail,
        },
        HermeticCleanupV1::Failed(detail) => IsolationCleanupV1 {
            session_id: session_id.to_owned(),
            process_tree_terminated: CleanupVerificationV1::Failed,
            environment_state_removed: CleanupVerificationV1::Failed,
            remote_session_closed: CleanupVerificationV1::Failed,
            evidence: detail,
        },
    }
}

fn failure(
    stage: BackendStageV1,
    dispatch: BackendDispatchV1,
    detail: &str,
) -> BackendExecutionFailureV1 {
    BackendExecutionFailureV1 {
        stage,
        dispatch,
        detail: detail.to_owned(),
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum HermeticIsolationError {
    #[error("hermetic isolation backend state is unavailable")]
    StateUnavailable,
}
