//! Typed Tauri command translation into the trusted-core repair service.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use aworkit_protocol::{ProcessGeneration, StableId};
use aworkit_trusted_core::{
    ActivateAndRestartV1, AuthorityManifestV1, BootstrapDeadlinesV1, CoreQuiescencePortV1,
    ManagementCheckpointPortV1, QueryActivationCapabilityV1, RejectCandidateV1,
    RepairArtifactIntegrityPortV1, RepairBootstrapPortV1, RepairCandidateDecisionV1,
    RepairCandidateDispositionV1, RepairInvestigationBudgetV1, RepairInvestigationPortV1,
    RepairLedgerPortV1, RepairOrchestratorV1, RequestManagedLocalEnrollmentV1,
    StartInvestigationV1,
};
use serde::{Deserialize, Serialize};

use super::{
    dto::ManagementRepairProjectionDto,
    ports::{
        ManagementRepairProjectionPortV1, durable_ledger_repair_service, unavailable_repair_service,
    },
    projection::{
        RepairProjectionEvent, RepairProjectionGroup, empty_projection, project_aggregates,
    },
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ManagementRepairCommandInput {
    InvestigateAndFix {
        command_id: String,
        error_group_id: String,
    },
    CancelRepairTask {
        command_id: String,
        investigation_id: String,
    },
    ExportPatch {
        command_id: String,
        candidate_id: String,
        expected_candidate_version: u64,
    },
    ExportCandidate {
        command_id: String,
        candidate_id: String,
        expected_candidate_version: u64,
    },
    OpenRebuildInstructions {
        command_id: String,
        candidate_id: String,
        expected_candidate_version: u64,
    },
    RejectCandidate {
        command_id: String,
        candidate_id: String,
        expected_candidate_version: u64,
    },
    RequestManagedLocalEnrollment {
        command_id: String,
        candidate_id: String,
        expected_candidate_version: u64,
        expected_artifact_hash: String,
    },
    RefreshActivationCapability {
        command_id: String,
        candidate_id: String,
        expected_candidate_version: u64,
    },
    ActivateRepairAndRestart {
        command_id: String,
        candidate_id: String,
        expected_candidate_version: u64,
        expected_capability_digest: String,
    },
}

impl ManagementRepairCommandInput {
    fn command_id(&self) -> &str {
        match self {
            Self::InvestigateAndFix { command_id, .. }
            | Self::CancelRepairTask { command_id, .. }
            | Self::ExportPatch { command_id, .. }
            | Self::ExportCandidate { command_id, .. }
            | Self::OpenRebuildInstructions { command_id, .. }
            | Self::RejectCandidate { command_id, .. }
            | Self::RequestManagedLocalEnrollment { command_id, .. }
            | Self::RefreshActivationCapability { command_id, .. }
            | Self::ActivateRepairAndRestart { command_id, .. } => command_id,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementRepairReceipt {
    pub command_id: String,
    pub accepted: bool,
    pub current_version: u64,
    pub reason: Option<String>,
}

#[derive(Clone)]
struct ProcessedCommand {
    fingerprint: String,
    receipt: ManagementRepairReceipt,
}

/// Native gateway keeps only transient accepted-command replay state. Durable
/// candidate, capability, decision, checkpoint, and receipt facts stay in core.
pub struct ManagementRepairGateway {
    service: RepairOrchestratorV1,
    ledger: Arc<dyn RepairLedgerPortV1>,
    projection: Arc<dyn ManagementRepairProjectionPortV1>,
    context: Option<ManagementRepairNativeContext>,
    processed: HashMap<String, ProcessedCommand>,
}

/// Core-owned values supplied by the native composition root. None of these
/// identifiers or authority facts are inferred by the presentation gateway.
pub struct ManagementRepairNativeContext {
    pub authority: AuthorityManifestV1,
    pub management_chat_id: StableId,
    pub management_run_id: StableId,
    pub current_process_generation: ProcessGeneration,
}

impl Default for ManagementRepairGateway {
    fn default() -> Self {
        let (service, ledger, projection) = unavailable_repair_service();
        Self {
            service,
            ledger,
            projection,
            context: None,
            processed: HashMap::new(),
        }
    }
}

impl ManagementRepairGateway {
    /// Production-local composition used when durable repair history is
    /// available but privileged helper/process ports have not been supplied.
    /// Those transitions remain fail-closed instead of being simulated.
    pub fn with_durable_ledger<L>(ledger: Arc<L>) -> Self
    where
        L: RepairLedgerPortV1 + ManagementRepairProjectionPortV1 + 'static,
    {
        let (service, ledger, projection) = durable_ledger_repair_service(ledger);
        Self {
            service,
            ledger,
            projection,
            context: None,
            processed: HashMap::new(),
        }
    }

    /// Composition seam for durable ledger, helper IPC, Run dispatch,
    /// checkpoint, and quiescence adapters. No adapter may fabricate support.
    #[allow(clippy::too_many_arguments)]
    pub fn with_ports(
        ledger: Arc<dyn RepairLedgerPortV1>,
        projection: Arc<dyn ManagementRepairProjectionPortV1>,
        bootstrap: Arc<dyn RepairBootstrapPortV1>,
        investigations: Arc<dyn RepairInvestigationPortV1>,
        management: Arc<dyn ManagementCheckpointPortV1>,
        quiescence: Arc<dyn CoreQuiescencePortV1>,
        artifacts: Arc<dyn RepairArtifactIntegrityPortV1>,
        context: Option<ManagementRepairNativeContext>,
    ) -> Self {
        let core_ledger = ledger.clone();
        Self {
            service: RepairOrchestratorV1::new(
                ledger,
                bootstrap,
                investigations,
                management,
                quiescence,
                artifacts,
            ),
            ledger: core_ledger,
            projection,
            context,
            processed: HashMap::new(),
        }
    }

    pub fn snapshot(&self, after_sequence: u64) -> Result<ManagementRepairProjectionDto, String> {
        let global_version = self
            .projection
            .current_global_version()
            .map_err(|error| error.to_string())?;
        if after_sequence > global_version {
            return Err("Management repair projection cursor is ahead of the core".to_owned());
        }
        let group_ids = self
            .projection
            .group_ids()
            .map_err(|error| error.to_string())?;
        if group_ids.is_empty() {
            return if global_version == 0 {
                Ok(empty_projection())
            } else {
                Err("Management repair global cursor has no durable groups".to_owned())
            };
        }
        let global_events = self
            .projection
            .load_all_global_events()
            .map_err(|error| error.to_string())?;
        validate_global_events(&global_events, global_version, &group_ids)?;
        let aggregates = group_ids
            .iter()
            .map(|group_id| {
                self.service
                    .load_aggregate(group_id)
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        for aggregate in &aggregates {
            let captured_group_version = global_events
                .iter()
                .filter(|event| event.committed.group_id == aggregate.group_id)
                .map(|event| event.committed.ledger_sequence)
                .max()
                .ok_or_else(|| {
                    "Management repair group discovery has no global event history".to_owned()
                })?;
            if aggregate.ledger_version != captured_group_version {
                return Err(
                    "Management repair history changed while the snapshot was being composed"
                        .to_owned(),
                );
            }
        }
        let projected_groups = aggregates
            .iter()
            .map(|aggregate| RepairProjectionGroup {
                aggregate,
                last_global_sequence: global_events
                    .iter()
                    .filter(|event| event.committed.group_id == aggregate.group_id)
                    .map(|event| event.global_sequence)
                    .max()
                    .unwrap_or(0),
            })
            .collect::<Vec<_>>();
        let projected_events = global_events
            .iter()
            .map(|event| RepairProjectionEvent {
                global_sequence: event.global_sequence,
                committed: &event.committed,
            })
            .collect::<Vec<_>>();
        Ok(project_aggregates(
            &projected_groups,
            &projected_events,
            after_sequence,
            global_version,
            now_epoch_ms(),
        ))
    }

    pub fn command(
        &mut self,
        command: ManagementRepairCommandInput,
        expected_version: u64,
    ) -> Result<ManagementRepairReceipt, String> {
        let fingerprint = serde_json::to_string(&command)
            .map_err(|error| format!("invalid Management repair command: {error}"))?;
        if let Some(processed) = self.processed.get(command.command_id()) {
            return if processed.fingerprint == fingerprint {
                Ok(processed.receipt.clone())
            } else {
                Err("Management repair command ID was reused with different content".to_owned())
            };
        }
        let command_id = parse_id(command.command_id())?;
        let current_version = self.current_version()?;
        let durable_replay = if expected_version == current_version {
            false
        } else if let Some(persisted_group) = self.persisted_operation_group(&command_id)? {
            self.command_target_group(&command)?
                .is_some_and(|target_group| target_group == persisted_group)
        } else {
            false
        };
        if expected_version != current_version && !durable_replay {
            return self.store_receipt(
                command_id,
                fingerprint,
                false,
                current_version,
                Some(format!(
                    "repair version conflict: expected {expected_version}, actual {current_version}"
                )),
            );
        }
        let outcome = self.execute(&command, &command_id);
        let next_version = self.current_version().map_err(|error| {
            format!(
                "Management repair command result is uncertain because its durable version could not be read: {error}"
            )
        })?;
        match outcome {
            Ok(()) => self.store_receipt(command_id, fingerprint, true, next_version, None),
            Err(reason) => {
                self.store_receipt(command_id, fingerprint, false, next_version, Some(reason))
            }
        }
    }

    fn execute(
        &self,
        command: &ManagementRepairCommandInput,
        operation_id: &StableId,
    ) -> Result<(), String> {
        match command {
            ManagementRepairCommandInput::InvestigateAndFix {
                error_group_id, ..
            } => self.investigate(error_group_id, operation_id),
            ManagementRepairCommandInput::CancelRepairTask { .. } => Err(
                "The trusted-core repair contract has no unsafe force-cancel transition; committed evidence remains available."
                    .to_owned(),
            ),
            ManagementRepairCommandInput::ExportPatch {
                candidate_id,
                expected_candidate_version,
                ..
            }
            | ManagementRepairCommandInput::ExportCandidate {
                candidate_id,
                expected_candidate_version,
                ..
            }
            | ManagementRepairCommandInput::OpenRebuildInstructions {
                candidate_id,
                expected_candidate_version,
                ..
            } => {
                self.exact_candidate(candidate_id, *expected_candidate_version)?;
                Err(
                    "Native repair artifact export is unavailable; no candidate data was modified."
                        .to_owned(),
                )
            }
            ManagementRepairCommandInput::RejectCandidate {
                candidate_id,
                expected_candidate_version,
                ..
            } => {
                let (group_id, _, group_version) =
                    self.exact_candidate(candidate_id, *expected_candidate_version)?;
                self.service
                    .reject_or_defer_candidate(RejectCandidateV1 {
                        operation_id: operation_id.clone(),
                        expected_ledger_version: group_version,
                        group_id,
                        decision: RepairCandidateDecisionV1 {
                            decision_id: derived_id(operation_id, "decision")?,
                            candidate_id: parse_id(candidate_id)?,
                            candidate_version: *expected_candidate_version,
                            disposition: RepairCandidateDispositionV1::Rejected,
                            reason: "Explicitly rejected in Management repair review".to_owned(),
                        },
                    })
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }
            ManagementRepairCommandInput::RefreshActivationCapability {
                candidate_id,
                expected_candidate_version,
                ..
            } => {
                let (group_id, candidate_hash, group_version) =
                    self.exact_candidate(candidate_id, *expected_candidate_version)?;
                self.service
                    .query_activation_capability(QueryActivationCapabilityV1 {
                        operation_id: operation_id.clone(),
                        expected_ledger_version: group_version,
                        group_id,
                        candidate_id: parse_id(candidate_id)?,
                        expected_candidate_version: *expected_candidate_version,
                        expected_candidate_hash: candidate_hash,
                        now_epoch_ms: now_epoch_ms(),
                    })
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }
            ManagementRepairCommandInput::RequestManagedLocalEnrollment {
                candidate_id,
                expected_candidate_version,
                expected_artifact_hash,
                ..
            } => self.enroll(
                operation_id,
                candidate_id,
                *expected_candidate_version,
                expected_artifact_hash,
            ),
            ManagementRepairCommandInput::ActivateRepairAndRestart {
                candidate_id,
                expected_candidate_version,
                expected_capability_digest,
                ..
            } => self.activate(
                operation_id,
                candidate_id,
                *expected_candidate_version,
                expected_capability_digest,
            ),
        }
    }

    fn investigate(&self, error_group_id: &str, operation_id: &StableId) -> Result<(), String> {
        let group_id = parse_id(error_group_id)?;
        let aggregate = self
            .service
            .load_aggregate(&group_id)
            .map_err(|error| error.to_string())?;
        let context = self.context.as_ref().ok_or_else(|| {
            "Frozen Management authority is unavailable; review cannot broaden authority."
                .to_owned()
        })?;
        let requested_capability_ids = context
            .authority
            .capability_bindings
            .iter()
            .filter(|binding| binding.enabled && binding.compatible)
            .map(|binding| binding.capability_id.clone())
            .collect::<Vec<_>>();
        self.service
            .start_bounded_investigation(
                StartInvestigationV1 {
                    operation_id: operation_id.clone(),
                    expected_ledger_version: aggregate.ledger_version,
                    investigation_id: derived_id(operation_id, "investigation")?,
                    explicit_user_decision_id: derived_id(operation_id, "decision")?,
                    group_id,
                    management_chat_id: context.management_chat_id.clone(),
                    management_run_id: context.management_run_id.clone(),
                    requested_capability_ids,
                    budget: RepairInvestigationBudgetV1 {
                        max_attempts: 8,
                        max_tool_calls: 64,
                        max_tokens: 250_000,
                        deadline_ms: 20 * 60 * 1_000,
                    },
                },
                &context.authority,
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn enroll(
        &self,
        operation_id: &StableId,
        candidate_id: &str,
        candidate_version: u64,
        expected_artifact_hash: &str,
    ) -> Result<(), String> {
        let (group_id, candidate_hash, group_version) =
            self.exact_candidate(candidate_id, candidate_version)?;
        let aggregate = self
            .service
            .load_aggregate(&group_id)
            .map_err(|error| error.to_string())?;
        let candidate = aggregate
            .candidate_exact(&parse_id(candidate_id)?, candidate_version)
            .ok_or_else(|| "The requested candidate version is missing.".to_owned())?;
        if candidate.build_bundle.artifact.content_hash != expected_artifact_hash {
            return Err("The candidate artifact hash changed.".to_owned());
        }
        let report = aggregate
            .latest_capability_report
            .as_ref()
            .ok_or_else(|| "An activation capability report is required.".to_owned())?;
        self.service
            .request_managed_local_enrollment(RequestManagedLocalEnrollmentV1 {
                operation_id: operation_id.clone(),
                request_id: derived_id(operation_id, "enrollment")?,
                explicit_user_decision_id: derived_id(operation_id, "decision")?,
                expected_ledger_version: group_version,
                group_id,
                candidate_id: parse_id(candidate_id)?,
                expected_candidate_version: candidate_version,
                expected_candidate_hash: candidate_hash,
                expected_capability_report_id: report.report_id.clone(),
                expected_capability_digest: report.capability_digest.clone(),
                now_epoch_ms: now_epoch_ms(),
            })
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn activate(
        &self,
        operation_id: &StableId,
        candidate_id: &str,
        candidate_version: u64,
        expected_capability_digest: &str,
    ) -> Result<(), String> {
        let (group_id, candidate_hash, group_version) =
            self.exact_candidate(candidate_id, candidate_version)?;
        let aggregate = self
            .service
            .load_aggregate(&group_id)
            .map_err(|error| error.to_string())?;
        let report = aggregate
            .latest_capability_report
            .as_ref()
            .ok_or_else(|| "An activation capability report is required.".to_owned())?;
        if report.capability_digest != expected_capability_digest {
            return Err("The capability generation digest changed.".to_owned());
        }
        let process_generation = self
            .context
            .as_ref()
            .map(|context| context.current_process_generation)
            .ok_or_else(|| {
                "Current process generation is unavailable; activation remains disabled.".to_owned()
            })?;
        self.service
            .activate_and_restart(ActivateAndRestartV1 {
                operation_id: operation_id.clone(),
                expected_ledger_version: group_version,
                group_id,
                activation_id: derived_id(operation_id, "activation")?,
                baton_id: derived_id(operation_id, "baton")?,
                explicit_user_decision_id: derived_id(operation_id, "decision")?,
                candidate_id: parse_id(candidate_id)?,
                expected_candidate_version: candidate_version,
                expected_candidate_hash: candidate_hash,
                expected_capability_report_id: report.report_id.clone(),
                expected_capability_digest: expected_capability_digest.to_owned(),
                current_process_generation: process_generation,
                deadlines: BootstrapDeadlinesV1 {
                    admission_ms: 10_000,
                    cleanup_ms: 30_000,
                    startup_ms: 60_000,
                    focused_verification_ms: 120_000,
                    rollback_ms: 60_000,
                    result_read_ms: 10_000,
                },
                now_epoch_ms: now_epoch_ms(),
            })
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn exact_candidate(
        &self,
        candidate_id: &str,
        candidate_version: u64,
    ) -> Result<(StableId, String, u64), String> {
        let candidate_id = parse_id(candidate_id)?;
        let mut exact = None;
        for group_id in self
            .projection
            .group_ids()
            .map_err(|error| error.to_string())?
        {
            let aggregate = self
                .service
                .load_aggregate(&group_id)
                .map_err(|error| error.to_string())?;
            if let Some(candidate) = aggregate.candidate_exact(&candidate_id, candidate_version) {
                if exact.is_some() {
                    return Err(
                        "The requested candidate identity is ambiguous across repair groups."
                            .to_owned(),
                    );
                }
                exact = Some((
                    group_id,
                    candidate.candidate_hash.clone(),
                    aggregate.ledger_version,
                ));
            }
        }
        exact.ok_or_else(|| "The requested candidate version is missing or stale.".to_owned())
    }

    fn current_version(&self) -> Result<u64, String> {
        self.projection
            .current_global_version()
            .map_err(|error| error.to_string())
    }

    /// Locates an already committed operation before global-version rejection.
    /// This is the only stale-version exception: trusted core still compares
    /// the exact persisted event payload and rejects an ID reused differently.
    fn persisted_operation_group(
        &self,
        operation_id: &StableId,
    ) -> Result<Option<StableId>, String> {
        let mut found = None;
        for group_id in self
            .projection
            .group_ids()
            .map_err(|error| error.to_string())?
        {
            if self
                .ledger
                .load_operation(&group_id, operation_id)
                .map_err(|error| error.to_string())?
                .is_some()
            {
                if found.is_some() {
                    return Err(
                        "Management repair operation identity is ambiguous across durable groups."
                            .to_owned(),
                    );
                }
                found = Some(group_id);
            }
        }
        Ok(found)
    }

    /// Resolves only explicit/core-owned command identity. This performs no
    /// helper, artifact, checkpoint, dispatch, or mutation work.
    fn command_target_group(
        &self,
        command: &ManagementRepairCommandInput,
    ) -> Result<Option<StableId>, String> {
        match command {
            ManagementRepairCommandInput::InvestigateAndFix { error_group_id, .. } => {
                parse_id(error_group_id).map(Some)
            }
            ManagementRepairCommandInput::ExportPatch {
                candidate_id,
                expected_candidate_version,
                ..
            }
            | ManagementRepairCommandInput::ExportCandidate {
                candidate_id,
                expected_candidate_version,
                ..
            }
            | ManagementRepairCommandInput::OpenRebuildInstructions {
                candidate_id,
                expected_candidate_version,
                ..
            }
            | ManagementRepairCommandInput::RejectCandidate {
                candidate_id,
                expected_candidate_version,
                ..
            }
            | ManagementRepairCommandInput::RequestManagedLocalEnrollment {
                candidate_id,
                expected_candidate_version,
                ..
            }
            | ManagementRepairCommandInput::RefreshActivationCapability {
                candidate_id,
                expected_candidate_version,
                ..
            }
            | ManagementRepairCommandInput::ActivateRepairAndRestart {
                candidate_id,
                expected_candidate_version,
                ..
            } => self
                .exact_candidate(candidate_id, *expected_candidate_version)
                .map(|(group_id, _, _)| Some(group_id)),
            ManagementRepairCommandInput::CancelRepairTask { .. } => Ok(None),
        }
    }

    fn store_receipt(
        &mut self,
        command_id: StableId,
        fingerprint: String,
        accepted: bool,
        current_version: u64,
        reason: Option<String>,
    ) -> Result<ManagementRepairReceipt, String> {
        let receipt = ManagementRepairReceipt {
            command_id: command_id.to_string(),
            accepted,
            current_version,
            reason,
        };
        // Explicit rejections do not consume an idempotency key. The caller
        // may resynchronize and retry the same semantic command, while an
        // accepted transition remains replayable without a second mutation.
        if accepted {
            self.processed.insert(
                command_id.to_string(),
                ProcessedCommand {
                    fingerprint,
                    receipt: receipt.clone(),
                },
            );
        }
        Ok(receipt)
    }
}

fn validate_global_events(
    events: &[super::ports::GloballyCommittedRepairEventV1],
    global_version: u64,
    group_ids: &[StableId],
) -> Result<(), String> {
    let known_groups = group_ids
        .iter()
        .map(StableId::as_str)
        .collect::<HashSet<_>>();
    if known_groups.len() != group_ids.len() {
        return Err("Management repair group discovery returned a duplicate identity".to_owned());
    }
    let mut group_sequences = HashMap::<&str, u64>::new();
    let mut expected = 1_u64;
    for event in events {
        if event.global_sequence != expected {
            return Err("Management repair global event cursor is not contiguous".to_owned());
        }
        let group_id = event.committed.group_id.as_str();
        if !known_groups.contains(group_id) {
            return Err(
                "Management repair global history references an undiscovered group".to_owned(),
            );
        }
        let group_sequence = group_sequences.entry(group_id).or_default();
        let expected_group_sequence = group_sequence
            .checked_add(1)
            .ok_or_else(|| "Management repair group event cursor is exhausted".to_owned())?;
        if event.committed.ledger_sequence != expected_group_sequence {
            return Err("Management repair group event cursor is not contiguous".to_owned());
        }
        *group_sequence = expected_group_sequence;
        expected = expected
            .checked_add(1)
            .ok_or_else(|| "Management repair global event cursor is exhausted".to_owned())?;
    }
    if events.last().map_or(0, |event| event.global_sequence) != global_version {
        return Err(
            "Management repair global event version does not match durable events".to_owned(),
        );
    }
    if group_sequences.len() != known_groups.len() {
        return Err("Management repair group discovery has no global event history".to_owned());
    }
    Ok(())
}

fn parse_id(value: &str) -> Result<StableId, String> {
    StableId::parse(value.to_owned()).map_err(|error| error.to_string())
}

fn derived_id(base: &StableId, suffix: &str) -> Result<StableId, String> {
    if base.as_str().len() + suffix.len() + 1 > 128 {
        return Err(
            "Management repair command ID is too long for derived operation IDs".to_owned(),
        );
    }
    parse_id(&format!("{}.{}", base.as_str(), suffix))
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aworkit_trusted_core::{
        CommittedRepairEventV1, ErrorOccurrenceV1, RepairEventV1, RepairLedgerAppendOutcomeV1,
        RepairLedgerAppendV1, RepairPortErrorV1, repair_group_id_for_fingerprint_v1,
    };
    use serde_json::json;

    #[test]
    fn empty_native_projection_is_fail_closed() {
        let gateway = ManagementRepairGateway::default();
        let projection = gateway.snapshot(0).expect("empty snapshot");

        assert_eq!(projection.version, 0);
        assert_eq!(projection.last_sequence, 0);
        assert!(projection.error_groups.is_empty());
        assert!(projection.candidates.is_empty());
        assert!(projection.capability_reports.is_empty());
        assert!(projection.restart_recovery.is_none());
    }

    #[test]
    fn command_input_accepts_camel_case_and_rejects_extra_fields() {
        let command: ManagementRepairCommandInput = serde_json::from_value(json!({
            "type": "activate_repair_and_restart",
            "commandId": "command.activate",
            "candidateId": "candidate.one",
            "expectedCandidateVersion": 3,
            "expectedCapabilityDigest": "sha256:capability"
        }))
        .expect("typed command");
        assert!(matches!(
            command,
            ManagementRepairCommandInput::ActivateRepairAndRestart {
                expected_candidate_version: 3,
                ..
            }
        ));

        let error = serde_json::from_value::<ManagementRepairCommandInput>(json!({
            "type": "reject_candidate",
            "commandId": "command.reject",
            "candidateId": "candidate.one",
            "expectedCandidateVersion": 3,
            "force": true
        }))
        .expect_err("unknown authority-broadening fields must fail");
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rejected_version_conflict_does_not_consume_command_id() {
        let mut gateway = ManagementRepairGateway::default();
        let command = ManagementRepairCommandInput::RefreshActivationCapability {
            command_id: "command.refresh".to_owned(),
            candidate_id: "candidate.one".to_owned(),
            expected_candidate_version: 1,
        };

        let stale = gateway
            .command(command.clone(), 1)
            .expect("explicit version rejection");
        assert!(!stale.accepted);
        assert!(
            stale
                .reason
                .as_deref()
                .is_some_and(|value| value.contains("version conflict"))
        );

        let synchronized = gateway
            .command(command, 0)
            .expect("same command ID remains retryable after rejection");
        assert!(!synchronized.accepted);
        assert!(
            synchronized
                .reason
                .as_deref()
                .is_some_and(|value| value.contains("missing or stale"))
        );
        assert!(
            gateway
                .snapshot(0)
                .expect("snapshot")
                .capability_reports
                .is_empty()
        );
    }

    #[test]
    fn accepted_command_replay_is_not_bound_to_a_stale_projection_cursor() {
        let mut gateway = ManagementRepairGateway::default();
        let command = ManagementRepairCommandInput::RefreshActivationCapability {
            command_id: "command.accepted.replay".to_owned(),
            candidate_id: "candidate.one".to_owned(),
            expected_candidate_version: 1,
        };
        let command_id = parse_id(command.command_id()).expect("command id");
        let fingerprint = serde_json::to_string(&command).expect("command fingerprint");
        gateway
            .store_receipt(command_id, fingerprint, true, 7, None)
            .expect("accepted receipt");

        let replay = gateway
            .command(command, 99)
            .expect("same semantic command replays after resynchronization");
        assert!(replay.accepted);
        assert_eq!(replay.current_version, 7);
    }

    #[test]
    fn snapshot_rejects_group_replay_newer_than_captured_global_history() {
        let ledger = Arc::new(MixedSnapshotLedger::new());
        let gateway = ManagementRepairGateway::with_durable_ledger(ledger);

        let error = gateway.snapshot(0).expect_err("mixed snapshot must fail");
        assert!(error.contains("changed while the snapshot was being composed"));
    }

    struct MixedSnapshotLedger {
        group_id: StableId,
        events: Vec<CommittedRepairEventV1>,
    }

    impl MixedSnapshotLedger {
        fn new() -> Self {
            let fingerprint = format!("sha256:{}", "d".repeat(64));
            let group_id = repair_group_id_for_fingerprint_v1(&fingerprint).expect("repair group");
            let events = ["one", "two"]
                .into_iter()
                .enumerate()
                .map(|(index, suffix)| CommittedRepairEventV1 {
                    group_id: group_id.clone(),
                    ledger_sequence: index as u64 + 1,
                    operation_id: parse_id(&format!("operation.mixed.{suffix}"))
                        .expect("operation id"),
                    event: RepairEventV1::FailureRecorded {
                        occurrence: ErrorOccurrenceV1 {
                            occurrence_id: parse_id(&format!("occurrence.mixed.{suffix}"))
                                .expect("occurrence id"),
                            fingerprint: fingerprint.clone(),
                            summary: format!("Mixed snapshot failure {suffix}"),
                            semantic_event_id: parse_id(&format!("semantic.mixed.{suffix}"))
                                .expect("semantic id"),
                            attempt_id: None,
                            diagnostic_record_id: None,
                            evidence: Vec::new(),
                            observed_at_epoch_ms: index as u64 + 1,
                        },
                    },
                })
                .collect();
            Self { group_id, events }
        }
    }

    impl RepairLedgerPortV1 for MixedSnapshotLedger {
        fn load_group(
            &self,
            group_id: &StableId,
        ) -> Result<Vec<CommittedRepairEventV1>, RepairPortErrorV1> {
            assert_eq!(group_id, &self.group_id);
            Ok(self.events.clone())
        }

        fn append(
            &self,
            _request: RepairLedgerAppendV1,
        ) -> Result<RepairLedgerAppendOutcomeV1, RepairPortErrorV1> {
            panic!("mixed snapshot test never appends")
        }
    }

    impl ManagementRepairProjectionPortV1 for MixedSnapshotLedger {
        fn group_ids(&self) -> Result<Vec<StableId>, RepairPortErrorV1> {
            Ok(vec![self.group_id.clone()])
        }

        fn load_all_global_events(
            &self,
        ) -> Result<Vec<super::super::ports::GloballyCommittedRepairEventV1>, RepairPortErrorV1>
        {
            Ok(vec![super::super::ports::GloballyCommittedRepairEventV1 {
                global_sequence: 1,
                committed: self.events[0].clone(),
            }])
        }

        fn current_global_version(&self) -> Result<u64, RepairPortErrorV1> {
            Ok(1)
        }
    }
}
