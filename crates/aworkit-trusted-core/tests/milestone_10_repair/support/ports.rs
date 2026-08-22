use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use aworkit_protocol::StableId;
use aworkit_trusted_core::*;

use super::{authenticated_result, hash, id};

type TestLog = Arc<Mutex<Vec<String>>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionMode {
    Accepted,
    Unsupported,
}

#[derive(Default)]
struct LedgerState {
    groups: BTreeMap<String, Vec<CommittedRepairEventV1>>,
    operations: BTreeMap<(String, String), Vec<RepairEventV1>>,
}

pub struct MemoryRepairLedger {
    state: Mutex<LedgerState>,
    pub fail_append_after_commit_once: AtomicBool,
    log: TestLog,
}

impl MemoryRepairLedger {
    pub fn new(log: TestLog) -> Self {
        Self {
            state: Mutex::new(LedgerState::default()),
            fail_append_after_commit_once: AtomicBool::new(false),
            log,
        }
    }
}

impl RepairLedgerPortV1 for MemoryRepairLedger {
    fn load_group(
        &self,
        group_id: &StableId,
    ) -> Result<Vec<CommittedRepairEventV1>, RepairPortErrorV1> {
        Ok(self
            .state
            .lock()
            .map_err(|_| error("ledger_lock"))?
            .groups
            .get(group_id.as_str())
            .cloned()
            .unwrap_or_default())
    }

    fn append(
        &self,
        request: RepairLedgerAppendV1,
    ) -> Result<RepairLedgerAppendOutcomeV1, RepairPortErrorV1> {
        let mut state = self.state.lock().map_err(|_| error("ledger_lock"))?;
        let operation_key = (
            request.group_id.as_str().to_owned(),
            request.operation_id.as_str().to_owned(),
        );
        if let Some(previous) = state.operations.get(&operation_key) {
            if previous != &request.events {
                return Err(error("operation_conflict"));
            }
            let ledger_version = state
                .groups
                .get(request.group_id.as_str())
                .map_or(0, |events| events.len() as u64);
            self.log
                .lock()
                .map_err(|_| error("log_lock"))?
                .push("ledger.duplicate".into());
            return Ok(RepairLedgerAppendOutcomeV1 {
                ledger_version,
                duplicate: true,
            });
        }
        let group = state
            .groups
            .entry(request.group_id.as_str().to_owned())
            .or_default();
        if group.len() as u64 != request.expected_ledger_version {
            return Err(error("ledger_cas"));
        }
        for event in &request.events {
            group.push(CommittedRepairEventV1 {
                group_id: request.group_id.clone(),
                ledger_sequence: group.len() as u64 + 1,
                operation_id: request.operation_id.clone(),
                event: event.clone(),
            });
            self.log
                .lock()
                .map_err(|_| error("log_lock"))?
                .push(format!("ledger.{}", event_name(event)));
        }
        let ledger_version = group.len() as u64;
        state.operations.insert(operation_key, request.events);
        if self
            .fail_append_after_commit_once
            .swap(false, Ordering::SeqCst)
        {
            return Err(error("ledger_ack_uncertain"));
        }
        Ok(RepairLedgerAppendOutcomeV1 {
            ledger_version,
            duplicate: false,
        })
    }
}

pub struct FakeInvestigation {
    pub calls: AtomicUsize,
    pub effects: AtomicUsize,
    pub dispatches: Mutex<Vec<RepairInvestigationDispatchV1>>,
    pub receipt: Mutex<Option<AuthenticatedInvestigationExecutionReceiptV1>>,
    pub fail_dispatch_after_effect_once: AtomicBool,
    log: TestLog,
}

impl FakeInvestigation {
    pub fn new(log: TestLog) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            effects: AtomicUsize::new(0),
            dispatches: Mutex::new(Vec::new()),
            receipt: Mutex::new(None),
            fail_dispatch_after_effect_once: AtomicBool::new(false),
            log,
        }
    }
}

impl RepairInvestigationPortV1 for FakeInvestigation {
    fn dispatch(&self, request: RepairInvestigationDispatchV1) -> Result<(), RepairPortErrorV1> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut dispatches = self.dispatches.lock().map_err(|_| error("dispatch_lock"))?;
        if !dispatches
            .iter()
            .any(|existing| existing.operation_id == request.operation_id)
        {
            self.effects.fetch_add(1, Ordering::SeqCst);
            dispatches.push(request);
        }
        drop(dispatches);
        self.log
            .lock()
            .map_err(|_| error("log_lock"))?
            .push("investigation.dispatch".into());
        if self
            .fail_dispatch_after_effect_once
            .swap(false, Ordering::SeqCst)
        {
            return Err(error("dispatch_uncertain"));
        }
        Ok(())
    }

    fn read_execution_receipt(
        &self,
        _query: InvestigationExecutionReceiptQueryV1,
    ) -> Result<AuthenticatedInvestigationExecutionReceiptV1, RepairPortErrorV1> {
        self.log
            .lock()
            .map_err(|_| error("log_lock"))?
            .push("investigation.receipt".into());
        self.receipt
            .lock()
            .map_err(|_| error("receipt_lock"))?
            .clone()
            .ok_or_else(|| error("receipt_missing"))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactFault {
    Missing,
    Unavailable,
    HashMismatch,
    SizeMismatch,
}

pub struct FakeArtifactIntegrity {
    pub calls: AtomicUsize,
    pub fault: Mutex<Option<(StableId, ArtifactFault)>>,
    log: TestLog,
}

impl FakeArtifactIntegrity {
    pub fn new(log: TestLog) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            fault: Mutex::new(None),
            log,
        }
    }
}

impl RepairArtifactIntegrityPortV1 for FakeArtifactIntegrity {
    fn verify_ready(
        &self,
        request: RepairArtifactVerificationRequestV1,
    ) -> Result<RepairArtifactReadinessV1, RepairPortErrorV1> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.log
            .lock()
            .map_err(|_| error("log_lock"))?
            .push("artifact.verify".into());
        let fault = self
            .fault
            .lock()
            .map_err(|_| error("artifact_fault_lock"))?
            .as_ref()
            .filter(|(artifact_id, _)| artifact_id == &request.artifact.artifact_id)
            .map(|(_, fault)| *fault);
        Ok(match fault {
            Some(ArtifactFault::Missing) => RepairArtifactReadinessV1::Missing {
                artifact_id: request.artifact.artifact_id,
            },
            Some(ArtifactFault::Unavailable) => RepairArtifactReadinessV1::Unavailable {
                artifact_id: request.artifact.artifact_id,
                reason: "artifact repository unavailable".into(),
            },
            Some(ArtifactFault::HashMismatch) => RepairArtifactReadinessV1::Ready {
                artifact_id: request.artifact.artifact_id,
                observed_content_hash: if request.artifact.content_hash == super::hash('0') {
                    super::hash('1')
                } else {
                    super::hash('0')
                },
                observed_byte_size: request.artifact.byte_size,
            },
            Some(ArtifactFault::SizeMismatch) => RepairArtifactReadinessV1::Ready {
                artifact_id: request.artifact.artifact_id,
                observed_content_hash: request.artifact.content_hash,
                observed_byte_size: request.artifact.byte_size.saturating_add(1),
            },
            None => RepairArtifactReadinessV1::Ready {
                artifact_id: request.artifact.artifact_id,
                observed_content_hash: request.artifact.content_hash,
                observed_byte_size: request.artifact.byte_size,
            },
        })
    }
}

pub struct FakeBootstrap {
    pub report: Mutex<Option<PlatformCapabilityReportV1>>,
    pub result: Mutex<Option<AuthenticatedBootstrapResultV1>>,
    pub last_baton: Mutex<Option<RepairActivationBatonV1>>,
    pub submitted_verification: Mutex<Vec<FocusedVerificationEvidenceV1>>,
    pub query_calls: AtomicUsize,
    pub enrollment_calls: AtomicUsize,
    pub enrollment_effects: AtomicUsize,
    pub fail_enrollment_after_effect_once: AtomicBool,
    pub admission_calls: AtomicUsize,
    pub admission_effects: AtomicUsize,
    pub fail_admission_after_effect_once: AtomicBool,
    pub quiescence_calls: AtomicUsize,
    pub quiescence_effects: AtomicUsize,
    pub fail_quiescence_after_effect_once: AtomicBool,
    pub result_calls: AtomicUsize,
    mode: AdmissionMode,
    enrollment_cache: Mutex<Option<(StableId, EnrollmentPreparedV1)>>,
    admission_cache: Mutex<Option<(String, BootstrapAdmissionV1)>>,
    quiescence_seen: Mutex<BTreeSet<(String, String)>>,
    log: TestLog,
}

impl FakeBootstrap {
    pub fn new(log: TestLog, mode: AdmissionMode) -> Self {
        Self {
            report: Mutex::new(None),
            result: Mutex::new(None),
            last_baton: Mutex::new(None),
            submitted_verification: Mutex::new(Vec::new()),
            query_calls: AtomicUsize::new(0),
            enrollment_calls: AtomicUsize::new(0),
            enrollment_effects: AtomicUsize::new(0),
            fail_enrollment_after_effect_once: AtomicBool::new(false),
            admission_calls: AtomicUsize::new(0),
            admission_effects: AtomicUsize::new(0),
            fail_admission_after_effect_once: AtomicBool::new(false),
            quiescence_calls: AtomicUsize::new(0),
            quiescence_effects: AtomicUsize::new(0),
            fail_quiescence_after_effect_once: AtomicBool::new(false),
            result_calls: AtomicUsize::new(0),
            mode,
            enrollment_cache: Mutex::new(None),
            admission_cache: Mutex::new(None),
            quiescence_seen: Mutex::new(BTreeSet::new()),
            log,
        }
    }
}

impl RepairBootstrapPortV1 for FakeBootstrap {
    fn query_activation_capability(
        &self,
        _query: ActivationCapabilityQueryV1,
    ) -> Result<PlatformCapabilityReportV1, RepairPortErrorV1> {
        self.query_calls.fetch_add(1, Ordering::SeqCst);
        self.log
            .lock()
            .map_err(|_| error("log_lock"))?
            .push("bootstrap.query".into());
        self.report
            .lock()
            .map_err(|_| error("report_lock"))?
            .clone()
            .ok_or_else(|| error("report_missing"))
    }

    fn prepare_managed_local_enrollment(
        &self,
        request: ManagedLocalEnrollmentRequestV1,
    ) -> Result<EnrollmentPreparedV1, RepairPortErrorV1> {
        self.enrollment_calls.fetch_add(1, Ordering::SeqCst);
        self.log
            .lock()
            .map_err(|_| error("log_lock"))?
            .push("bootstrap.enroll".into());
        let mut cache = self
            .enrollment_cache
            .lock()
            .map_err(|_| error("enrollment_cache_lock"))?;
        let prepared = match cache.as_ref() {
            Some((request_id, prepared)) if request_id == &request.request_id => prepared.clone(),
            Some(_) => return Err(error("enrollment_idempotency_conflict")),
            None => {
                self.enrollment_effects.fetch_add(1, Ordering::SeqCst);
                let prepared = EnrollmentPreparedV1 {
                    preparation_id: id("enrollment.prepared.one"),
                    request_id: request.request_id.clone(),
                    enrollment_digest: hash('a'),
                    stable_launcher: "aworkit-stable-launcher".into(),
                    restart_instructions: vec![
                        "restart explicitly through the stable launcher".into(),
                    ],
                };
                *cache = Some((request.request_id, prepared.clone()));
                prepared
            }
        };
        drop(cache);
        if self
            .fail_enrollment_after_effect_once
            .swap(false, Ordering::SeqCst)
        {
            return Err(error("enrollment_uncertain"));
        }
        Ok(prepared)
    }

    fn admit_activation(
        &self,
        baton: RepairActivationBatonV1,
    ) -> Result<BootstrapAdmissionV1, RepairPortErrorV1> {
        self.admission_calls.fetch_add(1, Ordering::SeqCst);
        self.log
            .lock()
            .map_err(|_| error("log_lock"))?
            .push("bootstrap.admit".into());
        *self.last_baton.lock().map_err(|_| error("baton_lock"))? = Some(baton.clone());
        let mut cache = self
            .admission_cache
            .lock()
            .map_err(|_| error("admission_cache_lock"))?;
        let response = if let Some((baton_hash, response)) = cache.as_ref() {
            if baton_hash != &baton.baton_hash {
                return Err(error("admission_idempotency_conflict"));
            }
            response.clone()
        } else {
            self.admission_effects.fetch_add(1, Ordering::SeqCst);
            let response = match self.mode {
                AdmissionMode::Accepted => {
                    let mut admission = BootstrapAcceptedAdmissionV1 {
                        admission_id: id("bootstrap.admission.one"),
                        activation_id: baton.activation_id.clone(),
                        baton_hash: baton.baton_hash.clone(),
                        candidate_process_generation: baton.candidate_process_generation,
                        rollback_process_generation: baton.rollback_process_generation,
                        admission_hash: String::new(),
                    };
                    admission.admission_hash =
                        bootstrap_admission_hash_v1(&admission).expect("admission hash");
                    BootstrapAdmissionV1::Accepted(admission)
                }
                AdmissionMode::Unsupported => {
                    BootstrapAdmissionV1::Unsupported(authenticated_result(
                        &baton,
                        BootstrapResultKindV1::Unsupported {
                            reason: PlatformReasonV1 {
                                code: "capability_generation_changed".into(),
                                message: "activation guarantees changed before quiescence".into(),
                                next_steps: vec!["query activation capability again".into()],
                            },
                        },
                        baton.current_process_generation,
                    ))
                }
            };
            *cache = Some((baton.baton_hash.clone(), response.clone()));
            response
        };
        drop(cache);
        if self
            .fail_admission_after_effect_once
            .swap(false, Ordering::SeqCst)
        {
            return Err(error("admission_uncertain"));
        }
        Ok(response)
    }

    fn record_core_quiescence(
        &self,
        admission_id: &StableId,
        facts: CoreQuiescenceFactsV1,
    ) -> Result<(), RepairPortErrorV1> {
        self.quiescence_calls.fetch_add(1, Ordering::SeqCst);
        self.log
            .lock()
            .map_err(|_| error("log_lock"))?
            .push("bootstrap.quiescence".into());
        let mut seen = self
            .quiescence_seen
            .lock()
            .map_err(|_| error("quiescence_seen_lock"))?;
        if seen.insert((admission_id.as_str().into(), facts.facts_hash)) {
            self.quiescence_effects.fetch_add(1, Ordering::SeqCst);
        }
        drop(seen);
        if self
            .fail_quiescence_after_effect_once
            .swap(false, Ordering::SeqCst)
        {
            return Err(error("quiescence_handoff_uncertain"));
        }
        Ok(())
    }

    fn submit_focused_verification(
        &self,
        _activation_id: &StableId,
        evidence: FocusedVerificationEvidenceV1,
    ) -> Result<(), RepairPortErrorV1> {
        self.submitted_verification
            .lock()
            .map_err(|_| error("verification_lock"))?
            .push(evidence);
        self.log
            .lock()
            .map_err(|_| error("log_lock"))?
            .push("bootstrap.verification".into());
        Ok(())
    }

    fn read_result(
        &self,
        _query: BootstrapResultQueryV1,
    ) -> Result<Option<AuthenticatedBootstrapResultV1>, RepairPortErrorV1> {
        self.result_calls.fetch_add(1, Ordering::SeqCst);
        self.log
            .lock()
            .map_err(|_| error("log_lock"))?
            .push("bootstrap.result".into());
        Ok(self
            .result
            .lock()
            .map_err(|_| error("result_lock"))?
            .clone())
    }
}

pub struct FakeManagement {
    pub checkpoint_calls: AtomicUsize,
    pub checkpoint_effects: AtomicUsize,
    pub fail_checkpoint_after_effect_once: AtomicBool,
    pub resume_calls: AtomicUsize,
    pub resume_effects: AtomicUsize,
    pub resumes: Mutex<Vec<ManagementResumeRequestV1>>,
    pub fail_resume_once: AtomicBool,
    checkpoint_cache: Mutex<Option<(StableId, ManagementCheckpointRefV1)>>,
    log: TestLog,
}

impl FakeManagement {
    pub fn new(log: TestLog) -> Self {
        Self {
            checkpoint_calls: AtomicUsize::new(0),
            checkpoint_effects: AtomicUsize::new(0),
            fail_checkpoint_after_effect_once: AtomicBool::new(false),
            resume_calls: AtomicUsize::new(0),
            resume_effects: AtomicUsize::new(0),
            resumes: Mutex::new(Vec::new()),
            fail_resume_once: AtomicBool::new(false),
            checkpoint_cache: Mutex::new(None),
            log,
        }
    }
}

impl ManagementCheckpointPortV1 for FakeManagement {
    fn create_checkpoint(
        &self,
        request: ManagementCheckpointRequestV1,
    ) -> Result<ManagementCheckpointRefV1, RepairPortErrorV1> {
        self.checkpoint_calls.fetch_add(1, Ordering::SeqCst);
        self.log
            .lock()
            .map_err(|_| error("log_lock"))?
            .push("management.checkpoint".into());
        let mut cache = self
            .checkpoint_cache
            .lock()
            .map_err(|_| error("checkpoint_cache_lock"))?;
        let checkpoint = match cache.as_ref() {
            Some((activation_id, checkpoint)) if activation_id == &request.activation_id => {
                checkpoint.clone()
            }
            Some(_) => return Err(error("checkpoint_idempotency_conflict")),
            None => {
                self.checkpoint_effects.fetch_add(1, Ordering::SeqCst);
                let checkpoint = ManagementCheckpointRefV1 {
                    checkpoint_id: id("management.checkpoint.one"),
                    chat_id: request.management_chat_id,
                    run_id: request.management_run_id,
                    committed_sequence: 41,
                    snapshot_hash: hash('6'),
                    checkpoint_hash: hash('7'),
                };
                *cache = Some((request.activation_id, checkpoint.clone()));
                checkpoint
            }
        };
        drop(cache);
        if self
            .fail_checkpoint_after_effect_once
            .swap(false, Ordering::SeqCst)
        {
            return Err(error("checkpoint_uncertain"));
        }
        Ok(checkpoint)
    }

    fn resume_same_chat(
        &self,
        request: ManagementResumeRequestV1,
    ) -> Result<(), RepairPortErrorV1> {
        self.resume_calls.fetch_add(1, Ordering::SeqCst);
        self.log
            .lock()
            .map_err(|_| error("log_lock"))?
            .push("management.resume".into());
        let mut resumes = self.resumes.lock().map_err(|_| error("resume_lock"))?;
        if !resumes
            .iter()
            .any(|existing| existing.receipt_id == request.receipt_id)
        {
            self.resume_effects.fetch_add(1, Ordering::SeqCst);
            resumes.push(request);
        }
        if self.fail_resume_once.swap(false, Ordering::SeqCst) {
            return Err(error("resume_interrupted"));
        }
        Ok(())
    }
}

pub struct FakeQuiescence {
    pub calls: AtomicUsize,
    pub effects: AtomicUsize,
    pub fail_after_effect_once: AtomicBool,
    pub unsafe_mode: Mutex<Option<(bool, bool)>>,
    cached: Mutex<Option<CoreQuiescenceFactsV1>>,
    log: TestLog,
}

impl FakeQuiescence {
    pub fn new(log: TestLog) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            effects: AtomicUsize::new(0),
            fail_after_effect_once: AtomicBool::new(false),
            unsafe_mode: Mutex::new(None),
            cached: Mutex::new(None),
            log,
        }
    }
}

impl CoreQuiescencePortV1 for FakeQuiescence {
    fn quiesce_current_generation(
        &self,
        request: CoreQuiescenceRequestV1,
    ) -> Result<CoreQuiescenceFactsV1, RepairPortErrorV1> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.log
            .lock()
            .map_err(|_| error("log_lock"))?
            .push("core.quiescence".into());
        if let Some(cached) = self
            .cached
            .lock()
            .map_err(|_| error("quiescence_cache_lock"))?
            .clone()
        {
            return Ok(cached);
        }
        let (timed_out, orphan_risk) = self
            .unsafe_mode
            .lock()
            .map_err(|_| error("unsafe_mode_lock"))?
            .unwrap_or((false, false));
        let mut facts = CoreQuiescenceFactsV1 {
            quiescence_id: id("quiescence.one"),
            activation_id: request.activation_id,
            process_generation: request.process_generation,
            worker_trees_stopped: 1,
            host_trees_stopped: 1,
            sidecar_trees_stopped: 1,
            timed_out,
            orphan_risk,
            facts_hash: String::new(),
        };
        facts.facts_hash = core_quiescence_facts_hash_v1(&facts).expect("quiescence hash");
        self.effects.fetch_add(1, Ordering::SeqCst);
        *self
            .cached
            .lock()
            .map_err(|_| error("quiescence_cache_lock"))? = Some(facts.clone());
        if self.fail_after_effect_once.swap(false, Ordering::SeqCst) {
            return Err(error("quiescence_uncertain"));
        }
        Ok(facts)
    }
}

fn event_name(event: &RepairEventV1) -> &'static str {
    match event {
        RepairEventV1::FailureRecorded { .. } => "failure",
        RepairEventV1::InvestigationStarted { .. } => "investigation",
        RepairEventV1::CandidateRegistered { .. } => "candidate",
        RepairEventV1::CapabilityReported { .. } => "capability",
        RepairEventV1::EnrollmentRequested { .. } => "enrollment_requested",
        RepairEventV1::EnrollmentPrepared { .. } => "enrollment_prepared",
        RepairEventV1::CandidateDecided { .. } => "candidate_decided",
        RepairEventV1::ActivationPrepared { .. } => "activation_prepared",
        RepairEventV1::BootstrapAdmissionAccepted { .. } => "admission",
        RepairEventV1::CoreQuiesced { .. } => "quiesced",
        RepairEventV1::FocusedVerificationSubmitted { .. } => "verification",
        RepairEventV1::BootstrapResultReconciled { .. } => "result",
        RepairEventV1::RegressionRecorded { .. } => "regression",
    }
}

fn error(code: &str) -> RepairPortErrorV1 {
    RepairPortErrorV1 {
        code: code.into(),
        message: format!("test port failure: {code}"),
        retryable: false,
    }
}
