//! Authority-first execution pipeline for the production Simple Chat slice.
//!
//! This module deliberately does not depend on desktop settings DTOs. Callers
//! pass an opaque credential reference and provider metadata, then receive a
//! provider-neutral settled result. Plaintext credentials are materialized only
//! inside the authenticated capability-host dispatcher.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use aworkit_capability_host::{
    AdapterRegistry, AdmissionReceipt, AdmittedInvocationDispatcherV1, AnthropicMessagesLimitsV1,
    AnthropicMessagesProvider, AnthropicMessagesProviderConfig, ApprovedInvocationEnvelopeV1,
    CancellationToken, CapabilityDescriptor, CapabilityHost, CapabilityKind, FrozenModelGateway,
    GoogleGeminiLimitsV1, GoogleGeminiProvider, GoogleGeminiProviderConfig, InjectionTargetV1,
    ModelCandidateV1, ModelEventV1, ModelRequestV1, ModelResolutionPlanV1, ModelToolExchangeV1,
    OpenAiCompatibleLimitsV1, OpenAiCompatibleProvider, OpenAiCompatibleProviderConfig,
    ProviderEnginePortV1, ProviderError, RedeemLeaseRequestV1 as HostRedeemLeaseRequestV1,
    SecretDeliveryV1 as HostSecretDeliveryV1, SecretFieldPlanV1, SecretLeaseClientV1,
    SecretLeaseHandleV1, SecretMaterializationError, SecretMaterializationPlanV1,
    SecretMaterializer, SideEffectClass,
};
use aworkit_local_store::{
    CommitBatch, Deduplication, Event, LocalHistoryStore, OutboxEntry, StoreError,
};
use aworkit_protocol::{
    AttestedExtensionSetV1, CapabilityOutcomeClassV1,
    CapabilityOutcomeV1 as WorkerCapabilityOutcomeV1, HistoryBackendV1, ProcessGeneration,
    SchemaVersion, StableId, WorkerBudgetV1, WorkerExecutorKindV1,
    WorkerInvocationProposalV1 as WorkerInvocationProposalContractV1, WorkerNodeV1, WorkerPortV1,
    WorkerTransitionV1, attested_extension_set_hash_v1,
};
use aworkit_trusted_core::{
    ApprovalRequirement, ApprovedDispatchV1, ApprovedHostDispatchPortV1, AuthorityManifest,
    AuthorityManifestV1, BrokerDecisionV1, BrokerError, CapabilityBinding, CapabilityBindingV1,
    CommittedWorkerResultPortV1, CredentialMetadataV1, CredentialRef, DeliveryAcceptanceV1,
    DispatchOutboxV1, DurableInvocationBroker, InvocationLeasePortV1, InvocationLedgerEventV1,
    InvocationLedgerPortV1, NativeCredentialStore, PlatformCredentialStorePort, ProjectCoordinator,
    RedeemLeaseRequestV1 as CoreRedeemLeaseRequestV1, ScopedLeaseRequestV1, SecretBroker,
    SnapshotFreezerV1, SnapshotRequestV1, WorkerInvocationProposalV1 as BrokerInvocationProposalV1,
    WorkerResultOutboxV1, WorkspaceBindingV1, workflow_graph_hash_v1,
};
use aworkit_workflow_worker::{
    agent::{AgentLoopCheckpointV1, AgentLoopConfigV1, AgentLoopV1},
    limits::{BudgetEnvelope, LimitCheckpoint, LimitLedger, Usage},
    plan::ExecutionPlanV1,
    scheduler::{SchedulerCheckpointV1, SchedulerV1, TokenStateV1},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::Zeroizing;

use super::{
    model_tool_loop::{ModelToolLoopErrorV1, ModelToolLoopRequestV1, execute_model_tool_loop_v1},
    project_scope::revalidate_git_branch,
    tool_loop::{
        FileToolAuthorityRuntimeV1, FrozenFileToolAuthorityContextV1, SimpleChatToolActivityV1,
        SimpleChatToolBindingV1, StoredFileToolBindingV1, file_tool_capability_binding,
        file_tool_descriptors, freeze_file_tool_bindings,
    },
};

const MODEL_ADAPTER_VERSION: &str = "1.0.0";
const MODEL_NODE_TYPE: &str = "agent";
const API_KEY_FIELD: &str = "api_key";
const MODEL_SCOPE: &str = "model.invoke";
pub(crate) const SIMPLE_CHAT_MAX_MESSAGE_CONTEXT_BYTES: usize = 256 * 1024;
pub(crate) const SIMPLE_CHAT_MAX_ASSISTANT_TEXT_BYTES: usize = 16 * 1024;
const MAXIMUM_INPUT_BYTES: usize = 384 * 1024;
const MAXIMUM_OUTPUT_BYTES: usize = SIMPLE_CHAT_MAX_ASSISTANT_TEXT_BYTES;
pub(crate) const MAXIMUM_WORKFLOW_SNAPSHOT_BYTES: usize = 128 * 1024;
const MAXIMUM_PREPARED_RECORD_BYTES: usize = 768 * 1024;
const MAXIMUM_PROVIDER_OUTCOME_BYTES: usize = 896 * 1024;
const MAXIMUM_ERROR_BYTES: usize = 16 * 1024;
const APPROVAL_TTL_MILLIS: u64 = 60_000;
const LEASE_TTL: Duration = Duration::from_secs(2 * 60);
const PIPELINE_CHAT_ID: &str = "pipeline.execution";
const BROKER_CHAT_ID: &str = "broker.invocations";
const STORE_BRANCH_ID: &str = "main";
const HOST_DESTINATION: &str = "aworkit.capability-host";
const WORKER_DESTINATION: &str = "aworkit.workflow-worker";
const DEFAULT_FROZEN_CONTEXT_HASH: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ProviderProtocolV1 {
    OpenAiCompatible,
    Anthropic,
    Gemini,
}

impl ProviderProtocolV1 {
    const ALL: [Self; 3] = [Self::OpenAiCompatible, Self::Anthropic, Self::Gemini];

    fn parse(kind: &str) -> Result<Self, SimpleChatPipelineError> {
        match kind {
            "openai_compatible" => Ok(Self::OpenAiCompatible),
            "anthropic" => Ok(Self::Anthropic),
            "gemini" => Ok(Self::Gemini),
            _ => Err(SimpleChatPipelineError::InvalidInput(format!(
                "provider protocol '{kind}' has no installed authority adapter"
            ))),
        }
    }

    const fn kind(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "openai_compatible",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
        }
    }

    const fn capability_id(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "model.openai-compatible",
            Self::Anthropic => "model.anthropic-messages",
            Self::Gemini => "model.google-gemini",
        }
    }

    const fn adapter_id(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "adapter.openai-compatible",
            Self::Anthropic => "adapter.anthropic-messages",
            Self::Gemini => "adapter.google-gemini",
        }
    }

    const fn api_key_header(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "Authorization",
            Self::Anthropic => "x-api-key",
            Self::Gemini => "x-goog-api-key",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SimpleChatMessageV1 {
    pub role: String,
    pub content: String,
}

/// Frozen binding for an installed native provider protocol.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SimpleChatProviderBindingV1 {
    /// Exact protocol kind: `openai_compatible`, `anthropic`, or `gemini`.
    pub kind: String,
    pub base_url: String,
    pub model: String,
    /// Opaque metadata only. The secret value remains in the platform store.
    pub credential: Option<CredentialMetadataV1>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SimpleChatExecutionRequestV1 {
    pub request_id: StableId,
    pub chat_id: StableId,
    pub run_id: StableId,
    pub provider: SimpleChatProviderBindingV1,
    /// Hash of the complete secret-free Chat/Run context frozen at first send.
    /// It binds saved-workflow and resolution provenance into the authority
    /// snapshot without copying editable Settings into the provider payload.
    pub frozen_context_hash: String,
    /// Exact native project workspace frozen by the desktop at first send.
    /// `None` keeps a non-project Chat valid and grants no project tool scope.
    pub workspace: Option<WorkspaceBindingV1>,
    /// Exact Git HEAD label frozen with a Git-worktree project. Local and
    /// unscoped workspaces keep this absent.
    pub project_branch: Option<String>,
    /// Exact enabled read/search bindings frozen from Settings and the Agent
    /// node. A non-project Chat must leave this empty.
    pub tools: Vec<SimpleChatToolBindingV1>,
    /// Maximum provider turns frozen from the Agent node.
    pub maximum_turns: u32,
    /// Exact saved Simple Chat document frozen at the first input.
    pub workflow_snapshot: Value,
    pub messages: Vec<SimpleChatMessageV1>,
    pub now_epoch_millis: u64,
    pub deadline_epoch_millis: u64,
    pub budget: WorkerBudgetV1,
}

impl SimpleChatExecutionRequestV1 {
    #[must_use]
    pub fn bounded(
        request_id: StableId,
        chat_id: StableId,
        run_id: StableId,
        provider: SimpleChatProviderBindingV1,
        messages: Vec<SimpleChatMessageV1>,
        now_epoch_millis: u64,
    ) -> Self {
        Self {
            request_id,
            chat_id,
            run_id,
            provider,
            frozen_context_hash: DEFAULT_FROZEN_CONTEXT_HASH.to_owned(),
            workspace: None,
            project_branch: None,
            tools: Vec::new(),
            maximum_turns: 1,
            workflow_snapshot: default_simple_chat_workflow_snapshot(),
            messages,
            now_epoch_millis,
            deadline_epoch_millis: now_epoch_millis.saturating_add(60_000),
            budget: WorkerBudgetV1 {
                turns: 1,
                attempts: 1,
                tool_calls: 0,
                tokens: 1_000_000,
                cost_micros: 100_000_000,
                actions: 1,
                depth: 0,
                fanout: 1,
                parallel: 1,
                deadline_ms: 60_000,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimpleChatExecutionStatusV1 {
    Succeeded,
    FailedDefinitelyNotStarted,
    FailedKnownStarted,
    OutcomeUncertain,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SimpleChatExecutionResultV1 {
    pub request_id: StableId,
    pub chat_id: StableId,
    pub run_id: StableId,
    pub snapshot_id: StableId,
    pub snapshot_hash: String,
    pub authority_manifest_id: StableId,
    pub worker_invocation_id: StableId,
    pub broker_invocation_id: StableId,
    pub outcome_hash: String,
    pub status: SimpleChatExecutionStatusV1,
    pub assistant_text: Option<String>,
    pub error: Option<String>,
    pub model: String,
    pub input_units: u64,
    pub output_units: u64,
    pub model_turns: u64,
    pub tool_calls: u64,
    pub tool_activity: Vec<SimpleChatToolActivityV1>,
    pub replayed: bool,
}

#[derive(Debug, Error)]
pub enum SimpleChatPipelineError {
    #[error("Simple Chat pipeline input is invalid: {0}")]
    InvalidInput(String),
    #[error("Simple Chat authority freezing failed: {0}")]
    Authority(String),
    #[error("Simple Chat durable execution store failed: {0}")]
    Store(String),
    #[error("Simple Chat invocation broker failed: {0}")]
    Broker(String),
    #[error("Simple Chat invocation requires an approval flow that is not supplied by this API")]
    ApprovalRequired,
    #[error("Simple Chat invocation was denied by its frozen authority")]
    AuthorityDenied,
    #[error("Simple Chat worker contract failed: {0}")]
    Worker(String),
    #[error("Simple Chat host composition failed: {0}")]
    Host(String),
    #[error("Simple Chat durable evidence is incomplete or internally inconsistent")]
    IncompleteEvidence,
}

/// Long-lived service seam. It owns no editable settings representation.
pub struct SimpleChatExecutionPipeline {
    root: PathBuf,
    projects: ProjectCoordinator,
    records: Arc<PipelineRecordStore>,
    ledger: Arc<LocalInvocationLedger>,
    host: Arc<CapabilityHost>,
    descriptors: BTreeMap<ProviderProtocolV1, CapabilityDescriptor>,
    file_tool_descriptors: BTreeMap<String, CapabilityDescriptor>,
    file_tool_authority: FileToolAuthorityRuntimeV1,
    generation: ProcessGeneration,
    core_key: Arc<CoreAuthenticationKey>,
    credential_store: Arc<dyn PlatformCredentialStorePort>,
    provider_factory: Arc<dyn ProviderFactoryV1>,
}

impl SimpleChatExecutionPipeline {
    pub fn open(data_root: impl AsRef<Path>) -> Result<Self, SimpleChatPipelineError> {
        Self::open_with_credential_store(data_root, Arc::new(NativeCredentialStore::new()))
    }

    pub fn open_with_credential_store(
        data_root: impl AsRef<Path>,
        credential_store: Arc<dyn PlatformCredentialStorePort>,
    ) -> Result<Self, SimpleChatPipelineError> {
        Self::compose(
            data_root.as_ref(),
            credential_store,
            Arc::new(BuiltInProviderFactory),
        )
    }

    fn compose(
        data_root: &Path,
        credential_store: Arc<dyn PlatformCredentialStorePort>,
        provider_factory: Arc<dyn ProviderFactoryV1>,
    ) -> Result<Self, SimpleChatPipelineError> {
        fs::create_dir_all(data_root).map_err(store_error)?;
        let root = fs::canonicalize(data_root).map_err(store_error)?;
        let projects = ProjectCoordinator::open(root.join("core").join("simple-chat"))
            .map_err(|error| SimpleChatPipelineError::Authority(error.to_string()))?;
        fs::create_dir_all(root.join("core").join("unscoped-workspace")).map_err(store_error)?;
        let database = root.join("history").join("aworkit-invocations.sqlite3");
        let records = Arc::new(PipelineRecordStore::open(&database)?);
        let ledger = Arc::new(LocalInvocationLedger::open(&database)?);
        let mut descriptors = BTreeMap::new();
        for protocol in ProviderProtocolV1::ALL {
            descriptors.insert(protocol, model_descriptor(protocol)?);
        }
        let file_tool_descriptors = file_tool_descriptors()?;
        let generation = random_generation()?;
        let core_key = Arc::new(CoreAuthenticationKey::random()?);
        let mut registry = AdapterRegistry::default();
        for descriptor in descriptors.values() {
            registry
                .register_capability(descriptor.clone())
                .map_err(|error| SimpleChatPipelineError::Host(error.to_string()))?;
        }
        for descriptor in file_tool_descriptors.values() {
            registry
                .register_capability(descriptor.clone())
                .map_err(|error| SimpleChatPipelineError::Host(error.to_string()))?;
        }
        let mut attested = AttestedExtensionSetV1 {
            host_id: stable("host.simple-chat")?,
            host_generation: generation,
            host_protocol: 1,
            extensions: Vec::new(),
            set_hash: String::new(),
        };
        attested.set_hash = attested_extension_set_hash_v1(&attested)
            .map_err(|error| SimpleChatPipelineError::Host(error.to_string()))?;
        let frozen = registry
            .materialize_attested_set(&attested)
            .map_err(|error| SimpleChatPipelineError::Host(error.to_string()))?;
        let host = Arc::new(
            CapabilityHost::from_attested_registry(frozen, core_key.copy(), 8)
                .map_err(|error| SimpleChatPipelineError::Host(error.to_string()))?,
        );
        let file_tool_authority = FileToolAuthorityRuntimeV1::open(
            &database,
            projects.clone(),
            host.clone(),
            file_tool_descriptors.clone(),
            generation,
            core_key.clone(),
        )?;
        Ok(Self {
            root,
            projects,
            records,
            ledger,
            host,
            descriptors,
            file_tool_descriptors,
            file_tool_authority,
            generation,
            core_key,
            credential_store,
            provider_factory,
        })
    }

    /// Performs every deterministic validation and constructs the exact
    /// bounded frozen execution record without writing records, proposing to
    /// the broker, materializing a secret, or invoking a provider/tool.
    pub fn preflight(
        &self,
        request: &SimpleChatExecutionRequestV1,
    ) -> Result<(), SimpleChatPipelineError> {
        self.validated_prepared(request).map(|_| ())
    }

    fn validated_prepared(
        &self,
        request: &SimpleChatExecutionRequestV1,
    ) -> Result<(PreparedExecutionRecordV1, bool), SimpleChatPipelineError> {
        let protocol = ProviderProtocolV1::parse(&request.provider.kind)?;
        let descriptor = self
            .descriptors
            .get(&protocol)
            .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
        validate_request(request, protocol, descriptor)?;
        if let Some(existing) = self.records.execution(&request.request_id)? {
            self.validate_existing_request_semantics(request, &existing)?;
            if self.existing_request_can_still_start_effect(&existing)? {
                let workspace = existing
                    .workspace
                    .as_ref()
                    .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
                self.projects
                    .revalidate_workspace_v1(workspace)
                    .map_err(|error| SimpleChatPipelineError::Authority(error.to_string()))?;
                revalidate_optional_project_branch(workspace, existing.project_branch.as_deref())?;
            }
            return Ok((existing, true));
        }
        let prepared = self.prepare(request, protocol, descriptor)?;
        if let Some(existing_run) = self
            .records
            .execution_for_chat_or_run(&request.chat_id, &request.run_id)?
            && !prepared.same_frozen_run(&existing_run)
        {
            return Err(SimpleChatPipelineError::Store(
                "Chat/Run identity was reused with changed frozen authority".to_owned(),
            ));
        }
        Ok((prepared, false))
    }

    fn validate_existing_request_semantics(
        &self,
        request: &SimpleChatExecutionRequestV1,
        existing: &PreparedExecutionRecordV1,
    ) -> Result<(), SimpleChatPipelineError> {
        let provider = StoredProviderBindingV1 {
            kind: request.provider.kind.clone(),
            base_url: request.provider.base_url.clone(),
            model: request.provider.model.clone(),
        };
        let secret = request
            .provider
            .credential
            .as_ref()
            .map(StoredSecretBindingV1::from_metadata)
            .transpose()?;
        let tools = freeze_file_tool_bindings(&request.tools)?;
        let stored_messages = existing
            .worker_proposal
            .payload
            .get("context")
            .and_then(|context| context.get("messages"))
            .cloned()
            .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
        let request_messages = serde_json::to_value(&request.messages).map_err(json_error)?;
        let workspace_matches = match &request.workspace {
            Some(workspace) => existing.workspace.as_ref() == Some(workspace),
            None => existing.workspace.as_ref().is_some_and(|workspace| {
                workspace.root == self.root.join("core").join("unscoped-workspace")
            }),
        };
        let workspace_identity_matches = existing.workspace.as_ref().is_some_and(|workspace| {
            serde_json::to_value(&workspace.identity)
                .is_ok_and(|identity| identity == existing.snapshot.workspace_identity)
        });
        let saved_nodes_match = request
            .workflow_snapshot
            .get("nodes")
            .and_then(Value::as_array)
            .is_some_and(|nodes| {
                nodes.len() == existing.snapshot.nodes.len()
                    && nodes.iter().all(|saved| {
                        let node_id = saved.get("id").and_then(Value::as_str);
                        existing.snapshot.nodes.iter().any(|frozen| {
                            Some(frozen.node_id.as_str()) == node_id
                                && frozen.config.get("savedNode") == Some(saved)
                        })
                    })
            });
        let saved_edges_match = request
            .workflow_snapshot
            .get("edges")
            .and_then(Value::as_array)
            .is_some_and(|edges| {
                edges.len() == existing.snapshot.transitions.len()
                    && edges.iter().all(|edge| {
                        existing.snapshot.transitions.iter().any(|transition| {
                            edge.get("id").and_then(Value::as_str)
                                == Some(transition.transition_id.as_str())
                                && edge.get("source").and_then(Value::as_str)
                                    == Some(transition.from_node.as_str())
                                && edge.get("target").and_then(Value::as_str)
                                    == Some(transition.to_node.as_str())
                        })
                    })
            });
        let frozen_context_matches = existing.snapshot.nodes.iter().any(|node| {
            node.node_id.as_str() == "agent.1"
                && node.config.get("frozenContextHash").and_then(Value::as_str)
                    == Some(request.frozen_context_hash.as_str())
        });
        let identity_matches = existing.request_id == request.request_id
            && existing.snapshot.chat_id == request.chat_id
            && existing.snapshot.run_id == request.run_id
            && existing.snapshot.snapshot_id == digest_id("snapshot", request.run_id.as_str())?;
        if !identity_matches
            || existing.provider != provider
            || existing.secret != secret
            || existing.tool_bindings != tools
            || existing.maximum_turns != request.maximum_turns
            || existing.project_branch != request.project_branch
            || existing.snapshot.budget != request.budget
            || stored_messages != request_messages
            || !workspace_matches
            || !workspace_identity_matches
            || !saved_nodes_match
            || !saved_edges_match
            || !frozen_context_matches
        {
            return Err(SimpleChatPipelineError::Store(
                "request ID was reused with changed frozen execution semantics".to_owned(),
            ));
        }
        Ok(())
    }

    fn existing_request_can_still_start_effect(
        &self,
        existing: &PreparedExecutionRecordV1,
    ) -> Result<bool, SimpleChatPipelineError> {
        let Some(invocation_id) = self
            .ledger
            .invocation_for_proposal(&existing.broker_proposal.proposal_id)?
        else {
            return Ok(true);
        };
        let events = self.ledger.events(&invocation_id).map_err(broker_error)?;
        let effect_fenced = events.iter().any(|event| {
            matches!(
                event,
                InvocationLedgerEventV1::DispatchAttempted { .. }
                    | InvocationLedgerEventV1::Settled { .. }
            )
        }) || self.records.outcome(&invocation_id)?.is_some();
        Ok(!effect_fenced)
    }

    /// Executes one exact Simple Chat model turn through every authority and
    /// settlement boundary. Reusing `request_id` with changed semantics fails;
    /// an exact retry returns durable evidence without calling the provider.
    pub fn execute(
        &self,
        request: SimpleChatExecutionRequestV1,
    ) -> Result<SimpleChatExecutionResultV1, SimpleChatPipelineError> {
        let (prepared, record_existing) = self.validated_prepared(&request)?;
        if !record_existing {
            self.records.record_execution(&prepared)?;
        }
        let lease_authority = Arc::new(PipelineLeaseAuthority::new(
            self.generation,
            self.credential_store.clone(),
        ));
        let lease_port = Arc::new(PreparedLeaseIssuer {
            authority: lease_authority.clone(),
            secret: prepared.secret.clone(),
        });
        let broker = DurableInvocationBroker::new(self.ledger.clone(), APPROVAL_TTL_MILLIS)
            .with_lease_port(lease_port);
        let decision = broker
            .propose(
                &prepared.legacy_manifest(),
                prepared.broker_proposal.clone(),
                request.now_epoch_millis,
            )
            .map_err(broker_error)?;
        let broker_invocation_id = match decision {
            BrokerDecisionV1::Denied => return Err(SimpleChatPipelineError::AuthorityDenied),
            BrokerDecisionV1::AwaitingApproval(_) => {
                return Err(SimpleChatPipelineError::ApprovalRequired);
            }
            BrokerDecisionV1::DispatchReady(dispatch) => dispatch.invocation_id,
            BrokerDecisionV1::AlreadySettled(_) => self
                .ledger
                .invocation_for_proposal(&prepared.broker_proposal.proposal_id)?
                .ok_or(SimpleChatPipelineError::IncompleteEvidence)?,
        };

        self.reconcile_persisted_outcomes(&broker)?;
        if self.ledger.settlement(&broker_invocation_id)?.is_none() {
            self.prepare_pending_leases(&broker, &lease_authority)?;
            let host_port = PipelineHostPort {
                host: self.host.clone(),
                projects: self.projects.clone(),
                records: self.records.clone(),
                descriptors: self.descriptors.clone(),
                generation: self.generation,
                core_key: self.core_key.clone(),
                lease_authority,
                provider_factory: self.provider_factory.clone(),
                file_tool_authority: self.file_tool_authority.clone(),
            };
            // The broker commits DispatchAttempted before this call. A transport
            // error or an old attempted dispatch is conservatively settled by
            // the broker and is never automatically replayed.
            let _ = broker.deliver_dispatches(&host_port);
            self.reconcile_persisted_outcomes(&broker)?;
        }

        let (outcome_hash, uncertain) = self
            .ledger
            .settlement(&broker_invocation_id)?
            .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
        let durable_outcome = self
            .records
            .outcome(&broker_invocation_id)?
            .filter(|outcome| outcome_hash_v1(outcome).ok().as_deref() == Some(&outcome_hash));
        let outcome = if let Some(outcome) = durable_outcome {
            outcome
        } else {
            let mut outcome = ProviderOutcomeRecordV1 {
                schema_version: 1,
                invocation_id: broker_invocation_id.clone(),
                status: if uncertain {
                    SimpleChatExecutionStatusV1::OutcomeUncertain
                } else {
                    SimpleChatExecutionStatusV1::FailedDefinitelyNotStarted
                },
                assistant_text: None,
                error: Some(if uncertain {
                    "The provider may have accepted the request, but no conclusive terminal evidence was durably committed. Automatic replay is forbidden."
                        .to_owned()
                } else {
                    "The capability host definitely did not start the provider request.".to_owned()
                }),
                model: prepared.provider.model.clone(),
                input_units: 0,
                output_units: 0,
                attempted_model_turns: if uncertain { 1 } else { 0 },
                settled_tool_calls: 0,
                tool_exchanges: Vec::new(),
                tool_activity: Vec::new(),
                scheduler_checkpoint: None,
                scheduler_trace: Vec::new(),
            };
            finalize_scheduler_evidence(&prepared, &mut outcome)?;
            outcome
        };
        settle_worker_contract(&prepared, &outcome, &outcome_hash)?;
        let _ = broker.deliver_worker_results(&CommittedWorkerAck);
        Ok(SimpleChatExecutionResultV1 {
            request_id: prepared.request_id,
            chat_id: prepared.snapshot.chat_id.clone(),
            run_id: prepared.snapshot.run_id.clone(),
            snapshot_id: prepared.snapshot.snapshot_id.clone(),
            snapshot_hash: prepared.snapshot.snapshot_hash.clone(),
            authority_manifest_id: prepared.manifest.manifest_id.clone(),
            worker_invocation_id: prepared.worker_proposal.invocation_id.clone(),
            broker_invocation_id,
            outcome_hash,
            status: outcome.status,
            assistant_text: outcome.assistant_text,
            error: outcome.error,
            model: outcome.model,
            input_units: outcome.input_units,
            output_units: outcome.output_units,
            model_turns: u64::from(outcome.attempted_model_turns),
            tool_calls: u64::from(outcome.settled_tool_calls),
            tool_activity: outcome.tool_activity,
            replayed: record_existing,
        })
    }

    fn prepare_pending_leases(
        &self,
        broker: &DurableInvocationBroker,
        authority: &PipelineLeaseAuthority,
    ) -> Result<(), SimpleChatPipelineError> {
        for outbox in broker.pending_dispatches().map_err(broker_error)? {
            let record = self
                .records
                .execution_for_dispatch(&outbox.dispatch)?
                .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
            authority.prepare(
                record.secret.as_ref(),
                &outbox.dispatch.invocation_id,
                &record.snapshot.run_id,
                &outbox.dispatch.lease_ids,
            )?;
        }
        Ok(())
    }

    fn prepare(
        &self,
        request: &SimpleChatExecutionRequestV1,
        protocol: ProviderProtocolV1,
        descriptor: &CapabilityDescriptor,
    ) -> Result<PreparedExecutionRecordV1, SimpleChatPipelineError> {
        let scheduler_basis = self.scheduler_continuation_basis(request)?;
        let capability_id = stable(protocol.capability_id())?;
        let secret = request
            .provider
            .credential
            .as_ref()
            .map(StoredSecretBindingV1::from_metadata)
            .transpose()?;
        let provider = StoredProviderBindingV1 {
            kind: protocol.kind().to_owned(),
            base_url: request.provider.base_url.clone(),
            model: request.provider.model.clone(),
        };
        let tool_bindings = freeze_file_tool_bindings(&request.tools)?;
        let CompiledSimpleChatGraphV1 {
            nodes,
            transitions,
            entry_nodes,
            model_node_id,
        } = compile_simple_chat_graph(
            request,
            descriptor,
            &provider,
            secret.as_ref(),
            &tool_bindings,
        )?;
        let workflow_hash =
            workflow_graph_hash_v1(&nodes, &transitions, &entry_nodes, &[], &[], &[])
                .map_err(|error| SimpleChatPipelineError::Authority(error.to_string()))?;
        let workspace = request.workspace.clone().map_or_else(
            || {
                self.projects
                    .resolve_workspace_v1(self.root.join("core").join("unscoped-workspace"))
                    .map_err(|error| SimpleChatPipelineError::Authority(error.to_string()))
            },
            Ok,
        )?;
        revalidate_optional_project_branch(&workspace, request.project_branch.as_deref())?;
        let model_binding = CapabilityBindingV1 {
            capability_id: capability_id.clone(),
            adapter_id: stable(protocol.adapter_id())?,
            adapter_version: descriptor.version.clone(),
            descriptor_hash: descriptor.version_hash.clone(),
            extension: None,
            required_isolation_profile: descriptor.required_isolation.clone(),
            enabled: true,
            compatible: true,
            approval: ApprovalRequirement::Never,
            allowed_node_types: vec![MODEL_NODE_TYPE.to_owned()],
        };
        let mut capability_bindings = vec![model_binding];
        for tool in &tool_bindings {
            let descriptor = self
                .file_tool_descriptors
                .get(&tool.capability_id)
                .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
            capability_bindings.push(file_tool_capability_binding(tool, descriptor)?);
        }
        let (snapshot, manifest) = SnapshotFreezerV1::freeze(
            &self.projects,
            SnapshotRequestV1 {
                snapshot_id: digest_id("snapshot", request.run_id.as_str())?,
                chat_id: request.chat_id.clone(),
                run_id: request.run_id.clone(),
                workflow_hash,
                nodes,
                transitions,
                entry_nodes,
                loop_descriptors: Vec::new(),
                join_descriptors: Vec::new(),
                route_rules: Vec::new(),
                workspace: workspace.clone(),
                capability_bindings,
                budget: request.budget.clone(),
                history_mode: HistoryBackendV1::LocalSqlite,
            },
        )
        .map_err(|error| SimpleChatPipelineError::Authority(error.to_string()))?;

        let (scheduler_checkpoint, scheduler_trace, scheduler_continuation, agent_token_id) =
            prepare_scheduler_for_agent(&snapshot, scheduler_basis.as_ref())?;

        let budget_ref = digest_id("budget", request.request_id.as_str())?;
        let scope_id = format!("run.{}", &digest_hex(request.run_id.as_str())[..24]);
        let mut limits = LimitLedger::new(
            scope_id.clone(),
            BudgetEnvelope {
                turns: request.budget.turns,
                attempts: request.budget.attempts,
                tool_calls: request.budget.tool_calls,
                tokens: request.budget.tokens,
                cost_micros: request.budget.cost_micros,
                actions: request.budget.actions,
                max_depth: request.budget.depth,
                max_fan_out: request.budget.fanout,
                max_parallel: request.budget.parallel,
                deadline_tick: request.budget.deadline_ms,
            },
        )
        .map_err(|error| SimpleChatPipelineError::Worker(error.to_string()))?;
        let mut agent = AgentLoopV1::new(AgentLoopConfigV1 {
            loop_id: digest_id("agent.loop", request.request_id.as_str())?,
            node_id: model_node_id,
            model_capability_ref: capability_id.clone(),
            authority_manifest_ref: manifest.manifest_id.clone(),
            budget_ref,
            scope_id,
            maximum_turns: request.maximum_turns,
            turn_reservation: Usage {
                turns: u64::from(request.maximum_turns),
                attempts: u64::from(request.maximum_turns),
                tool_calls: request.budget.tool_calls,
                tokens: request.budget.tokens,
                cost_micros: request.budget.cost_micros,
                actions: u64::from(request.maximum_turns).saturating_add(request.budget.tool_calls),
            },
            context_pointers: Vec::new(),
            allowed_tool_capability_refs: tool_bindings
                .iter()
                .map(|binding| stable(&binding.capability_id))
                .collect::<Result<Vec<_>, _>>()?,
        })
        .map_err(|error| SimpleChatPipelineError::Worker(error.to_string()))?;
        let context = json!({"messages": request.messages});
        let worker_proposal = agent
            .propose_model_turn(&context, &mut limits)
            .map_err(|error| SimpleChatPipelineError::Worker(error.to_string()))?;
        if worker_proposal.authority_manifest_ref != manifest.manifest_id
            || worker_proposal.capability_ref != capability_id
        {
            return Err(SimpleChatPipelineError::IncompleteEvidence);
        }
        let payload_hash = canonical_hash(&worker_proposal.payload)?;
        let broker_proposal = BrokerInvocationProposalV1 {
            proposal_id: worker_proposal.invocation_id.clone(),
            run_id: request.run_id.clone(),
            node_id: worker_proposal.node_id.clone(),
            attempt: 1,
            capability_id,
            payload_hash,
        };
        let prepared = PreparedExecutionRecordV1 {
            schema_version: 1,
            request_id: request.request_id.clone(),
            snapshot,
            manifest,
            workspace: Some(workspace),
            project_branch: request.project_branch.clone(),
            provider,
            tool_bindings,
            maximum_turns: request.maximum_turns,
            secret,
            worker_proposal,
            broker_proposal,
            agent_checkpoint: agent.checkpoint(),
            limit_checkpoint: limits.checkpoint(),
            scheduler_checkpoint: Some(scheduler_checkpoint),
            scheduler_trace,
            scheduler_continuation,
            agent_token_id: Some(agent_token_id),
            deadline_epoch_millis: request.deadline_epoch_millis,
        };
        enforce_serialized_bound(
            &prepared,
            MAXIMUM_PREPARED_RECORD_BYTES,
            "prepared execution record",
        )?;
        Ok(prepared)
    }

    fn scheduler_continuation_basis(
        &self,
        request: &SimpleChatExecutionRequestV1,
    ) -> Result<Option<SchedulerContinuationBasisV1>, SimpleChatPipelineError> {
        let executions = self.records.executions()?;
        if let Some(existing) = executions
            .iter()
            .find(|record| record.request_id == request.request_id)
            && (existing.snapshot.chat_id != request.chat_id
                || existing.snapshot.run_id != request.run_id)
        {
            return Err(SimpleChatPipelineError::Store(
                "request ID is already bound to a different Chat/Run".to_owned(),
            ));
        }
        if executions.iter().any(|record| {
            (record.snapshot.chat_id == request.chat_id)
                != (record.snapshot.run_id == request.run_id)
        }) {
            return Err(SimpleChatPipelineError::Store(
                "Chat and Run identities no longer refer to the same frozen session".to_owned(),
            ));
        }
        let session = executions
            .iter()
            .filter(|record| {
                record.snapshot.chat_id == request.chat_id
                    && record.snapshot.run_id == request.run_id
            })
            .collect::<Vec<_>>();
        let position = session
            .iter()
            .position(|record| record.request_id == request.request_id)
            .unwrap_or(session.len());
        for (ordinal, record) in session.iter().take(position).enumerate() {
            let ordinal =
                u64::try_from(ordinal).map_err(|_| SimpleChatPipelineError::IncompleteEvidence)?;
            if record.scheduler_continuation != ordinal {
                return Err(SimpleChatPipelineError::Store(
                    "this legacy Run contains independently seeded Scheduler records and cannot be continued safely; start a New Chat"
                        .to_owned(),
                ));
            }
        }
        let Some(previous) = position.checked_sub(1).and_then(|index| session.get(index)) else {
            return Ok(None);
        };
        let next_continuation = previous
            .scheduler_continuation
            .checked_add(1)
            .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
        let invocation_id = self
            .ledger
            .invocation_for_proposal(&previous.broker_proposal.proposal_id)?
            .ok_or_else(|| {
                SimpleChatPipelineError::Store(
                    "the previous Input has no durable invocation identity; recover it before continuing"
                        .to_owned(),
                )
            })?;
        let (settled_hash, uncertain) =
            self.ledger.settlement(&invocation_id)?.ok_or_else(|| {
                SimpleChatPipelineError::Store(
                    "the previous Input is not durably settled; recover it before continuing"
                        .to_owned(),
                )
            })?;
        let outcome = self.records.outcome(&invocation_id)?.ok_or_else(|| {
            SimpleChatPipelineError::Store(
                "the previous Input has no terminal Scheduler evidence; recover it before continuing"
                    .to_owned(),
            )
        })?;
        if uncertain
            || outcome.status != SimpleChatExecutionStatusV1::Succeeded
            || outcome_hash_v1(&outcome)? != settled_hash
        {
            return Err(SimpleChatPipelineError::Store(
                "the previous Input did not reach a conclusive suspended Wait; start a New Chat"
                    .to_owned(),
            ));
        }
        validate_final_scheduler_evidence(previous, &outcome)?;
        validate_follow_up_context(previous, &outcome, request)?;
        Ok(Some(SchedulerContinuationBasisV1 {
            checkpoint: outcome
                .scheduler_checkpoint
                .ok_or(SimpleChatPipelineError::IncompleteEvidence)?,
            trace: outcome.scheduler_trace,
            continuation: next_continuation,
        }))
    }

    fn reconcile_persisted_outcomes(
        &self,
        broker: &DurableInvocationBroker,
    ) -> Result<(), SimpleChatPipelineError> {
        for outcome in self.records.outcomes()? {
            let events = self
                .ledger
                .events(&outcome.invocation_id)
                .map_err(broker_error)?;
            if events.is_empty()
                || events
                    .iter()
                    .any(|event| matches!(event, InvocationLedgerEventV1::Settled { .. }))
            {
                continue;
            }
            let attempted = events
                .iter()
                .any(|event| matches!(event, InvocationLedgerEventV1::DispatchAttempted { .. }));
            let accepted = events
                .iter()
                .any(|event| matches!(event, InvocationLedgerEventV1::DispatchAccepted { .. }));
            if !accepted {
                if !attempted {
                    continue;
                }
                broker
                    .accept_dispatch(&outcome.invocation_id)
                    .map_err(broker_error)?;
            }
            if let Some(outbox) = self
                .ledger
                .pending_dispatches()
                .map_err(broker_error)?
                .into_iter()
                .find(|entry| entry.dispatch.invocation_id == outcome.invocation_id)
            {
                broker
                    .mark_dispatch_delivered(&outbox.outbox_id)
                    .map_err(broker_error)?;
            }
            broker
                .settle(
                    &outcome.invocation_id,
                    outcome_hash_v1(&outcome)?,
                    outcome.status == SimpleChatExecutionStatusV1::OutcomeUncertain,
                )
                .map_err(broker_error)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredProviderBindingV1 {
    kind: String,
    base_url: String,
    model: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredSecretBindingV1 {
    opaque_ref: StableId,
    field_names: BTreeSet<String>,
    revision: u64,
}

impl StoredSecretBindingV1 {
    fn from_metadata(metadata: &CredentialMetadataV1) -> Result<Self, SimpleChatPipelineError> {
        if metadata.revision == 0 || !metadata.field_names.contains(API_KEY_FIELD) {
            return Err(SimpleChatPipelineError::InvalidInput(
                "credential metadata must expose a non-zero revision and the api_key field"
                    .to_owned(),
            ));
        }
        Ok(Self {
            opaque_ref: metadata.credential.0.clone(),
            field_names: metadata.field_names.clone(),
            revision: metadata.revision,
        })
    }

    fn metadata(&self) -> CredentialMetadataV1 {
        CredentialMetadataV1 {
            credential: CredentialRef(self.opaque_ref.clone()),
            field_names: self.field_names.clone(),
            revision: self.revision,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreparedExecutionRecordV1 {
    schema_version: u16,
    request_id: StableId,
    snapshot: aworkit_protocol::WorkerFrozenRunSnapshotV1,
    manifest: AuthorityManifestV1,
    #[serde(default)]
    workspace: Option<WorkspaceBindingV1>,
    #[serde(default)]
    project_branch: Option<String>,
    provider: StoredProviderBindingV1,
    #[serde(default)]
    tool_bindings: Vec<StoredFileToolBindingV1>,
    #[serde(default = "default_maximum_turns")]
    maximum_turns: u32,
    #[serde(rename = "opaqueBinding")]
    secret: Option<StoredSecretBindingV1>,
    worker_proposal: WorkerInvocationProposalContractV1,
    broker_proposal: BrokerInvocationProposalV1,
    agent_checkpoint: AgentLoopCheckpointV1,
    limit_checkpoint: LimitCheckpoint,
    #[serde(default)]
    scheduler_checkpoint: Option<SchedulerCheckpointV1>,
    #[serde(default)]
    scheduler_trace: Vec<SchedulerTraceEntryV1>,
    /// Zero-based number of the accepted Input in this frozen Run. Version 1
    /// records written before persistent Wait continuation omitted this field
    /// and therefore safely decode as the initial Input only.
    #[serde(default)]
    scheduler_continuation: u64,
    #[serde(default)]
    agent_token_id: Option<StableId>,
    deadline_epoch_millis: u64,
}

impl PreparedExecutionRecordV1 {
    fn legacy_manifest(&self) -> AuthorityManifest {
        AuthorityManifest {
            manifest_id: self.manifest.manifest_id.clone(),
            capability_bindings: self
                .manifest
                .capability_bindings
                .iter()
                .map(|binding| CapabilityBinding {
                    capability_id: binding.capability_id.clone(),
                    adapter_version: binding.adapter_version.clone(),
                    enabled: binding.enabled,
                    compatible: binding.compatible,
                    approval: binding.approval,
                })
                .collect(),
            summary: self.manifest.summary.clone(),
        }
    }

    fn same_frozen_run(&self, other: &Self) -> bool {
        self.snapshot == other.snapshot
            && self.manifest == other.manifest
            && self.workspace == other.workspace
            && self.project_branch == other.project_branch
            && self.provider == other.provider
            && self.tool_bindings == other.tool_bindings
            && self.maximum_turns == other.maximum_turns
            && self.secret == other.secret
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderOutcomeRecordV1 {
    schema_version: u16,
    invocation_id: StableId,
    status: SimpleChatExecutionStatusV1,
    assistant_text: Option<String>,
    error: Option<String>,
    model: String,
    input_units: u64,
    output_units: u64,
    #[serde(default)]
    attempted_model_turns: u32,
    #[serde(default)]
    settled_tool_calls: u32,
    #[serde(default)]
    tool_exchanges: Vec<ModelToolExchangeV1>,
    #[serde(default)]
    tool_activity: Vec<SimpleChatToolActivityV1>,
    #[serde(default)]
    scheduler_checkpoint: Option<SchedulerCheckpointV1>,
    #[serde(default)]
    scheduler_trace: Vec<SchedulerTraceEntryV1>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SchedulerTraceEntryV1 {
    sequence: u64,
    action: String,
    node_id: StableId,
    token_id: StableId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transition_id: Option<StableId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_node_id: Option<StableId>,
}

#[derive(Clone, Debug)]
struct SchedulerContinuationBasisV1 {
    checkpoint: SchedulerCheckpointV1,
    trace: Vec<SchedulerTraceEntryV1>,
    /// Zero-based number to assign to the Input that is about to be accepted.
    continuation: u64,
}

pub(super) struct CoreAuthenticationKey(Zeroizing<Vec<u8>>);

impl CoreAuthenticationKey {
    pub(super) fn random() -> Result<Self, SimpleChatPipelineError> {
        let mut bytes = vec![0_u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|_| SimpleChatPipelineError::Host("random key generation failed".into()))?;
        Ok(Self(Zeroizing::new(bytes)))
    }

    pub(super) fn copy(&self) -> Vec<u8> {
        self.0.to_vec()
    }

    pub(super) fn as_slice(&self) -> &[u8] {
        self.0.as_slice()
    }
}

trait ProviderFactoryV1: Send + Sync {
    fn create(
        &self,
        descriptor: &CapabilityDescriptor,
        provider: &StoredProviderBindingV1,
        api_key: Option<Zeroizing<String>>,
    ) -> Result<Box<dyn ProviderEnginePortV1>, String>;
}

struct BuiltInProviderFactory;

impl ProviderFactoryV1 for BuiltInProviderFactory {
    fn create(
        &self,
        descriptor: &CapabilityDescriptor,
        provider: &StoredProviderBindingV1,
        api_key: Option<Zeroizing<String>>,
    ) -> Result<Box<dyn ProviderEnginePortV1>, String> {
        let protocol =
            ProviderProtocolV1::parse(&provider.kind).map_err(|error| error.to_string())?;
        if descriptor.capability_id != protocol.capability_id() {
            return Err("frozen provider protocol does not match its capability descriptor".into());
        }
        let api_key = api_key.map(|mut value| std::mem::take(&mut *value));
        match protocol {
            ProviderProtocolV1::OpenAiCompatible => {
                let config = OpenAiCompatibleProviderConfig::new(
                    protocol.capability_id(),
                    descriptor.version_hash.clone(),
                    &provider.base_url,
                    provider.model.clone(),
                    api_key,
                    OpenAiCompatibleLimitsV1::default(),
                )
                .map_err(|error| error.to_string())?;
                OpenAiCompatibleProvider::new(config)
                    .map(|provider| Box::new(provider) as Box<dyn ProviderEnginePortV1>)
                    .map_err(|error| error.to_string())
            }
            ProviderProtocolV1::Anthropic => {
                let config = AnthropicMessagesProviderConfig::new(
                    protocol.capability_id(),
                    descriptor.version_hash.clone(),
                    &provider.base_url,
                    provider.model.clone(),
                    api_key,
                    AnthropicMessagesLimitsV1::default(),
                )
                .map_err(|error| error.to_string())?;
                AnthropicMessagesProvider::new(config)
                    .map(|provider| Box::new(provider) as Box<dyn ProviderEnginePortV1>)
                    .map_err(|error| error.to_string())
            }
            ProviderProtocolV1::Gemini => {
                let config = GoogleGeminiProviderConfig::new(
                    protocol.capability_id(),
                    descriptor.version_hash.clone(),
                    &provider.base_url,
                    provider.model.clone(),
                    api_key,
                    GoogleGeminiLimitsV1::default(),
                )
                .map_err(|error| error.to_string())?;
                GoogleGeminiProvider::new(config)
                    .map(|provider| Box::new(provider) as Box<dyn ProviderEnginePortV1>)
                    .map_err(|error| error.to_string())
            }
        }
    }
}

/// Core-side owner of process-local, invocation-scoped credential leases.
///
/// The capability host receives only opaque lease handles and an authenticated
/// redemption client. It never receives the platform credential store or the
/// authority needed to mint a replacement lease.
struct PipelineLeaseAuthority {
    generation: ProcessGeneration,
    store: Arc<dyn PlatformCredentialStorePort>,
    brokers: Mutex<BTreeMap<String, SecretBroker>>,
}

impl PipelineLeaseAuthority {
    fn new(generation: ProcessGeneration, store: Arc<dyn PlatformCredentialStorePort>) -> Self {
        Self {
            generation,
            store,
            brokers: Mutex::new(BTreeMap::new()),
        }
    }

    fn prepare(
        &self,
        secret: Option<&StoredSecretBindingV1>,
        invocation_id: &StableId,
        run_id: &StableId,
        lease_ids: &[StableId],
    ) -> Result<(), SimpleChatPipelineError> {
        let Some(secret) = secret else {
            return if lease_ids.is_empty() {
                Ok(())
            } else {
                Err(SimpleChatPipelineError::IncompleteEvidence)
            };
        };
        if lease_ids.len() != 1 {
            return Err(SimpleChatPipelineError::IncompleteEvidence);
        }
        let expected = lease_id(invocation_id, secret)?;
        if lease_ids[0] != expected {
            return Err(SimpleChatPipelineError::IncompleteEvidence);
        }

        let mut broker = SecretBroker::with_store(self.store.clone());
        broker
            .restore_credential_metadata(secret.metadata())
            .map_err(|error| SimpleChatPipelineError::Host(error.to_string()))?;
        broker
            .issue_scoped(ScopedLeaseRequestV1 {
                lease_id: expected.clone(),
                credential: CredentialRef(secret.opaque_ref.clone()),
                decision_id: invocation_id.clone(),
                invocation_id: invocation_id.clone(),
                run_id: run_id.clone(),
                audience_generation: self.generation,
                permitted_fields: BTreeSet::from([API_KEY_FIELD.to_owned()]),
                ttl: LEASE_TTL,
                maximum_uses: 1,
            })
            .map_err(|error| SimpleChatPipelineError::Host(error.to_string()))?;
        self.brokers
            .lock()
            .map_err(|_| SimpleChatPipelineError::Host("credential lease lock poisoned".into()))?
            .insert(expected.as_str().to_owned(), broker);
        Ok(())
    }

    fn redeem(
        &self,
        request: &HostRedeemLeaseRequestV1,
    ) -> Result<HostSecretDeliveryV1, SecretMaterializationError> {
        let mut brokers = self
            .brokers
            .lock()
            .map_err(|_| SecretMaterializationError::ChannelUnavailable)?;
        let broker = brokers
            .get_mut(request.lease_id.as_str())
            .ok_or(SecretMaterializationError::LeaseDenied)?;
        let delivery = broker
            .redeem_scoped(&CoreRedeemLeaseRequestV1 {
                lease_id: request.lease_id.clone(),
                decision_id: request.decision_id.clone(),
                invocation_id: request.invocation_id.clone(),
                audience_generation: request.host_generation,
                requested_fields: request.requested_fields.clone(),
            })
            .map_err(|_| SecretMaterializationError::LeaseDenied)?;
        Ok(HostSecretDeliveryV1 {
            fields: delivery.into_fields(),
        })
    }

    fn revoke(&self, lease_id: &StableId) -> Result<(), SecretMaterializationError> {
        let mut brokers = self
            .brokers
            .lock()
            .map_err(|_| SecretMaterializationError::ChannelUnavailable)?;
        if let Some(mut broker) = brokers.remove(lease_id.as_str()) {
            broker.revoke(lease_id);
        }
        Ok(())
    }
}

struct PreparedLeaseIssuer {
    authority: Arc<PipelineLeaseAuthority>,
    secret: Option<StoredSecretBindingV1>,
}

impl InvocationLeasePortV1 for PreparedLeaseIssuer {
    fn issue_for_dispatch(
        &self,
        proposal: &BrokerInvocationProposalV1,
        _manifest: &AuthorityManifest,
        invocation_id: &StableId,
    ) -> Result<Vec<StableId>, BrokerError> {
        let Some(secret) = &self.secret else {
            return Ok(Vec::new());
        };
        let lease_id = lease_id(invocation_id, secret).map_err(|_| BrokerError::Unavailable)?;
        self.authority
            .prepare(
                Some(secret),
                invocation_id,
                &proposal.run_id,
                std::slice::from_ref(&lease_id),
            )
            .map_err(|_| BrokerError::Unavailable)?;
        Ok(vec![lease_id])
    }

    fn revoke_uncommitted(&self, lease_ids: &[StableId]) -> Result<(), BrokerError> {
        for lease_id in lease_ids {
            self.authority
                .revoke(lease_id)
                .map_err(|_| BrokerError::Unavailable)?;
        }
        Ok(())
    }
}

struct PipelineHostPort {
    host: Arc<CapabilityHost>,
    projects: ProjectCoordinator,
    records: Arc<PipelineRecordStore>,
    descriptors: BTreeMap<ProviderProtocolV1, CapabilityDescriptor>,
    generation: ProcessGeneration,
    core_key: Arc<CoreAuthenticationKey>,
    lease_authority: Arc<PipelineLeaseAuthority>,
    provider_factory: Arc<dyn ProviderFactoryV1>,
    file_tool_authority: FileToolAuthorityRuntimeV1,
}

impl ApprovedHostDispatchPortV1 for PipelineHostPort {
    fn dispatch(&self, dispatch: &ApprovedDispatchV1) -> Result<DeliveryAcceptanceV1, BrokerError> {
        let record = match self.records.execution_for_dispatch(dispatch) {
            Ok(Some(record)) => record,
            Ok(None) => return Ok(DeliveryAcceptanceV1::RejectedDefinitelyNotStarted),
            Err(_) => return Ok(DeliveryAcceptanceV1::Ambiguous),
        };
        if record.broker_proposal.payload_hash != dispatch.payload_hash
            || record.manifest.manifest_id != dispatch.manifest_id
            || record.broker_proposal.capability_id != dispatch.capability_id
            || canonical_hash(&record.worker_proposal.payload)
                .ok()
                .as_deref()
                != Some(dispatch.payload_hash.as_str())
        {
            return Err(BrokerError::IdentityConflict);
        }
        let Some(workspace) = record.workspace.as_ref() else {
            return Ok(DeliveryAcceptanceV1::RejectedDefinitelyNotStarted);
        };
        if self.projects.revalidate_workspace_v1(workspace).is_err()
            || revalidate_optional_project_branch(workspace, record.project_branch.as_deref())
                .is_err()
        {
            return Ok(DeliveryAcceptanceV1::RejectedDefinitelyNotStarted);
        }
        let binding = match record
            .manifest
            .capability_bindings
            .iter()
            .find(|binding| binding.capability_id == dispatch.capability_id)
        {
            Some(binding) => binding,
            None => return Ok(DeliveryAcceptanceV1::RejectedDefinitelyNotStarted),
        };
        let protocol = ProviderProtocolV1::parse(&record.provider.kind)
            .map_err(|_| BrokerError::IdentityConflict)?;
        let descriptor = self
            .descriptors
            .get(&protocol)
            .ok_or(BrokerError::IdentityConflict)?;
        if dispatch.capability_id.as_str() != protocol.capability_id()
            || binding.adapter_id.as_str() != protocol.adapter_id()
            || binding.adapter_version != descriptor.version
            || binding.descriptor_hash != descriptor.version_hash
            || binding.required_isolation_profile != descriptor.required_isolation
        {
            return Err(BrokerError::IdentityConflict);
        }

        match &record.secret {
            None if dispatch.lease_ids.is_empty() => {}
            Some(secret) if dispatch.lease_ids.len() == 1 => {
                let expected = lease_id(&dispatch.invocation_id, secret)
                    .map_err(|_| BrokerError::Unavailable)?;
                if dispatch.lease_ids[0] != expected {
                    return Err(BrokerError::IdentityConflict);
                }
            }
            _ => return Err(BrokerError::IdentityConflict),
        }
        let mut envelope = ApprovedInvocationEnvelopeV1 {
            schema_version: SchemaVersion::V1,
            invocation_id: dispatch.invocation_id.clone(),
            decision_id: dispatch.invocation_id.clone(),
            host_generation: self.generation,
            capability_id: dispatch.capability_id.to_string(),
            adapter_version: binding.adapter_version.clone(),
            binding_hash: binding.descriptor_hash.clone(),
            extension: binding.extension.clone(),
            required_isolation_profile: binding.required_isolation_profile.clone(),
            kind: CapabilityKind::Model,
            enforced_scopes: vec![MODEL_SCOPE.to_owned()],
            deadline_epoch_millis: record.deadline_epoch_millis,
            cancellation_token: digest_id("cancel", dispatch.invocation_id.as_str())
                .map_err(|_| BrokerError::Unavailable)?,
            lease_handles: dispatch.lease_ids.clone(),
            max_output_bytes: MAXIMUM_OUTPUT_BYTES,
            payload: record.worker_proposal.payload.clone(),
            core_authentication_tag: String::new(),
        };
        envelope
            .sign(self.core_key.as_slice())
            .map_err(|_| BrokerError::Unavailable)?;
        let dispatcher = ModelInvocationDispatcher {
            projects: self.projects.clone(),
            records: self.records.clone(),
            descriptor: descriptor.clone(),
            provider: record.provider.clone(),
            prepared: record,
            secret_client: CoreSecretLeaseClient {
                authority: self.lease_authority.clone(),
            },
            provider_factory: self.provider_factory.clone(),
            file_tool_authority: self.file_tool_authority.clone(),
        };
        match self
            .host
            .dispatch_v1(&envelope, current_epoch_millis(), &dispatcher)
        {
            Ok(receipt) if receipt.output.as_ref().is_some_and(Result::is_ok) => {
                Ok(if receipt.admission.duplicate {
                    DeliveryAcceptanceV1::AlreadyAccepted
                } else {
                    DeliveryAcceptanceV1::Accepted
                })
            }
            Ok(receipt) if receipt.output.is_none() => {
                if self
                    .records
                    .outcome(&dispatch.invocation_id)
                    .ok()
                    .flatten()
                    .is_some()
                {
                    Ok(DeliveryAcceptanceV1::AlreadyAccepted)
                } else {
                    Ok(DeliveryAcceptanceV1::Ambiguous)
                }
            }
            Ok(_) => Ok(DeliveryAcceptanceV1::Ambiguous),
            Err(_) => Ok(DeliveryAcceptanceV1::RejectedDefinitelyNotStarted),
        }
    }
}

struct ModelInvocationDispatcher {
    projects: ProjectCoordinator,
    records: Arc<PipelineRecordStore>,
    descriptor: CapabilityDescriptor,
    provider: StoredProviderBindingV1,
    prepared: PreparedExecutionRecordV1,
    secret_client: CoreSecretLeaseClient,
    provider_factory: Arc<dyn ProviderFactoryV1>,
    file_tool_authority: FileToolAuthorityRuntimeV1,
}

impl AdmittedInvocationDispatcherV1 for ModelInvocationDispatcher {
    type Output = Result<ProviderOutcomeRecordV1, SimpleChatPipelineError>;

    fn dispatch(
        &self,
        envelope: &ApprovedInvocationEnvelopeV1,
        _admission: &AdmissionReceipt,
        cancellation: &CancellationToken,
    ) -> Self::Output {
        let workspace_valid = self.prepared.workspace.as_ref().is_some_and(|workspace| {
            self.projects.revalidate_workspace_v1(workspace).is_ok()
                && revalidate_optional_project_branch(
                    workspace,
                    self.prepared.project_branch.as_deref(),
                )
                .is_ok()
        });
        if !workspace_valid {
            return self.persist(ProviderOutcomeRecordV1 {
                schema_version: 1,
                invocation_id: envelope.invocation_id.clone(),
                status: SimpleChatExecutionStatusV1::FailedDefinitelyNotStarted,
                assistant_text: None,
                error: Some(
                    "frozen project workspace or Git branch drifted before provider dispatch"
                        .into(),
                ),
                model: self.provider.model.clone(),
                input_units: 0,
                output_units: 0,
                attempted_model_turns: 0,
                settled_tool_calls: 0,
                tool_exchanges: Vec::new(),
                tool_activity: Vec::new(),
                scheduler_checkpoint: None,
                scheduler_trace: Vec::new(),
            });
        }
        let protocol = match ProviderProtocolV1::parse(&self.provider.kind) {
            Ok(protocol) => protocol,
            Err(error) => {
                return self.persist(ProviderOutcomeRecordV1 {
                    schema_version: 1,
                    invocation_id: envelope.invocation_id.clone(),
                    status: SimpleChatExecutionStatusV1::FailedDefinitelyNotStarted,
                    assistant_text: None,
                    error: Some(error.to_string()),
                    model: self.provider.model.clone(),
                    input_units: 0,
                    output_units: 0,
                    attempted_model_turns: 0,
                    settled_tool_calls: 0,
                    tool_exchanges: Vec::new(),
                    tool_activity: Vec::new(),
                    scheduler_checkpoint: None,
                    scheduler_trace: Vec::new(),
                });
            }
        };
        let materialized = if let Some(lease_id) = envelope.lease_handles.first() {
            let materializer = SecretMaterializer::new(self.secret_client.clone());
            match materializer.materialize(&SecretMaterializationPlanV1 {
                decision_id: envelope.decision_id.clone(),
                invocation_id: envelope.invocation_id.clone(),
                host_generation: envelope.host_generation,
                lease: SecretLeaseHandleV1 {
                    lease_id: lease_id.clone(),
                },
                fields: vec![SecretFieldPlanV1 {
                    field: API_KEY_FIELD.to_owned(),
                    target: InjectionTargetV1::Header(protocol.api_key_header().to_owned()),
                }],
            }) {
                Ok(materialized) => Some(materialized),
                Err(error) => {
                    return self.persist(ProviderOutcomeRecordV1 {
                        schema_version: 1,
                        invocation_id: envelope.invocation_id.clone(),
                        status: SimpleChatExecutionStatusV1::FailedDefinitelyNotStarted,
                        assistant_text: None,
                        error: Some(format!("credential lease materialization failed: {error}")),
                        model: self.provider.model.clone(),
                        input_units: 0,
                        output_units: 0,
                        attempted_model_turns: 0,
                        settled_tool_calls: 0,
                        tool_exchanges: Vec::new(),
                        tool_activity: Vec::new(),
                        scheduler_checkpoint: None,
                        scheduler_trace: Vec::new(),
                    });
                }
            }
        } else {
            None
        };
        let api_key_bytes = materialized
            .as_ref()
            .and_then(|secret| secret.value(API_KEY_FIELD));
        let api_key = match api_key_bytes {
            Some(bytes) => match String::from_utf8(bytes.to_vec()) {
                Ok(value) => Some(Zeroizing::new(value)),
                Err(_) => {
                    return self.persist(ProviderOutcomeRecordV1 {
                        schema_version: 1,
                        invocation_id: envelope.invocation_id.clone(),
                        status: SimpleChatExecutionStatusV1::FailedDefinitelyNotStarted,
                        assistant_text: None,
                        error: Some("credential API-key field is not valid UTF-8".to_owned()),
                        model: self.provider.model.clone(),
                        input_units: 0,
                        output_units: 0,
                        attempted_model_turns: 0,
                        settled_tool_calls: 0,
                        tool_exchanges: Vec::new(),
                        tool_activity: Vec::new(),
                        scheduler_checkpoint: None,
                        scheduler_trace: Vec::new(),
                    });
                }
            },
            None => None,
        };
        let provider = match self
            .provider_factory
            .create(&self.descriptor, &self.provider, api_key)
        {
            Ok(provider) => provider,
            Err(error) => {
                return self.persist(ProviderOutcomeRecordV1 {
                    schema_version: 1,
                    invocation_id: envelope.invocation_id.clone(),
                    status: SimpleChatExecutionStatusV1::FailedDefinitelyNotStarted,
                    assistant_text: None,
                    error: Some(redact_error(&materialized, &error)),
                    model: self.provider.model.clone(),
                    input_units: 0,
                    output_units: 0,
                    attempted_model_turns: 0,
                    settled_tool_calls: 0,
                    tool_exchanges: Vec::new(),
                    tool_activity: Vec::new(),
                    scheduler_checkpoint: None,
                    scheduler_trace: Vec::new(),
                });
            }
        };
        let Some(context) = envelope.payload.get("context") else {
            return self.persist(ProviderOutcomeRecordV1 {
                schema_version: 1,
                invocation_id: envelope.invocation_id.clone(),
                status: SimpleChatExecutionStatusV1::FailedDefinitelyNotStarted,
                assistant_text: None,
                error: Some("worker proposal did not contain a model context".to_owned()),
                model: self.provider.model.clone(),
                input_units: 0,
                output_units: 0,
                attempted_model_turns: 0,
                settled_tool_calls: 0,
                tool_exchanges: Vec::new(),
                tool_activity: Vec::new(),
                scheduler_checkpoint: None,
                scheduler_trace: Vec::new(),
            });
        };
        let gateway = FrozenModelGateway::new(vec![provider]);
        let outcome = if self.prepared.tool_bindings.is_empty() {
            text_only_outcome(
                &gateway,
                &self.descriptor,
                context,
                envelope,
                cancellation,
                &self.provider.model,
                &materialized,
            )
        } else {
            let workspace = self
                .prepared
                .workspace
                .clone()
                .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
            let authority = self
                .file_tool_authority
                .bind(FrozenFileToolAuthorityContextV1 {
                    manifest: self.prepared.manifest.clone(),
                    run_id: self.prepared.snapshot.run_id.clone(),
                    node_id: self.prepared.worker_proposal.node_id.clone(),
                    workspace,
                    project_branch: self.prepared.project_branch.clone(),
                    bindings: self.prepared.tool_bindings.clone(),
                    deadline_epoch_millis: self.prepared.deadline_epoch_millis,
                });
            match execute_model_tool_loop_v1(
                &gateway,
                ModelToolLoopRequestV1 {
                    outer_invocation_id: &envelope.invocation_id,
                    input: context.clone(),
                    definitions: self
                        .prepared
                        .tool_bindings
                        .iter()
                        .map(StoredFileToolBindingV1::definition)
                        .collect(),
                    binding_id: self.descriptor.capability_id.clone(),
                    binding_version_hash: self.descriptor.version_hash.clone(),
                    maximum_input_bytes: MAXIMUM_INPUT_BYTES,
                    maximum_output_bytes: MAXIMUM_OUTPUT_BYTES,
                    maximum_turns: self.prepared.maximum_turns,
                    maximum_tool_calls: u32::try_from(self.prepared.snapshot.budget.tool_calls)
                        .unwrap_or(u32::MAX),
                    maximum_tokens: self.prepared.snapshot.budget.tokens,
                },
                &authority,
                cancellation,
            ) {
                Ok(completed) => ProviderOutcomeRecordV1 {
                    schema_version: 1,
                    invocation_id: envelope.invocation_id.clone(),
                    status: SimpleChatExecutionStatusV1::Succeeded,
                    assistant_text: Some(completed.assistant_text),
                    error: None,
                    model: self.provider.model.clone(),
                    input_units: completed.input_tokens,
                    output_units: completed.output_tokens,
                    attempted_model_turns: completed.attempted_model_turns,
                    settled_tool_calls: completed.settled_tool_calls,
                    tool_exchanges: completed.exchanges,
                    tool_activity: completed.activities,
                    scheduler_checkpoint: None,
                    scheduler_trace: Vec::new(),
                },
                Err(failure) => {
                    let status = match &failure.error {
                        ModelToolLoopErrorV1::Provider(error) => provider_error_status(error),
                        ModelToolLoopErrorV1::ToolAuthority(_)
                        | ModelToolLoopErrorV1::Budget(_)
                        | ModelToolLoopErrorV1::MissingAssistantOutput => {
                            SimpleChatExecutionStatusV1::FailedKnownStarted
                        }
                    };
                    ProviderOutcomeRecordV1 {
                        schema_version: 1,
                        invocation_id: envelope.invocation_id.clone(),
                        status,
                        assistant_text: None,
                        error: Some(redact_error(&materialized, &failure.error.to_string())),
                        model: self.provider.model.clone(),
                        input_units: failure.input_tokens,
                        output_units: failure.output_tokens,
                        attempted_model_turns: failure.attempted_model_turns,
                        settled_tool_calls: failure.settled_tool_calls,
                        tool_exchanges: failure.exchanges,
                        tool_activity: failure.activities,
                        scheduler_checkpoint: None,
                        scheduler_trace: Vec::new(),
                    }
                }
            }
        };
        self.persist(outcome)
    }
}

impl ModelInvocationDispatcher {
    fn persist(
        &self,
        mut outcome: ProviderOutcomeRecordV1,
    ) -> Result<ProviderOutcomeRecordV1, SimpleChatPipelineError> {
        validate_provider_outcome_accounting(&self.prepared, &outcome)?;
        compact_provider_outcome_if_needed(&mut outcome)?;
        finalize_scheduler_evidence(&self.prepared, &mut outcome)?;
        enforce_serialized_bound(
            &outcome,
            MAXIMUM_PROVIDER_OUTCOME_BYTES,
            "provider outcome record",
        )?;
        self.records.record_outcome(&outcome)?;
        Ok(outcome)
    }
}

fn text_only_outcome(
    gateway: &FrozenModelGateway,
    descriptor: &CapabilityDescriptor,
    context: &Value,
    envelope: &ApprovedInvocationEnvelopeV1,
    cancellation: &CancellationToken,
    model: &str,
    materialized: &Option<aworkit_capability_host::SecretMaterializationV1>,
) -> ProviderOutcomeRecordV1 {
    let execution = gateway.execute_cancellable(
        &ModelResolutionPlanV1 {
            candidates: vec![ModelCandidateV1 {
                binding_id: descriptor.capability_id.clone(),
                version_hash: descriptor.version_hash.clone(),
            }],
            maximum_input_bytes: MAXIMUM_INPUT_BYTES,
            maximum_output_bytes: MAXIMUM_OUTPUT_BYTES,
        },
        &ModelRequestV1 {
            input: context.clone(),
        },
        cancellation,
    );
    match execution {
        Ok(evidence) => {
            let mut assistant_text = String::new();
            let mut usage = None;
            for event in evidence.events {
                match event {
                    ModelEventV1::AssistantOutput(text) => assistant_text.push_str(&text),
                    ModelEventV1::Usage {
                        input_tokens,
                        output_tokens,
                    } => usage = Some((input_tokens, output_tokens)),
                    ModelEventV1::ReasoningRaw(_)
                    | ModelEventV1::ReasoningSummary(_)
                    | ModelEventV1::Progress(_) => {}
                }
            }
            let (input_units, output_units) = usage.unwrap_or_default();
            if assistant_text.trim().is_empty() {
                ProviderOutcomeRecordV1 {
                    schema_version: 1,
                    invocation_id: envelope.invocation_id.clone(),
                    status: SimpleChatExecutionStatusV1::FailedKnownStarted,
                    assistant_text: None,
                    error: Some(
                        "provider accepted the request but returned no assistant text".to_owned(),
                    ),
                    model: model.to_owned(),
                    input_units,
                    output_units,
                    attempted_model_turns: 1,
                    settled_tool_calls: 0,
                    tool_exchanges: Vec::new(),
                    tool_activity: Vec::new(),
                    scheduler_checkpoint: None,
                    scheduler_trace: Vec::new(),
                }
            } else {
                ProviderOutcomeRecordV1 {
                    schema_version: 1,
                    invocation_id: envelope.invocation_id.clone(),
                    status: SimpleChatExecutionStatusV1::Succeeded,
                    assistant_text: Some(assistant_text),
                    error: None,
                    model: model.to_owned(),
                    input_units,
                    output_units,
                    attempted_model_turns: 1,
                    settled_tool_calls: 0,
                    tool_exchanges: Vec::new(),
                    tool_activity: Vec::new(),
                    scheduler_checkpoint: None,
                    scheduler_trace: Vec::new(),
                }
            }
        }
        Err(error) => ProviderOutcomeRecordV1 {
            schema_version: 1,
            invocation_id: envelope.invocation_id.clone(),
            status: provider_error_status(&error),
            assistant_text: None,
            error: Some(redact_error(materialized, &error.to_string())),
            model: model.to_owned(),
            input_units: 0,
            output_units: 0,
            attempted_model_turns: 1,
            settled_tool_calls: 0,
            tool_exchanges: Vec::new(),
            tool_activity: Vec::new(),
            scheduler_checkpoint: None,
            scheduler_trace: Vec::new(),
        },
    }
}

#[derive(Clone)]
struct CoreSecretLeaseClient {
    authority: Arc<PipelineLeaseAuthority>,
}

impl SecretLeaseClientV1 for CoreSecretLeaseClient {
    fn redeem(
        &self,
        request: &HostRedeemLeaseRequestV1,
    ) -> Result<HostSecretDeliveryV1, SecretMaterializationError> {
        self.authority.redeem(request)
    }

    fn revoke(&self, lease_id: &StableId) -> Result<(), SecretMaterializationError> {
        self.authority.revoke(lease_id)
    }
}

struct CommittedWorkerAck;

impl CommittedWorkerResultPortV1 for CommittedWorkerAck {
    fn deliver(&self, _result: &WorkerResultOutboxV1) -> Result<DeliveryAcceptanceV1, BrokerError> {
        Ok(DeliveryAcceptanceV1::Accepted)
    }
}

#[derive(Clone)]
struct PipelineRecordStore {
    store: LocalHistoryStore,
    write_lock: Arc<Mutex<()>>,
}

impl PipelineRecordStore {
    fn open(path: &Path) -> Result<Self, SimpleChatPipelineError> {
        Ok(Self {
            store: LocalHistoryStore::open(path).map_err(local_store_error)?,
            write_lock: Arc::new(Mutex::new(())),
        })
    }

    fn record_execution(
        &self,
        record: &PreparedExecutionRecordV1,
    ) -> Result<bool, SimpleChatPipelineError> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| SimpleChatPipelineError::Store("record lock poisoned".into()))?;
        let executions = self.executions()?;
        if let Some(existing) = executions
            .iter()
            .find(|existing| existing.request_id == record.request_id)
        {
            return if existing == record {
                Ok(true)
            } else {
                Err(SimpleChatPipelineError::Store(
                    "request ID was reused with changed frozen execution semantics".to_owned(),
                ))
            };
        }
        if executions.iter().any(|existing| {
            existing.snapshot.chat_id == record.snapshot.chat_id
                && existing.snapshot.run_id == record.snapshot.run_id
                && existing.scheduler_continuation == record.scheduler_continuation
        }) {
            return Err(SimpleChatPipelineError::Store(
                "this frozen Run continuation already has a different durable request".to_owned(),
            ));
        }
        self.append_record_without_lock(
            "pipeline.execution-prepared",
            &record.request_id,
            serde_json::to_value(record).map_err(json_error)?,
        )?;
        Ok(false)
    }

    fn record_outcome(
        &self,
        outcome: &ProviderOutcomeRecordV1,
    ) -> Result<bool, SimpleChatPipelineError> {
        if let Some(existing) = self.outcome(&outcome.invocation_id)? {
            return if existing == *outcome {
                Ok(true)
            } else {
                Err(SimpleChatPipelineError::Store(
                    "provider outcome identity was reused with changed evidence".to_owned(),
                ))
            };
        }
        self.append_record(
            "pipeline.provider-outcome",
            &digest_id("record.outcome", outcome.invocation_id.as_str())?,
            serde_json::to_value(outcome).map_err(json_error)?,
        )?;
        Ok(false)
    }

    fn append_record(
        &self,
        kind: &str,
        dedup_key: &StableId,
        record: Value,
    ) -> Result<(), SimpleChatPipelineError> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| SimpleChatPipelineError::Store("record lock poisoned".into()))?;
        self.append_record_without_lock(kind, dedup_key, record)
    }

    fn append_record_without_lock(
        &self,
        kind: &str,
        dedup_key: &StableId,
        record: Value,
    ) -> Result<(), SimpleChatPipelineError> {
        let expected_head = self
            .store
            .events(PIPELINE_CHAT_ID, STORE_BRANCH_ID)
            .map_err(local_store_error)?
            .len() as u64;
        let event_id = digest_id(
            "record.event",
            &format!("{}:{}", kind, canonical_hash(&record)?),
        )?;
        self.store
            .commit(&CommitBatch {
                chat_id: PIPELINE_CHAT_ID.to_owned(),
                branch_id: STORE_BRANCH_ID.to_owned(),
                expected_head,
                events: vec![Event {
                    event_id: event_id.to_string(),
                    kind: kind.to_owned(),
                    payload: json!({"schemaVersion": 1, "record": record}),
                }],
                attempt: None,
                checkpoint: None,
                deduplication: Some(Deduplication {
                    key_type: kind.to_owned(),
                    key: dedup_key.to_string(),
                    request_hash: String::new(),
                }),
                outbox: Vec::new(),
            })
            .map_err(local_store_error)?;
        Ok(())
    }

    fn execution(
        &self,
        request_id: &StableId,
    ) -> Result<Option<PreparedExecutionRecordV1>, SimpleChatPipelineError> {
        for record in self.executions()? {
            if &record.request_id == request_id {
                return Ok(Some(record));
            }
        }
        Ok(None)
    }

    fn executions(&self) -> Result<Vec<PreparedExecutionRecordV1>, SimpleChatPipelineError> {
        self.events_of_kind("pipeline.execution-prepared")?
            .into_iter()
            .map(|event| serde_json::from_value(event).map_err(json_error))
            .collect()
    }

    fn execution_for_dispatch(
        &self,
        dispatch: &ApprovedDispatchV1,
    ) -> Result<Option<PreparedExecutionRecordV1>, SimpleChatPipelineError> {
        for record in self.executions()? {
            if record.broker_proposal.proposal_id == dispatch.proposal_id {
                return Ok(Some(record));
            }
        }
        Ok(None)
    }

    fn execution_for_chat_or_run(
        &self,
        chat_id: &StableId,
        run_id: &StableId,
    ) -> Result<Option<PreparedExecutionRecordV1>, SimpleChatPipelineError> {
        for record in self.executions()? {
            if record.snapshot.chat_id == *chat_id || record.snapshot.run_id == *run_id {
                if record.snapshot.chat_id != *chat_id || record.snapshot.run_id != *run_id {
                    return Err(SimpleChatPipelineError::Store(
                        "Chat and Run identities no longer refer to the same frozen session"
                            .to_owned(),
                    ));
                }
                return Ok(Some(record));
            }
        }
        Ok(None)
    }

    fn outcome(
        &self,
        invocation_id: &StableId,
    ) -> Result<Option<ProviderOutcomeRecordV1>, SimpleChatPipelineError> {
        Ok(self
            .outcomes()?
            .into_iter()
            .find(|outcome| &outcome.invocation_id == invocation_id))
    }

    fn outcomes(&self) -> Result<Vec<ProviderOutcomeRecordV1>, SimpleChatPipelineError> {
        self.events_of_kind("pipeline.provider-outcome")?
            .into_iter()
            .map(|event| serde_json::from_value(event).map_err(json_error))
            .collect()
    }

    fn events_of_kind(&self, kind: &str) -> Result<Vec<Value>, SimpleChatPipelineError> {
        Ok(self
            .store
            .events(PIPELINE_CHAT_ID, STORE_BRANCH_ID)
            .map_err(local_store_error)?
            .into_iter()
            .filter(|event| event.kind == kind)
            .filter_map(|event| event.payload.get("record").cloned())
            .collect())
    }
}

#[derive(Clone)]
pub(super) struct LocalInvocationLedger {
    store: LocalHistoryStore,
    write_lock: Arc<Mutex<()>>,
    aggregate_id: String,
    host_destination: String,
    worker_destination: String,
}

impl LocalInvocationLedger {
    fn open(path: &Path) -> Result<Self, SimpleChatPipelineError> {
        Self::open_scoped(path, BROKER_CHAT_ID, HOST_DESTINATION, WORKER_DESTINATION)
    }

    pub(super) fn open_scoped(
        path: &Path,
        aggregate_id: &str,
        host_destination: &str,
        worker_destination: &str,
    ) -> Result<Self, SimpleChatPipelineError> {
        Ok(Self {
            store: LocalHistoryStore::open(path).map_err(local_store_error)?,
            write_lock: Arc::new(Mutex::new(())),
            aggregate_id: aggregate_id.to_owned(),
            host_destination: host_destination.to_owned(),
            worker_destination: worker_destination.to_owned(),
        })
    }

    fn broker_events(&self) -> Result<Vec<InvocationLedgerEventV1>, BrokerError> {
        self.store
            .events(&self.aggregate_id, STORE_BRANCH_ID)
            .map_err(|_| BrokerError::Unavailable)?
            .into_iter()
            .filter(|event| event.kind.starts_with("broker."))
            .map(|event| {
                event
                    .payload
                    .get("event")
                    .cloned()
                    .ok_or(BrokerError::Unavailable)
                    .and_then(|value| {
                        serde_json::from_value(value).map_err(|_| BrokerError::Unavailable)
                    })
            })
            .collect()
    }

    fn append_broker_batch(
        &self,
        events: &[InvocationLedgerEventV1],
        outbox: Option<(&str, StableId, Value)>,
    ) -> Result<(), BrokerError> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| BrokerError::Unavailable)?;
        let existing = self
            .store
            .events(&self.aggregate_id, STORE_BRANCH_ID)
            .map_err(|_| BrokerError::Unavailable)?;
        let expected_head = u64::try_from(existing.len()).map_err(|_| BrokerError::Unavailable)?;
        let batch_identity = canonical_hash(&json!({"events":events,"outbox":outbox}))
            .map_err(|_| BrokerError::Unavailable)?;
        let persisted_events = events
            .iter()
            .enumerate()
            .map(|(ordinal, event)| {
                let identity = canonical_hash(&json!({"ordinal":ordinal,"event":event}))
                    .map_err(|_| BrokerError::Unavailable)?;
                Ok(Event {
                    event_id: digest_id(
                        "broker.event",
                        &format!("{}:{identity}", self.aggregate_id),
                    )
                    .map_err(|_| BrokerError::Unavailable)?
                    .to_string(),
                    kind: broker_event_kind(event).to_owned(),
                    payload: json!({"schemaVersion":1,"event":event}),
                })
            })
            .collect::<Result<Vec<_>, BrokerError>>()?;
        let outbox = outbox
            .map(|(destination, outbox_id, payload)| OutboxEntry {
                outbox_id: outbox_id.to_string(),
                destination: destination.to_owned(),
                payload: json!({"schemaVersion":1,"entry":payload}),
            })
            .into_iter()
            .collect();
        self.store
            .commit(&CommitBatch {
                chat_id: self.aggregate_id.clone(),
                branch_id: STORE_BRANCH_ID.to_owned(),
                expected_head,
                events: persisted_events,
                attempt: None,
                checkpoint: None,
                deduplication: Some(Deduplication {
                    key_type: "broker.atomic".to_owned(),
                    key: digest_id(
                        "broker.batch",
                        &format!("{}:{batch_identity}", self.aggregate_id),
                    )
                    .map_err(|_| BrokerError::Unavailable)?
                    .to_string(),
                    request_hash: String::new(),
                }),
                outbox,
            })
            .map_err(map_store_to_broker)?;
        Ok(())
    }

    fn pending_entries(&self, destination: &str) -> Result<Vec<Value>, BrokerError> {
        let mut cursor = 0;
        let mut values = Vec::new();
        loop {
            let page = self
                .store
                .pending_outbox_v1(cursor, 512)
                .map_err(map_store_to_broker)?;
            if page.is_empty() {
                break;
            }
            cursor = page.last().map_or(cursor, |entry| entry.delivery_cursor);
            values.extend(
                page.into_iter()
                    .filter(|entry| entry.destination == destination)
                    .filter_map(|entry| entry.payload.get("entry").cloned()),
            );
        }
        Ok(values)
    }

    pub(super) fn settlement(
        &self,
        invocation_id: &StableId,
    ) -> Result<Option<(String, bool)>, SimpleChatPipelineError> {
        Ok(self
            .events(invocation_id)
            .map_err(broker_error)?
            .into_iter()
            .find_map(|event| match event {
                InvocationLedgerEventV1::Settled {
                    outcome_hash,
                    uncertain,
                    ..
                } => Some((outcome_hash, uncertain)),
                _ => None,
            }))
    }

    pub(super) fn invocation_for_proposal(
        &self,
        proposal_id: &StableId,
    ) -> Result<Option<StableId>, SimpleChatPipelineError> {
        Ok(self
            .broker_events()
            .map_err(broker_error)?
            .into_iter()
            .find_map(|event| match event {
                InvocationLedgerEventV1::Proposed {
                    invocation_id,
                    proposal,
                    ..
                } if &proposal.proposal_id == proposal_id => Some(invocation_id),
                _ => None,
            }))
    }
}

impl InvocationLedgerPortV1 for LocalInvocationLedger {
    fn append_atomic(
        &self,
        events: &[InvocationLedgerEventV1],
        outbox: Option<&DispatchOutboxV1>,
    ) -> Result<(), BrokerError> {
        let outbox = outbox
            .map(|entry| {
                Ok((
                    self.host_destination.as_str(),
                    entry.outbox_id.clone(),
                    serde_json::to_value(entry).map_err(|_| BrokerError::Unavailable)?,
                ))
            })
            .transpose()?;
        self.append_broker_batch(events, outbox)
    }

    fn events(
        &self,
        invocation_id: &StableId,
    ) -> Result<Vec<InvocationLedgerEventV1>, BrokerError> {
        Ok(self
            .broker_events()?
            .into_iter()
            .filter(|event| broker_event_invocation_id(event) == invocation_id)
            .collect())
    }

    fn pending_dispatches(&self) -> Result<Vec<DispatchOutboxV1>, BrokerError> {
        self.pending_entries(&self.host_destination)?
            .into_iter()
            .map(|value| serde_json::from_value(value).map_err(|_| BrokerError::Unavailable))
            .collect()
    }

    fn mark_dispatch_delivered(&self, outbox_id: &StableId) -> Result<(), BrokerError> {
        self.store
            .mark_outbox_delivered(outbox_id.as_str())
            .map_err(map_store_to_broker)
    }

    fn append_settlement_atomic(
        &self,
        event: &InvocationLedgerEventV1,
        outbox: &WorkerResultOutboxV1,
    ) -> Result<(), BrokerError> {
        self.append_broker_batch(
            std::slice::from_ref(event),
            Some((
                self.worker_destination.as_str(),
                outbox.outbox_id.clone(),
                serde_json::to_value(outbox).map_err(|_| BrokerError::Unavailable)?,
            )),
        )
    }

    fn pending_worker_results(&self) -> Result<Vec<WorkerResultOutboxV1>, BrokerError> {
        self.pending_entries(&self.worker_destination)?
            .into_iter()
            .map(|value| serde_json::from_value(value).map_err(|_| BrokerError::Unavailable))
            .collect()
    }

    fn mark_worker_result_delivered(&self, outbox_id: &StableId) -> Result<(), BrokerError> {
        self.store
            .mark_outbox_delivered(outbox_id.as_str())
            .map_err(map_store_to_broker)
    }
}

fn validate_follow_up_context(
    previous: &PreparedExecutionRecordV1,
    outcome: &ProviderOutcomeRecordV1,
    request: &SimpleChatExecutionRequestV1,
) -> Result<(), SimpleChatPipelineError> {
    let previous_messages: Vec<SimpleChatMessageV1> = serde_json::from_value(
        previous
            .worker_proposal
            .payload
            .get("context")
            .and_then(|context| context.get("messages"))
            .cloned()
            .ok_or(SimpleChatPipelineError::IncompleteEvidence)?,
    )
    .map_err(|_| SimpleChatPipelineError::IncompleteEvidence)?;
    let assistant_text = outcome
        .assistant_text
        .as_deref()
        .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
    let expected_len = previous_messages
        .len()
        .checked_add(2)
        .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
    let extends_exact_context = request.messages.len() == expected_len
        && request.messages.starts_with(&previous_messages)
        && request
            .messages
            .get(previous_messages.len())
            .is_some_and(|message| {
                message.role == "assistant" && message.content == assistant_text
            })
        && request
            .messages
            .last()
            .is_some_and(|message| message.role == "user");
    if !extends_exact_context {
        return Err(SimpleChatPipelineError::Store(
            "follow-up Input must extend the settled frozen conversation by its exact assistant output and one new user message"
                .to_owned(),
        ));
    }
    Ok(())
}

fn prepare_scheduler_for_agent(
    snapshot: &aworkit_protocol::WorkerFrozenRunSnapshotV1,
    continuation: Option<&SchedulerContinuationBasisV1>,
) -> Result<
    (
        SchedulerCheckpointV1,
        Vec<SchedulerTraceEntryV1>,
        u64,
        StableId,
    ),
    SimpleChatPipelineError,
> {
    let plan = ExecutionPlanV1::compile(snapshot.clone(), &snapshot.snapshot_hash)
        .map_err(worker_error)?;
    let (mut scheduler, mut trace, scheduler_continuation, input) =
        if let Some(continuation) = continuation {
            if continuation.continuation == 0 {
                return Err(SimpleChatPipelineError::IncompleteEvidence);
            }
            validate_scheduler_trace_sequence(&continuation.trace)?;
            let mut scheduler = SchedulerV1::restore(plan, continuation.checkpoint.clone())
                .map_err(worker_error)?;
            if !scheduler.is_quiescent() {
                return Err(SimpleChatPipelineError::IncompleteEvidence);
            }
            let suspended_waits = scheduler
                .checkpoint()
                .tokens
                .into_iter()
                .filter(|token| {
                    token.node_id.as_str() == "wait.1" && token.state == TokenStateV1::Suspended
                })
                .collect::<Vec<_>>();
            if suspended_waits.len() != 1 {
                return Err(SimpleChatPipelineError::IncompleteEvidence);
            }
            let suspended_wait = suspended_waits
                .into_iter()
                .next()
                .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
            if continuation.trace.last().is_none_or(|entry| {
                entry.action != "suspended"
                    || entry.node_id.as_str() != "wait.1"
                    || entry.token_id != suspended_wait.token_id
            }) {
                return Err(SimpleChatPipelineError::IncompleteEvidence);
            }
            let mut trace = continuation.trace.clone();
            scheduler
                .resume(&suspended_wait.token_id)
                .map_err(worker_error)?;
            push_scheduler_trace(&mut trace, "resumed", &suspended_wait, None, None)?;
            let resumed_wait = scheduler
                .claim_next()
                .filter(|token| {
                    token.token_id == suspended_wait.token_id
                        && token.node_id.as_str() == "wait.1"
                        && token.state == TokenStateV1::InFlight
                })
                .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
            push_scheduler_trace(&mut trace, "claimed", &resumed_wait, None, None)?;
            let input_node_id = snapshot
                .entry_nodes
                .first()
                .filter(|node_id| node_id.as_str() == "input.1")
                .cloned()
                .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
            let input_revision = continuation
                .checkpoint
                .committed_cursor
                .checked_add(1)
                .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
            let input_received = scheduler
                .propose_wait_input(
                    &resumed_wait.token_id,
                    json!({
                        "inputReceived":true,
                        "schedulerContinuation":continuation.continuation,
                    }),
                )
                .map_err(worker_error)?;
            let acknowledgement = scheduler
                .acknowledge_wait_input(&input_received.proposal_id, input_revision)
                .map_err(worker_error)?;
            if acknowledgement.duplicate || acknowledgement.admitted_token.is_some() {
                return Err(SimpleChatPipelineError::IncompleteEvidence);
            }
            push_scheduler_trace(
                &mut trace,
                "input_received_acknowledged",
                &resumed_wait,
                None,
                Some(input_node_id.clone()),
            )?;
            let admitted_input = scheduler
                .enqueue(
                    input_node_id.clone(),
                    input_revision,
                    resumed_wait.branch_lineage.clone(),
                )
                .map_err(worker_error)?;
            let input = scheduler
                .claim_next()
                .filter(|token| token.token_id == admitted_input.token_id)
                .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
            (scheduler, trace, continuation.continuation, input)
        } else {
            let mut scheduler = SchedulerV1::new(plan);
            let seeded = scheduler.seed_entries(0).map_err(worker_error)?;
            if seeded.len() != 1 || seeded[0].node_id.as_str() != "input.1" {
                return Err(SimpleChatPipelineError::IncompleteEvidence);
            }
            let input = scheduler
                .claim_next()
                .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
            (scheduler, Vec::new(), 0, input)
        };
    let input = scheduler
        .checkpoint()
        .tokens
        .into_iter()
        .find(|token| token.token_id == input.token_id)
        .filter(|token| {
            token.node_id.as_str() == "input.1" && token.state == TokenStateV1::InFlight
        })
        .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
    if input.context_revision != scheduler.checkpoint().committed_cursor {
        return Err(SimpleChatPipelineError::IncompleteEvidence);
    }
    push_scheduler_trace(&mut trace, "claimed", &input, None, None)?;
    let proposal = scheduler
        .propose_transition(
            &input.token_id,
            json!({
                "inputCommitted":true,
                "schedulerContinuation":scheduler_continuation,
            }),
        )
        .map_err(worker_error)?;
    let target = transition_target(snapshot, &proposal.transition_id)?;
    if target.as_str() != "agent.1" {
        return Err(SimpleChatPipelineError::IncompleteEvidence);
    }
    let input_cursor = scheduler
        .checkpoint()
        .committed_cursor
        .checked_add(1)
        .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
    let admission = scheduler
        .acknowledge_transition(&proposal.proposal_id, input_cursor, input_cursor)
        .map_err(worker_error)?;
    let admitted = admission
        .admitted_token
        .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
    if admission.duplicate || admitted.node_id != target {
        return Err(SimpleChatPipelineError::IncompleteEvidence);
    }
    push_scheduler_trace(
        &mut trace,
        "transition_acknowledged",
        &input,
        Some(proposal.transition_id),
        Some(target),
    )?;
    let agent = scheduler
        .claim_next()
        .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
    if agent.token_id != admitted.token_id
        || agent.node_id.as_str() != "agent.1"
        || agent.state != TokenStateV1::InFlight
    {
        return Err(SimpleChatPipelineError::IncompleteEvidence);
    }
    push_scheduler_trace(&mut trace, "claimed", &agent, None, None)?;
    Ok((
        scheduler.checkpoint(),
        trace,
        scheduler_continuation,
        agent.token_id,
    ))
}

fn finalize_scheduler_evidence(
    prepared: &PreparedExecutionRecordV1,
    outcome: &mut ProviderOutcomeRecordV1,
) -> Result<(), SimpleChatPipelineError> {
    let checkpoint = prepared
        .scheduler_checkpoint
        .clone()
        .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
    let agent_token_id = prepared
        .agent_token_id
        .as_ref()
        .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
    let plan =
        ExecutionPlanV1::compile(prepared.snapshot.clone(), &prepared.snapshot.snapshot_hash)
            .map_err(worker_error)?;
    let mut scheduler = SchedulerV1::restore(plan, checkpoint).map_err(worker_error)?;
    let agent = scheduler
        .checkpoint()
        .tokens
        .into_iter()
        .find(|token| &token.token_id == agent_token_id)
        .filter(|token| {
            token.node_id.as_str() == "agent.1" && token.state == TokenStateV1::InFlight
        })
        .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
    let mut trace = prepared.scheduler_trace.clone();

    if outcome.status == SimpleChatExecutionStatusV1::Succeeded {
        let output_cursor = scheduler
            .checkpoint()
            .committed_cursor
            .checked_add(1)
            .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
        let proposal = scheduler
            .propose_transition(
                agent_token_id,
                json!({
                    "status":"succeeded",
                    "inputUnits":outcome.input_units,
                    "outputUnits":outcome.output_units,
                }),
            )
            .map_err(worker_error)?;
        let output_id = transition_target(&prepared.snapshot, &proposal.transition_id)?;
        if output_id.as_str() != "output.1" {
            return Err(SimpleChatPipelineError::IncompleteEvidence);
        }
        let admission = scheduler
            .acknowledge_transition(&proposal.proposal_id, output_cursor, output_cursor)
            .map_err(worker_error)?;
        let admitted_output = admission
            .admitted_token
            .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
        if admission.duplicate || admitted_output.node_id != output_id {
            return Err(SimpleChatPipelineError::IncompleteEvidence);
        }
        push_scheduler_trace(
            &mut trace,
            "transition_acknowledged",
            &agent,
            Some(proposal.transition_id),
            Some(output_id),
        )?;
        let output = scheduler
            .claim_next()
            .filter(|token| token.token_id == admitted_output.token_id)
            .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
        push_scheduler_trace(&mut trace, "claimed", &output, None, None)?;

        let proposal = scheduler
            .propose_transition(&output.token_id, json!({"outputCommitted":true}))
            .map_err(worker_error)?;
        let wait_id = transition_target(&prepared.snapshot, &proposal.transition_id)?;
        if wait_id.as_str() != "wait.1" {
            return Err(SimpleChatPipelineError::IncompleteEvidence);
        }
        let wait_cursor = scheduler
            .checkpoint()
            .committed_cursor
            .checked_add(1)
            .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
        let admission = scheduler
            .acknowledge_transition(&proposal.proposal_id, wait_cursor, wait_cursor)
            .map_err(worker_error)?;
        let admitted_wait = admission
            .admitted_token
            .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
        if admission.duplicate || admitted_wait.node_id != wait_id {
            return Err(SimpleChatPipelineError::IncompleteEvidence);
        }
        push_scheduler_trace(
            &mut trace,
            "transition_acknowledged",
            &output,
            Some(proposal.transition_id),
            Some(wait_id),
        )?;
        let wait = scheduler
            .claim_next()
            .filter(|token| token.token_id == admitted_wait.token_id)
            .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
        push_scheduler_trace(&mut trace, "claimed", &wait, None, None)?;
        scheduler.suspend(&wait.token_id).map_err(worker_error)?;
        push_scheduler_trace(&mut trace, "suspended", &wait, None, None)?;
        if !scheduler.is_quiescent() {
            return Err(SimpleChatPipelineError::IncompleteEvidence);
        }
    } else {
        if scheduler.cancel_lineage("root") != 1 {
            return Err(SimpleChatPipelineError::IncompleteEvidence);
        }
        push_scheduler_trace(&mut trace, "failed", &agent, None, None)?;
    }
    outcome.scheduler_checkpoint = Some(scheduler.checkpoint());
    outcome.scheduler_trace = trace;
    Ok(())
}

fn push_scheduler_trace(
    trace: &mut Vec<SchedulerTraceEntryV1>,
    action: &str,
    token: &aworkit_workflow_worker::scheduler::TokenV1,
    transition_id: Option<StableId>,
    target_node_id: Option<StableId>,
) -> Result<(), SimpleChatPipelineError> {
    let sequence = u64::try_from(trace.len())
        .map_err(|_| SimpleChatPipelineError::IncompleteEvidence)?
        .checked_add(1)
        .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
    trace.push(SchedulerTraceEntryV1 {
        sequence,
        action: action.to_owned(),
        node_id: token.node_id.clone(),
        token_id: token.token_id.clone(),
        transition_id,
        target_node_id,
    });
    Ok(())
}

fn validate_scheduler_trace_sequence(
    trace: &[SchedulerTraceEntryV1],
) -> Result<(), SimpleChatPipelineError> {
    let valid = trace.iter().enumerate().all(|(index, entry)| {
        u64::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            == Some(entry.sequence)
    });
    valid
        .then_some(())
        .ok_or(SimpleChatPipelineError::IncompleteEvidence)
}

fn transition_target(
    snapshot: &aworkit_protocol::WorkerFrozenRunSnapshotV1,
    transition_id: &StableId,
) -> Result<StableId, SimpleChatPipelineError> {
    snapshot
        .transitions
        .iter()
        .find(|transition| &transition.transition_id == transition_id)
        .map(|transition| transition.to_node.clone())
        .ok_or(SimpleChatPipelineError::IncompleteEvidence)
}

fn validate_provider_outcome_accounting(
    prepared: &PreparedExecutionRecordV1,
    outcome: &ProviderOutcomeRecordV1,
) -> Result<(), SimpleChatPipelineError> {
    let turns = u64::from(outcome.attempted_model_turns);
    let tool_calls = u64::from(outcome.settled_tool_calls);
    let requires_attempt = !matches!(
        outcome.status,
        SimpleChatExecutionStatusV1::FailedDefinitelyNotStarted
    );
    if turns > u64::from(prepared.maximum_turns)
        || tool_calls > prepared.snapshot.budget.tool_calls
        || (turns == 0 && requires_attempt)
        || (tool_calls > 0 && turns == 0)
        || turns.saturating_add(tool_calls) > prepared.snapshot.budget.actions
    {
        return Err(SimpleChatPipelineError::IncompleteEvidence);
    }
    Ok(())
}

fn compact_provider_outcome_if_needed(
    outcome: &mut ProviderOutcomeRecordV1,
) -> Result<(), SimpleChatPipelineError> {
    if serialized_len(outcome)? <= MAXIMUM_PROVIDER_OUTCOME_BYTES {
        return Ok(());
    }
    outcome.status = if outcome.attempted_model_turns == 0 {
        SimpleChatExecutionStatusV1::FailedDefinitelyNotStarted
    } else {
        SimpleChatExecutionStatusV1::FailedKnownStarted
    };
    outcome.assistant_text = None;
    outcome.error = Some(
        "Provider/tool evidence exceeded the durable outcome bound; large exchange bodies were omitted after their individual authority outcomes were committed."
            .into(),
    );
    outcome.tool_exchanges.clear();
    if serialized_len(outcome)? > MAXIMUM_PROVIDER_OUTCOME_BYTES {
        return Err(SimpleChatPipelineError::IncompleteEvidence);
    }
    Ok(())
}

fn serialized_len<T: Serialize>(value: &T) -> Result<usize, SimpleChatPipelineError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(json_error)
}

fn enforce_serialized_bound<T: Serialize>(
    value: &T,
    maximum: usize,
    label: &str,
) -> Result<(), SimpleChatPipelineError> {
    if serialized_len(value)? <= maximum {
        Ok(())
    } else {
        Err(SimpleChatPipelineError::InvalidInput(format!(
            "{label} exceeds its persistence-safe byte bound"
        )))
    }
}

fn worker_error(error: impl std::fmt::Display) -> SimpleChatPipelineError {
    SimpleChatPipelineError::Worker(error.to_string())
}

fn settle_worker_contract(
    prepared: &PreparedExecutionRecordV1,
    outcome: &ProviderOutcomeRecordV1,
    outcome_hash: &str,
) -> Result<(), SimpleChatPipelineError> {
    validate_final_scheduler_evidence(prepared, outcome)?;
    let observed_tokens = outcome.input_units.saturating_add(outcome.output_units);
    // Provider usage can arrive above the frozen allowance only after the
    // provider has started. Preserve the exact observation in the outcome,
    // while the non-minting ledger charges the complete reserved allowance.
    let charged_tokens = observed_tokens.min(prepared.snapshot.budget.tokens);
    let mut agent = AgentLoopV1::restore(prepared.agent_checkpoint.clone())
        .map_err(|error| SimpleChatPipelineError::Worker(error.to_string()))?;
    let mut limits = LimitLedger::restore(
        prepared.limit_checkpoint.clone(),
        prepared.limit_checkpoint.current_tick,
    )
    .map_err(|error| SimpleChatPipelineError::Worker(error.to_string()))?;
    let class = match outcome.status {
        SimpleChatExecutionStatusV1::Succeeded => CapabilityOutcomeClassV1::Success,
        SimpleChatExecutionStatusV1::FailedDefinitelyNotStarted => {
            CapabilityOutcomeClassV1::DefiniteNotStarted
        }
        SimpleChatExecutionStatusV1::FailedKnownStarted => {
            CapabilityOutcomeClassV1::FailedKnownStarted
        }
        SimpleChatExecutionStatusV1::OutcomeUncertain => CapabilityOutcomeClassV1::Uncertain,
    };
    let worker_outcome = WorkerCapabilityOutcomeV1 {
        outcome_id: digest_id("worker.outcome", outcome_hash)?,
        invocation_id: prepared.worker_proposal.invocation_id.clone(),
        class,
        retry_safe_proof: matches!(
            outcome.status,
            SimpleChatExecutionStatusV1::FailedDefinitelyNotStarted
        ),
        payload: serde_json::to_value(outcome).map_err(json_error)?,
        usage: Some(json!({
            "turns": outcome.attempted_model_turns,
            "attempts": outcome.attempted_model_turns,
            "toolCalls": outcome.settled_tool_calls,
            "observedTokens": observed_tokens,
            "chargedTokens": charged_tokens,
            "actions": u64::from(outcome.attempted_model_turns)
                .saturating_add(u64::from(outcome.settled_tool_calls)),
        })),
    };
    agent
        .settle_committed_run_outcome(
            &worker_outcome,
            &mut limits,
            Usage {
                turns: u64::from(outcome.attempted_model_turns),
                attempts: u64::from(outcome.attempted_model_turns),
                tool_calls: u64::from(outcome.settled_tool_calls),
                tokens: charged_tokens,
                cost_micros: 0,
                actions: u64::from(outcome.attempted_model_turns)
                    .saturating_add(u64::from(outcome.settled_tool_calls)),
            },
        )
        .map_err(|error| SimpleChatPipelineError::Worker(error.to_string()))?;
    Ok(())
}

fn validate_final_scheduler_evidence(
    prepared: &PreparedExecutionRecordV1,
    outcome: &ProviderOutcomeRecordV1,
) -> Result<(), SimpleChatPipelineError> {
    validate_scheduler_trace_sequence(&outcome.scheduler_trace)?;
    if outcome.scheduler_trace.len() < prepared.scheduler_trace.len()
        || outcome.scheduler_trace[..prepared.scheduler_trace.len()] != prepared.scheduler_trace
    {
        return Err(SimpleChatPipelineError::IncompleteEvidence);
    }
    let checkpoint = outcome
        .scheduler_checkpoint
        .clone()
        .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
    let plan =
        ExecutionPlanV1::compile(prepared.snapshot.clone(), &prepared.snapshot.snapshot_hash)
            .map_err(worker_error)?;
    let scheduler = SchedulerV1::restore(plan, checkpoint).map_err(worker_error)?;
    let checkpoint = scheduler.checkpoint();
    let tokens = &checkpoint.tokens;
    let continuation = usize::try_from(prepared.scheduler_continuation)
        .map_err(|_| SimpleChatPipelineError::IncompleteEvidence)?;
    let turns = continuation
        .checked_add(1)
        .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
    let count = |node_id: &str, state: TokenStateV1| {
        tokens
            .iter()
            .filter(|token| token.node_id.as_str() == node_id && token.state == state)
            .count()
    };
    let completed_waits_are_input_events = tokens.iter().all(|token| {
        if token.node_id.as_str() == "wait.1" && token.state == TokenStateV1::Completed {
            token.external_completion
                == Some(aworkit_workflow_worker::scheduler::ExternalCompletionV1::WaitInputReceived)
        } else if token.node_id.as_str() != "wait.1" && token.state == TokenStateV1::Completed {
            token.external_completion.is_none()
        } else {
            true
        }
    });
    let valid = if outcome.status == SimpleChatExecutionStatusV1::Succeeded {
        tokens.len() == turns.saturating_mul(4)
            && count("input.1", TokenStateV1::Completed) == turns
            && count("agent.1", TokenStateV1::Completed) == turns
            && count("output.1", TokenStateV1::Completed) == turns
            && count("wait.1", TokenStateV1::Completed) == continuation
            && count("wait.1", TokenStateV1::Suspended) == 1
            && checkpoint.committed_cursor
                == prepared
                    .scheduler_continuation
                    .checked_mul(4)
                    .and_then(|cursor| cursor.checked_add(3))
                    .ok_or(SimpleChatPipelineError::IncompleteEvidence)?
            && outcome.scheduler_trace.last().is_some_and(|entry| {
                entry.action == "suspended" && entry.node_id.as_str() == "wait.1"
            })
            && completed_waits_are_input_events
            && scheduler.is_quiescent()
    } else {
        tokens.len() == continuation.saturating_mul(4).saturating_add(2)
            && count("input.1", TokenStateV1::Completed) == turns
            && count("agent.1", TokenStateV1::Completed) == continuation
            && count("agent.1", TokenStateV1::Cancelled) == 1
            && count("output.1", TokenStateV1::Completed) == continuation
            && count("wait.1", TokenStateV1::Completed) == continuation
            && count("wait.1", TokenStateV1::Suspended) == 0
            && checkpoint.committed_cursor
                == prepared
                    .scheduler_continuation
                    .checked_mul(4)
                    .and_then(|cursor| cursor.checked_add(1))
                    .ok_or(SimpleChatPipelineError::IncompleteEvidence)?
            && outcome.scheduler_trace.last().is_some_and(|entry| {
                entry.action == "failed" && entry.node_id.as_str() == "agent.1"
            })
            && completed_waits_are_input_events
            && scheduler.is_quiescent()
    };
    valid
        .then_some(())
        .ok_or(SimpleChatPipelineError::IncompleteEvidence)
}

struct CompiledSimpleChatGraphV1 {
    nodes: Vec<WorkerNodeV1>,
    transitions: Vec<WorkerTransitionV1>,
    entry_nodes: Vec<StableId>,
    model_node_id: StableId,
}

fn compile_simple_chat_graph(
    request: &SimpleChatExecutionRequestV1,
    descriptor: &CapabilityDescriptor,
    provider: &StoredProviderBindingV1,
    secret: Option<&StoredSecretBindingV1>,
    tool_bindings: &[StoredFileToolBindingV1],
) -> Result<CompiledSimpleChatGraphV1, SimpleChatPipelineError> {
    validate_simple_chat_workflow_request(request)?;
    let input_id = stable("input.1")?;
    let agent_id = stable("agent.1")?;
    let output_id = stable("output.1")?;
    let wait_id = stable("wait.1")?;
    let model_capability = stable(descriptor.capability_id.as_str())?;
    let source_nodes = request
        .workflow_snapshot
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_workflow("nodes are missing"))?;
    let source = |id: &str| {
        source_nodes
            .iter()
            .find(|node| node.get("id").and_then(Value::as_str) == Some(id))
            .cloned()
            .ok_or_else(|| invalid_workflow("required node is missing"))
    };
    let port = || WorkerPortV1 {
        name: "value".to_owned(),
        schema_ref: None,
        required: true,
    };
    let input_source = source("input.1")?;
    let agent_source = source("agent.1")?;
    let output_source = source("output.1")?;
    let wait_source = source("wait.1")?;
    let nodes = vec![
        WorkerNodeV1 {
            node_id: input_id.clone(),
            node_type: "input".into(),
            node_version: 1,
            contribution_hash: canonical_digest(&input_source)?,
            inputs: Vec::new(),
            outputs: vec![port()],
            executor: WorkerExecutorKindV1::Pure,
            config: json!({"savedNode": input_source}),
            capability_ref: None,
            result_schema_ref: None,
        },
        WorkerNodeV1 {
            node_id: agent_id.clone(),
            node_type: MODEL_NODE_TYPE.into(),
            node_version: 1,
            contribution_hash: canonical_digest(&json!({
                "savedNode": agent_source,
                "modelDescriptor": descriptor.version_hash,
                "tools": tool_bindings,
            }))?,
            inputs: vec![port()],
            outputs: vec![port()],
            executor: WorkerExecutorKindV1::Agent,
            config: json!({
                "savedNode": agent_source,
                "provider": provider,
                "frozenContextHash": request.frozen_context_hash,
                "opaqueSecretRef": secret.map(|binding| binding.opaque_ref.as_str()),
                "secretRevision": secret.map(|binding| binding.revision),
                "tools": tool_bindings,
                "maximumTurns": request.maximum_turns,
            }),
            capability_ref: Some(model_capability),
            result_schema_ref: None,
        },
        WorkerNodeV1 {
            node_id: output_id.clone(),
            node_type: "output".into(),
            node_version: 1,
            contribution_hash: canonical_digest(&output_source)?,
            inputs: vec![port()],
            outputs: vec![port()],
            executor: WorkerExecutorKindV1::Pure,
            config: json!({"savedNode": output_source}),
            capability_ref: None,
            result_schema_ref: None,
        },
        WorkerNodeV1 {
            node_id: wait_id.clone(),
            node_type: "wait".into(),
            node_version: 1,
            contribution_hash: canonical_digest(&wait_source)?,
            inputs: vec![port()],
            outputs: Vec::new(),
            executor: WorkerExecutorKindV1::Wait,
            config: json!({"savedNode": wait_source}),
            capability_ref: None,
            result_schema_ref: None,
        },
    ];
    let transitions = [
        ("input.1", "agent.1", input_id.clone(), agent_id.clone()),
        ("agent.1", "output.1", agent_id.clone(), output_id.clone()),
        ("output.1", "wait.1", output_id, wait_id),
    ]
    .into_iter()
    .map(|(source, target, from_node, to_node)| {
        let edge = request
            .workflow_snapshot
            .get("edges")
            .and_then(Value::as_array)
            .and_then(|edges| {
                edges.iter().find(|edge| {
                    edge.get("source").and_then(Value::as_str) == Some(source)
                        && edge.get("target").and_then(Value::as_str) == Some(target)
                })
            })
            .ok_or_else(|| invalid_workflow("required transition is missing"))?;
        Ok(WorkerTransitionV1 {
            transition_id: stable(
                edge.get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid_workflow("transition ID is missing"))?,
            )?,
            from_node,
            from_port: "value".into(),
            to_node,
            to_port: "value".into(),
            priority: 0,
            predicate: Some(json!({"always":true})),
            declared_loop_id: None,
        })
    })
    .collect::<Result<Vec<_>, SimpleChatPipelineError>>()?;
    Ok(CompiledSimpleChatGraphV1 {
        nodes,
        transitions,
        entry_nodes: vec![input_id],
        model_node_id: agent_id,
    })
}

fn validate_simple_chat_workflow_request(
    request: &SimpleChatExecutionRequestV1,
) -> Result<(), SimpleChatPipelineError> {
    let document = request
        .workflow_snapshot
        .as_object()
        .ok_or_else(|| invalid_workflow("document must be an object"))?;
    if document.get("schemaVersion").and_then(Value::as_u64) != Some(1)
        || document.get("id").and_then(Value::as_str) != Some("workflow.simple-chat")
    {
        return Err(invalid_workflow(
            "schemaVersion 1 and workflow.simple-chat are required",
        ));
    }
    let nodes = document
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_workflow("nodes are missing"))?;
    let edges = document
        .get("edges")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_workflow("edges are missing"))?;
    let required_nodes = [
        ("input.1", "input"),
        ("agent.1", "agent"),
        ("output.1", "output"),
        ("wait.1", "wait"),
    ];
    if nodes.len() != required_nodes.len()
        || required_nodes.iter().any(|(id, kind)| {
            nodes
                .iter()
                .filter(|node| {
                    node.get("id").and_then(Value::as_str) == Some(*id)
                        && node.get("type").and_then(Value::as_str) == Some(*kind)
                })
                .count()
                != 1
        })
    {
        return Err(invalid_workflow(
            "exact Input → Agent → Output → Wait node set is required",
        ));
    }
    let required_edges = [
        ("input.1", "agent.1"),
        ("agent.1", "output.1"),
        ("output.1", "wait.1"),
    ];
    if edges.len() != required_edges.len()
        || required_edges.iter().any(|(source, target)| {
            edges
                .iter()
                .filter(|edge| {
                    edge.get("source").and_then(Value::as_str) == Some(*source)
                        && edge.get("target").and_then(Value::as_str) == Some(*target)
                        && edge
                            .get("id")
                            .and_then(Value::as_str)
                            .is_some_and(|id| StableId::parse(id.to_owned()).is_ok())
                })
                .count()
                != 1
        })
    {
        return Err(invalid_workflow(
            "exact Input → Agent → Output → Wait transitions are required",
        ));
    }
    for node_id in ["input.1", "output.1", "wait.1"] {
        let node = nodes
            .iter()
            .find(|node| node.get("id").and_then(Value::as_str) == Some(node_id))
            .ok_or_else(|| invalid_workflow("required node is missing"))?;
        if let Some(configuration) = node.get("configuration")
            && configuration
                .as_object()
                .is_none_or(|object| !object.is_empty())
        {
            return Err(invalid_workflow(
                "Input, Output, and Wait configuration must be omitted or an empty object",
            ));
        }
    }
    let configuration = nodes
        .iter()
        .find(|node| node.get("id").and_then(Value::as_str) == Some("agent.1"))
        .and_then(|node| node.get("configuration"))
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_workflow("Agent configuration is missing"))?;
    let keys = configuration
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let required_keys = BTreeSet::from(["maxTurns", "modelTierId", "toolIds"]);
    let allowed_keys = BTreeSet::from(["instructions", "maxTurns", "modelTierId", "toolIds"]);
    if !required_keys.is_subset(&keys) || !keys.is_subset(&allowed_keys) {
        return Err(invalid_workflow(
            "Agent configuration accepts exactly modelTierId, toolIds, maxTurns, and optional instructions",
        ));
    }
    if configuration.get("modelTierId").and_then(Value::as_str) != Some("tier:balanced")
        || configuration.get("maxTurns").and_then(Value::as_u64)
            != Some(u64::from(request.maximum_turns))
    {
        return Err(invalid_workflow(
            "Agent modelTierId/maxTurns do not match the frozen execution request",
        ));
    }
    let workflow_tools = configuration
        .get("toolIds")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_workflow("Agent toolIds must be an array"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| invalid_workflow("Agent toolIds must contain only strings"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let request_tools = request
        .tools
        .iter()
        .map(|binding| binding.capability_id.clone())
        .collect::<Vec<_>>();
    if workflow_tools != request_tools {
        return Err(invalid_workflow(
            "Agent toolIds do not match the frozen Settings bindings",
        ));
    }
    let instructions = configuration.get("instructions");
    if instructions.is_some_and(|value| {
        value.as_str().is_none_or(|instructions| {
            instructions.trim().is_empty()
                || instructions.len() > 64 * 1024
                || instructions.contains('\0')
        })
    }) {
        return Err(invalid_workflow(
            "Agent instructions must be a non-empty string of at most 64 KiB",
        ));
    }
    Ok(())
}

fn default_simple_chat_workflow_snapshot() -> Value {
    json!({
        "schemaVersion":1,
        "id":"workflow.simple-chat",
        "name":"Simple Chat",
        "nodes":[
            {"id":"input.1","type":"input"},
            {"id":"agent.1","type":"agent","configuration":{"modelTierId":"tier:balanced","toolIds":[],"maxTurns":1}},
            {"id":"output.1","type":"output"},
            {"id":"wait.1","type":"wait"}
        ],
        "edges":[
            {"id":"input-agent","source":"input.1","target":"agent.1"},
            {"id":"agent-output","source":"agent.1","target":"output.1"},
            {"id":"output-wait","source":"output.1","target":"wait.1"}
        ]
    })
}

const fn default_maximum_turns() -> u32 {
    1
}

fn invalid_workflow(message: &str) -> SimpleChatPipelineError {
    SimpleChatPipelineError::InvalidInput(format!("frozen Simple Chat workflow: {message}"))
}

fn validate_request(
    request: &SimpleChatExecutionRequestV1,
    protocol: ProviderProtocolV1,
    descriptor: &CapabilityDescriptor,
) -> Result<(), SimpleChatPipelineError> {
    validate_simple_chat_workflow_request(request)?;
    let agent_instructions = workflow_agent_instructions(&request.workflow_snapshot);
    let frozen_tools = freeze_file_tool_bindings(&request.tools)?;
    if request.messages.is_empty()
        || request.deadline_epoch_millis <= request.now_epoch_millis
        || request.budget.turns == 0
        || request.budget.attempts == 0
        || request.budget.tokens == 0
        || request.budget.actions == 0
        || request.budget.deadline_ms == 0
    {
        return Err(SimpleChatPipelineError::InvalidInput(
            "messages, deadline, and model budget must be non-empty".to_owned(),
        ));
    }
    if request.tools.is_empty() {
        if request.maximum_turns != 1 || request.budget.turns != 1 || request.budget.tool_calls != 0
        {
            return Err(SimpleChatPipelineError::InvalidInput(
                "tool-free Simple Chat requires exactly one model turn and zero tool calls".into(),
            ));
        }
    } else if request.workspace.is_none()
        || !(2..=8).contains(&request.maximum_turns)
        || request.budget.turns != u64::from(request.maximum_turns)
        || request.budget.attempts < u64::from(request.maximum_turns)
        || request.budget.tool_calls == 0
        || request.budget.tool_calls > 32
        || request.budget.actions
            < request
                .budget
                .turns
                .saturating_add(request.budget.tool_calls)
        || frozen_tools.len() != request.tools.len()
    {
        return Err(SimpleChatPipelineError::InvalidInput(
            "tool-bound Simple Chat requires a frozen project, 2-8 model turns, and matching bounded turn/tool/action budgets"
                .into(),
        ));
    }
    if !is_sha256(&request.frozen_context_hash) {
        return Err(SimpleChatPipelineError::InvalidInput(
            "frozen Chat context hash must be a canonical sha256 identity".to_owned(),
        ));
    }
    if request.project_branch.as_ref().is_some_and(|branch| {
        request.workspace.is_none()
            || branch.trim().is_empty()
            || branch.len() > 1024
            || branch.chars().any(char::is_control)
    }) {
        return Err(SimpleChatPipelineError::InvalidInput(
            "a frozen Git branch requires a project workspace and a bounded non-empty HEAD label"
                .to_owned(),
        ));
    }
    if request.messages.iter().any(|message| {
        !matches!(message.role.as_str(), "system" | "user" | "assistant")
            || message.content.is_empty()
            || message.content.contains('\0')
    }) || request
        .messages
        .last()
        .is_none_or(|message| message.role != "user")
    {
        return Err(SimpleChatPipelineError::InvalidInput(
            "messages require supported roles, non-empty content, and a final user turn".to_owned(),
        ));
    }
    let system_messages = request
        .messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message.role == "system")
        .collect::<Vec<_>>();
    let system_layer_matches = match agent_instructions {
        Some(instructions) => {
            system_messages.len() == 1
                && system_messages[0].0 == 0
                && system_messages[0].1.content == instructions
        }
        None => system_messages.is_empty(),
    };
    if !system_layer_matches {
        return Err(SimpleChatPipelineError::InvalidInput(
            "the provider context must contain exactly the saved Agent instructions as its sole leading system message"
                .to_owned(),
        ));
    }
    if serde_json::to_vec(&request.messages)
        .map_err(json_error)?
        .len()
        > SIMPLE_CHAT_MAX_MESSAGE_CONTEXT_BYTES
    {
        return Err(SimpleChatPipelineError::InvalidInput(
            "message context exceeds the frozen input bound".to_owned(),
        ));
    }
    if serialized_len(&request.workflow_snapshot)? > MAXIMUM_WORKFLOW_SNAPSHOT_BYTES {
        return Err(SimpleChatPipelineError::InvalidInput(
            "saved workflow snapshot exceeds the executable persistence bound".to_owned(),
        ));
    }
    if descriptor.capability_id != protocol.capability_id() {
        return Err(SimpleChatPipelineError::IncompleteEvidence);
    }
    match protocol {
        ProviderProtocolV1::OpenAiCompatible => {
            OpenAiCompatibleProviderConfig::new(
                protocol.capability_id(),
                descriptor.version_hash.clone(),
                &request.provider.base_url,
                request.provider.model.clone(),
                None,
                OpenAiCompatibleLimitsV1::default(),
            )
            .map_err(|error| SimpleChatPipelineError::InvalidInput(error.to_string()))?;
        }
        ProviderProtocolV1::Anthropic => {
            AnthropicMessagesProviderConfig::new(
                protocol.capability_id(),
                descriptor.version_hash.clone(),
                &request.provider.base_url,
                request.provider.model.clone(),
                None,
                AnthropicMessagesLimitsV1::default(),
            )
            .map_err(|error| SimpleChatPipelineError::InvalidInput(error.to_string()))?;
        }
        ProviderProtocolV1::Gemini => {
            GoogleGeminiProviderConfig::new(
                protocol.capability_id(),
                descriptor.version_hash.clone(),
                &request.provider.base_url,
                request.provider.model.clone(),
                None,
                GoogleGeminiLimitsV1::default(),
            )
            .map_err(|error| SimpleChatPipelineError::InvalidInput(error.to_string()))?;
        }
    }
    if let Some(metadata) = &request.provider.credential {
        StoredSecretBindingV1::from_metadata(metadata)?;
    }
    Ok(())
}

fn workflow_agent_instructions(workflow: &Value) -> Option<&str> {
    workflow
        .get("nodes")
        .and_then(Value::as_array)
        .and_then(|nodes| {
            nodes
                .iter()
                .find(|node| node.get("id").and_then(Value::as_str) == Some("agent.1"))
        })
        .and_then(|agent| agent.get("configuration"))
        .and_then(|configuration| configuration.get("instructions"))
        .and_then(Value::as_str)
}

fn model_descriptor(
    protocol: ProviderProtocolV1,
) -> Result<CapabilityDescriptor, SimpleChatPipelineError> {
    let mut descriptor = CapabilityDescriptor::build(
        protocol.capability_id(),
        MODEL_ADAPTER_VERSION,
        CapabilityKind::Model,
        SideEffectClass::NonIdempotent,
    )
    .map_err(|error| SimpleChatPipelineError::Host(error.to_string()))?;
    descriptor.guarantees_same_id_deduplication = false;
    descriptor.supports_streaming = false;
    descriptor.supports_cancellation = false;
    descriptor.allowed_scopes = vec![MODEL_SCOPE.to_owned()];
    descriptor.secret_slots = vec![API_KEY_FIELD.to_owned()];
    descriptor.maximum_concurrency = 8;
    descriptor.max_input_bytes = MAXIMUM_INPUT_BYTES;
    descriptor.max_output_bytes = MAXIMUM_OUTPUT_BYTES;
    descriptor
        .rehash()
        .map_err(|error| SimpleChatPipelineError::Host(error.to_string()))?;
    Ok(descriptor)
}

fn provider_error_status(error: &ProviderError) -> SimpleChatExecutionStatusV1 {
    match error {
        ProviderError::BindingDrift
        | ProviderError::InvalidPlan
        | ProviderError::NoCandidateAccepted => {
            SimpleChatExecutionStatusV1::FailedDefinitelyNotStarted
        }
        ProviderError::OutputBound
        | ProviderError::ConflictingAcceptanceEvidence
        | ProviderError::MissingOrDuplicateUsage => SimpleChatExecutionStatusV1::FailedKnownStarted,
        ProviderError::AcceptanceAmbiguous
        | ProviderError::Failed(_)
        | ProviderError::Cancelled => SimpleChatExecutionStatusV1::OutcomeUncertain,
    }
}

fn revalidate_optional_project_branch(
    workspace: &WorkspaceBindingV1,
    expected_branch: Option<&str>,
) -> Result<(), SimpleChatPipelineError> {
    expected_branch.map_or(Ok(()), |expected| {
        revalidate_git_branch(&workspace.root, expected).map_err(SimpleChatPipelineError::Authority)
    })
}

fn redact_error(
    materialized: &Option<aworkit_capability_host::SecretMaterializationV1>,
    error: &str,
) -> String {
    let redacted = materialized.as_ref().map_or_else(
        || error.to_owned(),
        |secret| secret.redactor().redact(error),
    );
    truncate_utf8(redacted, MAXIMUM_ERROR_BYTES)
}

fn truncate_utf8(mut value: String, maximum_bytes: usize) -> String {
    if value.len() <= maximum_bytes {
        return value;
    }
    let mut end = maximum_bytes;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value.truncate(end);
    value.push('…');
    value
}

fn broker_event_kind(event: &InvocationLedgerEventV1) -> &'static str {
    match event {
        InvocationLedgerEventV1::Proposed { .. } => "broker.proposed",
        InvocationLedgerEventV1::ApprovalRequested(_) => "broker.approval-requested",
        InvocationLedgerEventV1::ApprovalRejected { .. } => "broker.approval-rejected",
        InvocationLedgerEventV1::Authorized(_) => "broker.authorized",
        InvocationLedgerEventV1::DispatchAttempted { .. } => "broker.dispatch-attempted",
        InvocationLedgerEventV1::DispatchAccepted { .. } => "broker.dispatch-accepted",
        InvocationLedgerEventV1::ProgressCommitted { .. } => "broker.progress-committed",
        InvocationLedgerEventV1::Settled { .. } => "broker.settled",
    }
}

fn broker_event_invocation_id(event: &InvocationLedgerEventV1) -> &StableId {
    match event {
        InvocationLedgerEventV1::Proposed { invocation_id, .. }
        | InvocationLedgerEventV1::ApprovalRejected { invocation_id, .. }
        | InvocationLedgerEventV1::DispatchAttempted { invocation_id }
        | InvocationLedgerEventV1::DispatchAccepted { invocation_id }
        | InvocationLedgerEventV1::ProgressCommitted { invocation_id, .. }
        | InvocationLedgerEventV1::Settled { invocation_id, .. } => invocation_id,
        InvocationLedgerEventV1::ApprovalRequested(challenge) => &challenge.invocation_id,
        InvocationLedgerEventV1::Authorized(dispatch) => &dispatch.invocation_id,
    }
}

fn outcome_hash_v1(outcome: &ProviderOutcomeRecordV1) -> Result<String, SimpleChatPipelineError> {
    canonical_hash(outcome)
}

fn canonical_hash<T: Serialize>(value: &T) -> Result<String, SimpleChatPipelineError> {
    let bytes = serde_jcs::to_vec(value).map_err(json_error)?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn canonical_digest<T: Serialize>(value: &T) -> Result<String, SimpleChatPipelineError> {
    let bytes = serde_jcs::to_vec(value).map_err(json_error)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn digest_hex(material: &str) -> String {
    format!("{:x}", Sha256::digest(material.as_bytes()))
}

fn digest_id(prefix: &str, material: &str) -> Result<StableId, SimpleChatPipelineError> {
    stable(&format!("{prefix}.{}", &digest_hex(material)[..40]))
}

fn lease_id(
    invocation_id: &StableId,
    secret: &StoredSecretBindingV1,
) -> Result<StableId, SimpleChatPipelineError> {
    digest_id(
        "lease.model",
        &format!(
            "{}:{}:{}",
            invocation_id.as_str(),
            secret.opaque_ref.as_str(),
            secret.revision
        ),
    )
}

fn stable(value: &str) -> Result<StableId, SimpleChatPipelineError> {
    StableId::parse(value.to_owned())
        .map_err(|error| SimpleChatPipelineError::InvalidInput(error.to_string()))
}

fn random_generation() -> Result<ProcessGeneration, SimpleChatPipelineError> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes)
        .map_err(|_| SimpleChatPipelineError::Host("generation randomness failed".into()))?;
    // Protocol identities are JCS encoded; stay inside JSON's exact integer
    // range while retaining a process-unique generation fence.
    let generation = u64::from_le_bytes(bytes) & ((1_u64 << 53) - 1);
    Ok(ProcessGeneration(generation.max(1)))
}

fn current_epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn map_store_to_broker(error: StoreError) -> BrokerError {
    match error {
        StoreError::DeduplicationKeyReused
        | StoreError::DuplicateEventInBatch
        | StoreError::DuplicateOutboxInBatch => BrokerError::IdentityConflict,
        StoreError::HeadConflict { .. } | StoreError::AggregateVersionConflict { .. } => {
            BrokerError::CommitFailed
        }
        _ => BrokerError::Unavailable,
    }
}

fn local_store_error(error: StoreError) -> SimpleChatPipelineError {
    SimpleChatPipelineError::Store(error.to_string())
}

fn store_error(error: std::io::Error) -> SimpleChatPipelineError {
    SimpleChatPipelineError::Store(error.to_string())
}

fn json_error(error: impl std::fmt::Display) -> SimpleChatPipelineError {
    SimpleChatPipelineError::Store(error.to_string())
}

fn broker_error(error: BrokerError) -> SimpleChatPipelineError {
    SimpleChatPipelineError::Broker(error.to_string())
}

#[cfg(test)]
mod tests {
    use crate::runtime::{
        PROJECT_FILE_READ_MAXIMUM_BYTES_V1, PROJECT_FILE_SEARCH_MAXIMUM_RESULTS_V1,
    };

    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::runtime::tool_loop::{FILE_READ_CAPABILITY_ID, FILE_SEARCH_CAPABILITY_ID};
    use aworkit_capability_host::{
        ModelToolCallV1, ModelToolEventV1, ModelToolRequestV1, ProviderAcceptanceV1,
    };
    use aworkit_trusted_core::{MemoryCredentialStore, SecretBroker};
    use tempfile::TempDir;

    use super::*;

    type ToolPipelineSetupV1 = (
        SimpleChatExecutionPipeline,
        Arc<MemoryCredentialStore>,
        CredentialMetadataV1,
        Arc<AtomicUsize>,
        Arc<Mutex<Vec<Value>>>,
    );

    #[derive(Clone, Copy)]
    enum ScriptedBehavior {
        Succeed,
        Ambiguous,
        EmptyAcceptedOutput,
        OversizedOutput,
    }

    struct ScriptedProviderFactory {
        calls: Arc<AtomicUsize>,
        behavior: ScriptedBehavior,
        saw_secret: Arc<Mutex<bool>>,
        observed_inputs: Option<Arc<Mutex<Vec<Value>>>>,
    }

    impl ProviderFactoryV1 for ScriptedProviderFactory {
        fn create(
            &self,
            descriptor: &CapabilityDescriptor,
            _provider: &StoredProviderBindingV1,
            api_key: Option<Zeroizing<String>>,
        ) -> Result<Box<dyn ProviderEnginePortV1>, String> {
            *self.saw_secret.lock().expect("secret observation") =
                api_key.as_ref().map(|value| value.as_str()) == Some("test-secret");
            Ok(Box::new(ScriptedProvider {
                calls: self.calls.clone(),
                binding: descriptor.capability_id.clone(),
                version: descriptor.version_hash.clone(),
                behavior: self.behavior,
                observed_inputs: self.observed_inputs.clone(),
            }))
        }
    }

    struct ScriptedProvider {
        calls: Arc<AtomicUsize>,
        binding: String,
        version: String,
        behavior: ScriptedBehavior,
        observed_inputs: Option<Arc<Mutex<Vec<Value>>>>,
    }

    impl ProviderEnginePortV1 for ScriptedProvider {
        fn binding_id(&self) -> &str {
            &self.binding
        }

        fn version_hash(&self) -> &str {
            &self.version
        }

        fn execute(
            &self,
            request: &ModelRequestV1,
            emit: &mut dyn FnMut(ModelEventV1) -> Result<(), ProviderError>,
        ) -> Result<aworkit_capability_host::ProviderAcceptanceV1, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(observed) = &self.observed_inputs {
                observed
                    .lock()
                    .expect("observed provider input")
                    .push(request.input.clone());
            }
            match self.behavior {
                ScriptedBehavior::Ambiguous => {
                    return Err(ProviderError::AcceptanceAmbiguous);
                }
                ScriptedBehavior::EmptyAcceptedOutput => {
                    emit(ModelEventV1::Usage {
                        input_tokens: 7,
                        output_tokens: 0,
                    })?;
                    return Ok(aworkit_capability_host::ProviderAcceptanceV1::Accepted);
                }
                ScriptedBehavior::OversizedOutput => {
                    emit(ModelEventV1::AssistantOutput(
                        "x".repeat(SIMPLE_CHAT_MAX_ASSISTANT_TEXT_BYTES + 1),
                    ))?;
                    emit(ModelEventV1::Usage {
                        input_tokens: 7,
                        output_tokens: 100_000,
                    })?;
                    return Ok(aworkit_capability_host::ProviderAcceptanceV1::Accepted);
                }
                ScriptedBehavior::Succeed => {}
            }
            emit(ModelEventV1::AssistantOutput("working answer".to_owned()))?;
            emit(ModelEventV1::Usage {
                input_tokens: 7,
                output_tokens: 3,
            })?;
            Ok(aworkit_capability_host::ProviderAcceptanceV1::Accepted)
        }
    }

    #[derive(Clone, Copy)]
    enum ToolScriptV1 {
        ReadAndSearch,
        LargeAggregate,
        Escape,
        Malformed,
        ReadThenProviderFailure,
    }

    struct ToolProviderFactoryV1 {
        calls: Arc<AtomicUsize>,
        script: ToolScriptV1,
        observed_results: Arc<Mutex<Vec<Value>>>,
    }

    impl ProviderFactoryV1 for ToolProviderFactoryV1 {
        fn create(
            &self,
            descriptor: &CapabilityDescriptor,
            _provider: &StoredProviderBindingV1,
            _api_key: Option<Zeroizing<String>>,
        ) -> Result<Box<dyn ProviderEnginePortV1>, String> {
            Ok(Box::new(ToolProviderV1 {
                calls: self.calls.clone(),
                script: self.script,
                observed_results: self.observed_results.clone(),
                binding: descriptor.capability_id.clone(),
                version: descriptor.version_hash.clone(),
            }))
        }
    }

    struct ToolProviderV1 {
        calls: Arc<AtomicUsize>,
        script: ToolScriptV1,
        observed_results: Arc<Mutex<Vec<Value>>>,
        binding: String,
        version: String,
    }

    impl ProviderEnginePortV1 for ToolProviderV1 {
        fn binding_id(&self) -> &str {
            &self.binding
        }

        fn version_hash(&self) -> &str {
            &self.version
        }

        fn execute(
            &self,
            _request: &ModelRequestV1,
            _emit: &mut dyn FnMut(ModelEventV1) -> Result<(), ProviderError>,
        ) -> Result<ProviderAcceptanceV1, ProviderError> {
            Err(ProviderError::Failed(
                "tool test provider requires the tool-turn API".into(),
            ))
        }

        fn execute_tool_turn_cancellable(
            &self,
            request: &ModelToolRequestV1,
            cancellation: &CancellationToken,
            emit: &mut dyn FnMut(ModelToolEventV1) -> Result<(), ProviderError>,
        ) -> Result<ProviderAcceptanceV1, ProviderError> {
            if cancellation.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            if request.exchanges.is_empty() {
                match self.script {
                    ToolScriptV1::ReadAndSearch => {
                        emit(tool_call(
                            "call.read",
                            FILE_READ_CAPABILITY_ID,
                            "aworkit_read_project_file",
                            json!({"path":"notes.txt"}),
                        ))?;
                        emit(tool_call(
                            "call.search",
                            FILE_SEARCH_CAPABILITY_ID,
                            "aworkit_search_project_file",
                            json!({"path":"notes.txt","query":"alpha"}),
                        ))?;
                    }
                    ToolScriptV1::LargeAggregate => emit(tool_call(
                        "call.large-one",
                        FILE_READ_CAPABILITY_ID,
                        "aworkit_read_project_file",
                        json!({"path":"large.txt"}),
                    ))?,
                    ToolScriptV1::Escape => emit(tool_call(
                        "call.escape",
                        FILE_READ_CAPABILITY_ID,
                        "aworkit_read_project_file",
                        json!({"path":"../outside.txt"}),
                    ))?,
                    ToolScriptV1::Malformed => emit(tool_call(
                        "call.malformed",
                        FILE_READ_CAPABILITY_ID,
                        "aworkit_read_project_file",
                        json!({"path":"notes.txt","unknown":true}),
                    ))?,
                    ToolScriptV1::ReadThenProviderFailure => emit(tool_call(
                        "call.before-failure",
                        FILE_READ_CAPABILITY_ID,
                        "aworkit_read_project_file",
                        json!({"path":"notes.txt"}),
                    ))?,
                }
                emit(ModelToolEventV1::Usage {
                    input_tokens: 5,
                    output_tokens: 2,
                })?;
                return Ok(ProviderAcceptanceV1::Accepted);
            }
            if matches!(self.script, ToolScriptV1::LargeAggregate) && request.exchanges.len() == 1 {
                emit(tool_call(
                    "call.large-two",
                    FILE_READ_CAPABILITY_ID,
                    "aworkit_read_project_file",
                    json!({"path":"large.txt"}),
                ))?;
                emit(ModelToolEventV1::Usage {
                    input_tokens: 9,
                    output_tokens: 2,
                })?;
                return Ok(ProviderAcceptanceV1::Accepted);
            }
            let results = request
                .exchanges
                .last()
                .expect("tool exchange")
                .results
                .iter()
                .map(|result| result.content.clone())
                .collect::<Vec<_>>();
            self.observed_results
                .lock()
                .expect("observed tool results")
                .extend(results);
            if matches!(self.script, ToolScriptV1::ReadThenProviderFailure) {
                return Err(ProviderError::Failed(
                    "provider failed after one settled tool call".into(),
                ));
            }
            emit(ModelToolEventV1::AssistantOutput {
                text: "tool loop complete".into(),
            })?;
            emit(ModelToolEventV1::Usage {
                input_tokens: 9,
                output_tokens: 4,
            })?;
            Ok(ProviderAcceptanceV1::Accepted)
        }
    }

    fn tool_call(
        call_id: &str,
        capability_id: &str,
        name: &str,
        arguments: Value,
    ) -> ModelToolEventV1 {
        ModelToolEventV1::ToolCall {
            call: ModelToolCallV1 {
                call_id: call_id.into(),
                provider_call_id: Some(call_id.into()),
                capability_id: capability_id.into(),
                name: name.into(),
                arguments,
                provider_context: None,
            },
        }
    }

    fn setup(
        root: &TempDir,
        behavior: ScriptedBehavior,
    ) -> (
        SimpleChatExecutionPipeline,
        Arc<MemoryCredentialStore>,
        CredentialMetadataV1,
        Arc<AtomicUsize>,
        Arc<Mutex<bool>>,
    ) {
        let credential_store = Arc::new(MemoryCredentialStore::default());
        let mut secret_broker = SecretBroker::with_store(credential_store.clone());
        let metadata = secret_broker
            .put_credential(
                CredentialRef(stable("credential.pipeline-test").expect("credential ID")),
                BTreeMap::from([(API_KEY_FIELD.to_owned(), b"test-secret".to_vec())]),
            )
            .expect("credential");
        let calls = Arc::new(AtomicUsize::new(0));
        let saw_secret = Arc::new(Mutex::new(false));
        let pipeline = SimpleChatExecutionPipeline::compose(
            root.path(),
            credential_store.clone(),
            Arc::new(ScriptedProviderFactory {
                calls: calls.clone(),
                behavior,
                saw_secret: saw_secret.clone(),
                observed_inputs: None,
            }),
        )
        .expect("pipeline");
        (pipeline, credential_store, metadata, calls, saw_secret)
    }

    fn request(metadata: CredentialMetadataV1) -> SimpleChatExecutionRequestV1 {
        SimpleChatExecutionRequestV1::bounded(
            stable("command.pipeline-test").expect("request"),
            stable("chat.pipeline-test").expect("chat"),
            stable("run.pipeline-test").expect("run"),
            SimpleChatProviderBindingV1 {
                kind: "openai_compatible".to_owned(),
                base_url: "http://127.0.0.1:9876/v1".to_owned(),
                model: "test-model".to_owned(),
                credential: Some(metadata),
            },
            vec![SimpleChatMessageV1 {
                role: "user".to_owned(),
                content: "Please prove the pipeline works.".to_owned(),
            }],
            current_epoch_millis(),
        )
    }

    fn setup_tool_pipeline(root: &TempDir, script: ToolScriptV1) -> ToolPipelineSetupV1 {
        let credential_store = Arc::new(MemoryCredentialStore::default());
        let mut secret_broker = SecretBroker::with_store(credential_store.clone());
        let metadata = secret_broker
            .put_credential(
                CredentialRef(stable("credential.tool-pipeline-test").expect("credential ID")),
                BTreeMap::from([(API_KEY_FIELD.to_owned(), b"test-secret".to_vec())]),
            )
            .expect("credential");
        let calls = Arc::new(AtomicUsize::new(0));
        let observed_results = Arc::new(Mutex::new(Vec::new()));
        let pipeline = SimpleChatExecutionPipeline::compose(
            root.path(),
            credential_store.clone(),
            Arc::new(ToolProviderFactoryV1 {
                calls: calls.clone(),
                script,
                observed_results: observed_results.clone(),
            }),
        )
        .expect("tool pipeline");
        (
            pipeline,
            credential_store,
            metadata,
            calls,
            observed_results,
        )
    }

    fn tool_bound_request(
        pipeline: &SimpleChatExecutionPipeline,
        metadata: CredentialMetadataV1,
        project: &Path,
        tool_ids: &[&str],
    ) -> SimpleChatExecutionRequestV1 {
        let mut request = request(metadata);
        request.request_id = stable("command.pipeline-tool-test").expect("request");
        request.chat_id = stable("chat.pipeline-tool-test").expect("chat");
        request.run_id = stable("run.pipeline-tool-test").expect("run");
        request.workspace = Some(
            pipeline
                .projects
                .resolve_workspace_v1(project)
                .expect("workspace"),
        );
        request.maximum_turns = 2;
        request.budget.turns = 2;
        request.budget.attempts = 2;
        request.budget.tool_calls = 8;
        request.budget.actions = 10;
        request.tools = tool_ids
            .iter()
            .map(|tool_id| SimpleChatToolBindingV1 {
                capability_id: (*tool_id).into(),
                configuration: match *tool_id {
                    FILE_READ_CAPABILITY_ID => json!({
                        "authorityMode":"project_files",
                        "effect":"read",
                        "maximumBytes":PROJECT_FILE_READ_MAXIMUM_BYTES_V1,
                    }),
                    FILE_SEARCH_CAPABILITY_ID => json!({
                        "authorityMode":"project_files",
                        "effect":"search",
                        "maximumResults":PROJECT_FILE_SEARCH_MAXIMUM_RESULTS_V1,
                    }),
                    _ => json!({}),
                },
            })
            .collect();
        request.workflow_snapshot = default_simple_chat_workflow_snapshot();
        request.workflow_snapshot["nodes"][1]["configuration"]["toolIds"] = json!(tool_ids);
        request.workflow_snapshot["nodes"][1]["configuration"]["maxTurns"] = json!(2);
        request
    }

    #[test]
    fn provider_tool_calls_cross_frozen_authority_read_search_and_restart_without_replay() {
        let root = TempDir::new().expect("root");
        let project = root.path().join("project");
        fs::create_dir(&project).expect("project");
        fs::create_dir(project.join(".git")).expect("git metadata");
        fs::write(project.join(".git/HEAD"), b"ref: refs/heads/main\n").expect("Git HEAD");
        fs::write(project.join("notes.txt"), b"alpha beta alpha").expect("notes");
        let (pipeline, credential_store, metadata, calls, observed_results) =
            setup_tool_pipeline(&root, ToolScriptV1::ReadAndSearch);
        let mut execution_request = tool_bound_request(
            &pipeline,
            metadata.clone(),
            &project,
            &[FILE_READ_CAPABILITY_ID, FILE_SEARCH_CAPABILITY_ID],
        );
        execution_request.project_branch = Some("main".into());

        let first = pipeline
            .execute(execution_request.clone())
            .expect("tool execution");
        assert_eq!(
            first.status,
            SimpleChatExecutionStatusV1::Succeeded,
            "{:?}",
            first.error
        );
        assert_eq!(first.assistant_text.as_deref(), Some("tool loop complete"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!((first.model_turns, first.tool_calls), (2, 2));
        assert_eq!(first.tool_activity.len(), 2);
        assert_eq!(
            first.tool_activity[0].capability_id,
            FILE_READ_CAPABILITY_ID
        );
        assert_eq!(
            first.tool_activity[1].capability_id,
            FILE_SEARCH_CAPABILITY_ID
        );
        assert!(
            first
                .tool_activity
                .iter()
                .all(|activity| activity.status == "completed")
        );
        let observed = observed_results.lock().expect("tool results");
        assert_eq!(observed[0]["content"], "alpha beta alpha");
        assert_eq!(observed[1]["offsets"], json!([0, 11]));
        drop(observed);
        let prepared = pipeline
            .records
            .execution(&execution_request.request_id)
            .expect("execution record")
            .expect("prepared");
        assert_eq!(
            prepared
                .snapshot
                .nodes
                .iter()
                .map(|node| node.node_type.as_str())
                .collect::<Vec<_>>(),
            vec!["input", "agent", "output", "wait"]
        );
        assert_eq!(prepared.snapshot.transitions.len(), 3);
        assert_eq!(prepared.manifest.capability_bindings.len(), 3);
        assert_eq!(prepared.tool_bindings.len(), 2);
        let prepared_scheduler = prepared
            .scheduler_checkpoint
            .as_ref()
            .expect("prepared scheduler checkpoint");
        assert_eq!(prepared_scheduler.committed_cursor, 1);
        assert_eq!(
            prepared
                .scheduler_trace
                .iter()
                .map(|entry| (entry.action.as_str(), entry.node_id.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("claimed", "input.1"),
                ("transition_acknowledged", "input.1"),
                ("claimed", "agent.1"),
            ]
        );
        let durable = pipeline
            .records
            .outcomes()
            .expect("durable outcomes")
            .into_iter()
            .find(|outcome| outcome.status == SimpleChatExecutionStatusV1::Succeeded)
            .expect("successful outcome");
        let final_scheduler = durable
            .scheduler_checkpoint
            .as_ref()
            .expect("final scheduler checkpoint");
        assert_eq!(final_scheduler.committed_cursor, 3);
        assert_eq!(durable.scheduler_trace.len(), 8);
        assert_eq!(
            durable
                .scheduler_trace
                .iter()
                .filter_map(|entry| entry.transition_id.as_ref().map(StableId::as_str))
                .collect::<Vec<_>>(),
            vec!["input-agent", "agent-output", "output-wait"]
        );
        assert!(final_scheduler.tokens.iter().any(|token| {
            token.node_id.as_str() == "wait.1" && token.state == TokenStateV1::Suspended
        }));

        fs::write(project.join("notes.txt"), b"changed after settlement").expect("change");
        fs::write(
            project.join(".git/HEAD"),
            b"ref: refs/heads/drifted-after-settlement\n",
        )
        .expect("drift branch after settlement");
        fs::rename(&project, root.path().join("project-moved-after-settlement"))
            .expect("make frozen root unavailable after settlement");
        drop(pipeline);
        let restarted_calls = Arc::new(AtomicUsize::new(0));
        let restarted_results = Arc::new(Mutex::new(Vec::new()));
        let restarted = SimpleChatExecutionPipeline::compose(
            root.path(),
            credential_store,
            Arc::new(ToolProviderFactoryV1 {
                calls: restarted_calls.clone(),
                script: ToolScriptV1::ReadAndSearch,
                observed_results: restarted_results.clone(),
            }),
        )
        .expect("restart");
        restarted
            .preflight(&execution_request)
            .expect("settled retry preflight ignores later workspace/branch drift");
        let replay = restarted
            .execute(execution_request.clone())
            .expect("durable replay");
        assert!(replay.replayed);
        assert_eq!(replay.assistant_text.as_deref(), Some("tool loop complete"));
        assert_eq!(restarted_calls.load(Ordering::SeqCst), 0);
        assert!(restarted_results.lock().expect("results").is_empty());

        let mut follow_up = execution_request;
        follow_up.request_id = stable("command.pipeline-tool-follow-up").expect("follow-up");
        follow_up.messages.extend([
            SimpleChatMessageV1 {
                role: "assistant".into(),
                content: "tool loop complete".into(),
            },
            SimpleChatMessageV1 {
                role: "user".into(),
                content: "read it again".into(),
            },
        ]);
        assert!(matches!(
            restarted.preflight(&follow_up),
            Err(SimpleChatPipelineError::Authority(_))
        ));
        assert_eq!(restarted_calls.load(Ordering::SeqCst), 0);
        assert!(
            restarted
                .records
                .execution(&follow_up.request_id)
                .expect("follow-up record lookup")
                .is_none()
        );
    }

    #[test]
    fn project_tool_root_malformed_and_missing_scope_fail_closed() {
        let root = TempDir::new().expect("root");
        let project = root.path().join("project");
        fs::create_dir(&project).expect("project");
        fs::write(project.join("notes.txt"), b"inside").expect("notes");
        fs::write(root.path().join("outside.txt"), b"outside secret").expect("outside");
        let (pipeline, _store, metadata, calls, observed_results) =
            setup_tool_pipeline(&root, ToolScriptV1::Escape);
        let escaped = tool_bound_request(
            &pipeline,
            metadata.clone(),
            &project,
            &[FILE_READ_CAPABILITY_ID],
        );
        let result = pipeline.execute(escaped).expect("settled denied read");
        assert_eq!(
            result.status,
            SimpleChatExecutionStatusV1::Succeeded,
            "{:?}",
            result.error
        );
        assert_eq!(result.tool_activity.len(), 1, "{:?}", result.error);
        assert_eq!(result.tool_activity[0].status, "failed");
        let encoded = serde_json::to_string(&*observed_results.lock().expect("result"))
            .expect("encode result");
        assert!(!encoded.contains("outside secret"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        let malformed_calls = Arc::new(AtomicUsize::new(0));
        let malformed = SimpleChatExecutionPipeline::compose(
            &root.path().join("malformed-profile"),
            Arc::new(MemoryCredentialStore::default()),
            Arc::new(ToolProviderFactoryV1 {
                calls: malformed_calls.clone(),
                script: ToolScriptV1::Malformed,
                observed_results: Arc::new(Mutex::new(Vec::new())),
            }),
        )
        .expect("malformed pipeline");
        let mut malformed_request = tool_bound_request(
            &malformed,
            metadata.clone(),
            &project,
            &[FILE_READ_CAPABILITY_ID],
        );
        malformed_request.provider.credential = None;
        let denied = malformed
            .execute(malformed_request)
            .expect("malformed call is a durable provider outcome");
        assert_eq!(
            denied.status,
            SimpleChatExecutionStatusV1::FailedKnownStarted
        );
        assert!(denied.tool_activity.is_empty());
        assert_eq!(malformed_calls.load(Ordering::SeqCst), 1);

        let mut missing_scope =
            tool_bound_request(&pipeline, metadata, &project, &[FILE_READ_CAPABILITY_ID]);
        missing_scope.request_id = stable("command.pipeline-tool-no-project").expect("request");
        missing_scope.chat_id = stable("chat.pipeline-tool-no-project").expect("chat");
        missing_scope.run_id = stable("run.pipeline-tool-no-project").expect("run");
        missing_scope.workspace = None;
        assert!(matches!(
            pipeline.execute(missing_scope),
            Err(SimpleChatPipelineError::InvalidInput(_))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn settled_tool_activity_survives_a_later_provider_failure() {
        let root = TempDir::new().expect("root");
        let project = root.path().join("project");
        fs::create_dir(&project).expect("project");
        fs::write(project.join("notes.txt"), b"durable evidence").expect("notes");
        let (pipeline, _store, metadata, calls, _results) =
            setup_tool_pipeline(&root, ToolScriptV1::ReadThenProviderFailure);
        let result = pipeline
            .execute(tool_bound_request(
                &pipeline,
                metadata,
                &project,
                &[FILE_READ_CAPABILITY_ID],
            ))
            .expect("later provider failure");
        assert_eq!(result.status, SimpleChatExecutionStatusV1::OutcomeUncertain);
        assert_eq!(result.tool_activity.len(), 1, "{:?}", result.error);
        assert_eq!(result.tool_activity[0].status, "completed");
        assert_eq!((result.model_turns, result.tool_calls), (2, 1));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let durable = pipeline
            .records
            .outcomes()
            .expect("outcomes")
            .into_iter()
            .next()
            .expect("failure outcome");
        assert!(
            durable
                .scheduler_checkpoint
                .as_ref()
                .expect("failure scheduler")
                .tokens
                .iter()
                .any(|token| token.node_id.as_str() == "agent.1"
                    && token.state == TokenStateV1::Cancelled)
        );
    }

    #[test]
    fn aggregate_tool_history_bound_fails_explicitly_without_journal_failure() {
        let root = TempDir::new().expect("root");
        let project = root.path().join("project");
        fs::create_dir(&project).expect("project");
        // Each byte is valid UTF-8 but expands to a six-byte JSON escape. One
        // canonical 60 KiB read fits; two exchanges exceed the 512 KiB run
        // aggregate and must fail explicitly before journal commit.
        fs::write(project.join("large.txt"), "\u{1}".repeat(60 * 1024)).expect("large file");
        let (pipeline, _store, metadata, calls, _results) =
            setup_tool_pipeline(&root, ToolScriptV1::LargeAggregate);
        let mut execution_request =
            tool_bound_request(&pipeline, metadata, &project, &[FILE_READ_CAPABILITY_ID]);
        execution_request.maximum_turns = 3;
        execution_request.budget.turns = 3;
        execution_request.budget.attempts = 3;
        execution_request.budget.actions = 11;
        execution_request.workflow_snapshot["nodes"][1]["configuration"]["maxTurns"] = json!(3);

        let result = pipeline
            .execute(execution_request)
            .expect("bounded failure remains durably representable");
        assert_eq!(
            result.status,
            SimpleChatExecutionStatusV1::FailedKnownStarted
        );
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|error| error.contains("history byte limit"))
        );
        assert_eq!((result.model_turns, result.tool_calls), (2, 2));
        assert_eq!(result.tool_activity.len(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let outcome = pipeline
            .records
            .outcomes()
            .expect("outcomes")
            .into_iter()
            .next()
            .expect("outcome");
        assert!(serialized_len(&outcome).unwrap() <= MAXIMUM_PROVIDER_OUTCOME_BYTES);
        assert_eq!(outcome.tool_exchanges.len(), 1);
    }

    #[test]
    fn oversized_input_and_output_are_rejected_or_settled_before_store_limits() {
        let root = TempDir::new().expect("root");
        let (pipeline, _store, metadata, calls, _) =
            setup(&root, ScriptedBehavior::OversizedOutput);
        let output = pipeline
            .execute(request(metadata.clone()))
            .expect("oversized provider output is a bounded durable outcome");
        assert_eq!(
            output.status,
            SimpleChatExecutionStatusV1::FailedKnownStarted
        );
        assert_eq!((output.model_turns, output.tool_calls), (1, 0));
        assert!(output.assistant_text.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let mut oversized_input = request(metadata);
        oversized_input.request_id = stable("command.oversized-input").expect("request");
        oversized_input.chat_id = stable("chat.oversized-input").expect("chat");
        oversized_input.run_id = stable("run.oversized-input").expect("run");
        oversized_input.messages = (0..5)
            .flat_map(|ordinal| {
                [
                    SimpleChatMessageV1 {
                        role: "user".into(),
                        content: format!("{ordinal}:{}", "u".repeat(31 * 1024)),
                    },
                    SimpleChatMessageV1 {
                        role: "assistant".into(),
                        content: "a".repeat(31 * 1024),
                    },
                ]
            })
            .chain(std::iter::once(SimpleChatMessageV1 {
                role: "user".into(),
                content: "final".into(),
            }))
            .collect();
        assert!(matches!(
            pipeline.execute(oversized_input.clone()),
            Err(SimpleChatPipelineError::InvalidInput(_))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            pipeline
                .records
                .execution(&oversized_input.request_id)
                .expect("execution lookup")
                .is_none()
        );
    }

    #[test]
    fn git_head_drift_is_rejected_before_proposal_or_provider_effect() {
        let root = TempDir::new().expect("root");
        let project = root.path().join("git-project");
        fs::create_dir_all(project.join(".git")).expect("git metadata");
        fs::write(
            project.join(".git/HEAD"),
            b"ref: refs/heads/feature/frozen\n",
        )
        .expect("HEAD");
        let (pipeline, _store, metadata, calls, _) = setup(&root, ScriptedBehavior::Succeed);
        let mut execution_request = request(metadata);
        execution_request.request_id = stable("command.branch-drift").expect("request");
        execution_request.chat_id = stable("chat.branch-drift").expect("chat");
        execution_request.run_id = stable("run.branch-drift").expect("run");
        execution_request.workspace = Some(
            pipeline
                .projects
                .resolve_workspace_v1(&project)
                .expect("workspace"),
        );
        execution_request.project_branch = Some("feature/frozen".into());
        fs::write(
            project.join(".git/HEAD"),
            b"ref: refs/heads/feature/drifted\n",
        )
        .expect("switch branch");

        assert!(matches!(
            pipeline.execute(execution_request.clone()),
            Err(SimpleChatPipelineError::Authority(_))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(
            pipeline
                .records
                .execution(&execution_request.request_id)
                .expect("execution lookup")
                .is_none()
        );
    }

    #[test]
    fn simple_chat_crosses_authority_host_secret_and_durable_settlement_once() {
        let root = TempDir::new().expect("root");
        let (pipeline, credential_store, metadata, calls, saw_secret) =
            setup(&root, ScriptedBehavior::Succeed);
        let first = pipeline
            .execute(request(metadata.clone()))
            .expect("execute");
        assert_eq!(first.status, SimpleChatExecutionStatusV1::Succeeded);
        assert_eq!(first.assistant_text.as_deref(), Some("working answer"));
        assert_eq!((first.input_units, first.output_units), (7, 3));
        assert_eq!((first.model_turns, first.tool_calls), (1, 0));
        assert!(!first.replayed);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(*saw_secret.lock().expect("secret observation"));
        let prepared = pipeline
            .records
            .execution(&first.request_id)
            .expect("stored execution")
            .expect("prepared execution");
        assert_eq!(
            prepared
                .snapshot
                .nodes
                .iter()
                .map(|node| node.node_type.as_str())
                .collect::<Vec<_>>(),
            vec!["input", "agent", "output", "wait"]
        );
        assert_eq!(prepared.snapshot.transitions.len(), 3);
        assert_eq!(prepared.manifest.capability_bindings.len(), 1);
        assert!(prepared.tool_bindings.is_empty());
        assert!(
            prepared
                .workspace
                .as_ref()
                .is_some_and(|workspace| workspace.root.ends_with("unscoped-workspace"))
        );
        for entry in fs::read_dir(root.path().join("history")).expect("history directory") {
            let path = entry.expect("history entry").path();
            if path.is_file() {
                let bytes = fs::read(path).expect("durable pipeline file");
                assert!(
                    !bytes
                        .windows(b"test-secret".len())
                        .any(|window| window == b"test-secret"),
                    "plaintext credential reached durable pipeline storage"
                );
            }
        }

        let replay = pipeline.execute(request(metadata.clone())).expect("replay");
        assert_eq!(replay.status, SimpleChatExecutionStatusV1::Succeeded);
        assert!(replay.replayed);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let reopened_calls = Arc::new(AtomicUsize::new(0));
        let reopened = SimpleChatExecutionPipeline::compose(
            root.path(),
            credential_store,
            Arc::new(ScriptedProviderFactory {
                calls: reopened_calls.clone(),
                behavior: ScriptedBehavior::Succeed,
                saw_secret: Arc::new(Mutex::new(false)),
                observed_inputs: None,
            }),
        )
        .expect("reopen");
        let after_restart = reopened.execute(request(metadata)).expect("restart replay");
        assert_eq!(after_restart.status, SimpleChatExecutionStatusV1::Succeeded);
        assert!(after_restart.replayed);
        assert_eq!(reopened_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn provider_protocol_selects_an_exact_frozen_descriptor_and_adapter() {
        let root = TempDir::new().expect("root");
        let (pipeline, _credential_store, metadata, calls, _) =
            setup(&root, ScriptedBehavior::Succeed);
        let cases = [
            (
                "openai_compatible",
                "http://127.0.0.1:9876/v1",
                "model.openai-compatible",
                "adapter.openai-compatible",
            ),
            (
                "anthropic",
                "http://127.0.0.1:9876",
                "model.anthropic-messages",
                "adapter.anthropic-messages",
            ),
            (
                "gemini",
                "http://127.0.0.1:9876",
                "model.google-gemini",
                "adapter.google-gemini",
            ),
        ];
        let mut descriptor_hashes = BTreeSet::new();
        for (ordinal, (kind, base_url, capability_id, adapter_id)) in cases.into_iter().enumerate()
        {
            let mut execution_request = request(metadata.clone());
            execution_request.request_id = stable(&format!("command.pipeline-protocol-{ordinal}"))
                .expect("protocol request ID");
            execution_request.chat_id =
                stable(&format!("chat.pipeline-protocol-{ordinal}")).expect("protocol Chat ID");
            execution_request.run_id =
                stable(&format!("run.pipeline-protocol-{ordinal}")).expect("protocol Run ID");
            execution_request.provider.kind = kind.to_owned();
            execution_request.provider.base_url = base_url.to_owned();

            let result = pipeline
                .execute(execution_request.clone())
                .expect("protocol execution");
            assert_eq!(result.status, SimpleChatExecutionStatusV1::Succeeded);

            let record = pipeline
                .records
                .execution(&execution_request.request_id)
                .expect("stored execution")
                .expect("prepared execution");
            let binding = record
                .manifest
                .capability_bindings
                .first()
                .expect("frozen provider binding");
            assert_eq!(record.provider.kind, kind);
            assert_eq!(record.broker_proposal.capability_id.as_str(), capability_id);
            assert_eq!(binding.capability_id.as_str(), capability_id);
            assert_eq!(binding.adapter_id.as_str(), adapter_id);
            let protocol = ProviderProtocolV1::parse(kind).expect("installed protocol");
            let descriptor = pipeline
                .descriptors
                .get(&protocol)
                .expect("registered descriptor");
            assert_eq!(binding.descriptor_hash, descriptor.version_hash);
            descriptor_hashes.insert(binding.descriptor_hash.clone());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(descriptor_hashes.len(), 3);

        let mut changed_protocol = request(metadata);
        changed_protocol.request_id =
            stable("command.pipeline-protocol-0").expect("protocol request ID");
        changed_protocol.chat_id = stable("chat.pipeline-protocol-0").expect("protocol Chat ID");
        changed_protocol.run_id = stable("run.pipeline-protocol-0").expect("protocol Run ID");
        changed_protocol.provider.kind = "anthropic".to_owned();
        changed_protocol.provider.base_url = "http://127.0.0.1:9876".to_owned();
        assert!(matches!(
            pipeline.execute(changed_protocol),
            Err(SimpleChatPipelineError::Store(_))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn follow_up_reuses_one_frozen_run_and_changed_authority_fails_before_effect() {
        let root = TempDir::new().expect("root");
        let (pipeline, credential_store, metadata, calls, _) =
            setup(&root, ScriptedBehavior::Succeed);
        let mut first_request = request(metadata.clone());
        first_request.frozen_context_hash = format!("sha256:{}", "a".repeat(64));
        let first = pipeline.execute(first_request.clone()).expect("first turn");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        drop(pipeline);

        let mut follow_up = request(metadata.clone());
        follow_up.request_id = stable("command.pipeline-follow-up").expect("follow-up request");
        follow_up.frozen_context_hash = first_request.frozen_context_hash.clone();
        follow_up.messages = vec![
            SimpleChatMessageV1 {
                role: "user".into(),
                content: "Please prove the pipeline works.".into(),
            },
            SimpleChatMessageV1 {
                role: "assistant".into(),
                content: "working answer".into(),
            },
            SimpleChatMessageV1 {
                role: "user".into(),
                content: "follow up".into(),
            },
        ];
        let continuation_calls = Arc::new(AtomicUsize::new(0));
        let pipeline = SimpleChatExecutionPipeline::compose(
            root.path(),
            credential_store.clone(),
            Arc::new(ScriptedProviderFactory {
                calls: continuation_calls.clone(),
                behavior: ScriptedBehavior::Succeed,
                saw_secret: Arc::new(Mutex::new(false)),
                observed_inputs: None,
            }),
        )
        .expect("reopen for continuation");
        pipeline.preflight(&follow_up).expect("follow-up preflight");
        assert!(
            pipeline
                .records
                .execution(&follow_up.request_id)
                .expect("preflight record lookup")
                .is_none(),
            "preflight must not write the continuation record"
        );
        assert_eq!(continuation_calls.load(Ordering::SeqCst), 0);
        let second = pipeline.execute(follow_up.clone()).expect("follow-up turn");
        assert_eq!(first.chat_id, second.chat_id);
        assert_eq!(first.run_id, second.run_id);
        assert_eq!(first.snapshot_id, second.snapshot_id);
        assert_eq!(first.snapshot_hash, second.snapshot_hash);
        assert_eq!(first.authority_manifest_id, second.authority_manifest_id);
        assert_ne!(first.worker_invocation_id, second.worker_invocation_id);
        assert_ne!(first.broker_invocation_id, second.broker_invocation_id);
        assert_eq!(continuation_calls.load(Ordering::SeqCst), 1);

        let prepared = pipeline
            .records
            .execution(&follow_up.request_id)
            .expect("follow-up record")
            .expect("prepared follow-up");
        assert_eq!(prepared.scheduler_continuation, 1);
        let prepared_checkpoint = prepared
            .scheduler_checkpoint
            .as_ref()
            .expect("prepared continuation checkpoint");
        assert_eq!(prepared_checkpoint.committed_cursor, 5);
        assert_eq!(prepared_checkpoint.next_token_ordinal, 7);
        assert_eq!(prepared.scheduler_trace.len(), 14);
        assert_eq!(
            prepared
                .scheduler_trace
                .iter()
                .skip(8)
                .map(|entry| (entry.action.as_str(), entry.node_id.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("resumed", "wait.1"),
                ("claimed", "wait.1"),
                ("input_received_acknowledged", "wait.1"),
                ("claimed", "input.1"),
                ("transition_acknowledged", "input.1"),
                ("claimed", "agent.1"),
            ]
        );
        let outcome = pipeline
            .records
            .outcome(&second.broker_invocation_id)
            .expect("follow-up outcome")
            .expect("durable follow-up outcome");
        let checkpoint = outcome
            .scheduler_checkpoint
            .as_ref()
            .expect("terminal continuation checkpoint");
        assert_eq!(checkpoint.committed_cursor, 7);
        assert_eq!(checkpoint.next_token_ordinal, 9);
        assert_eq!(outcome.scheduler_trace.len(), 19);
        assert_eq!(
            checkpoint
                .tokens
                .iter()
                .filter(|token| token.node_id.as_str() == "wait.1"
                    && token.state == TokenStateV1::Completed)
                .count(),
            1
        );
        assert_eq!(
            checkpoint
                .tokens
                .iter()
                .filter(|token| token.node_id.as_str() == "wait.1"
                    && token.state == TokenStateV1::Suspended)
                .count(),
            1
        );
        assert_eq!(
            checkpoint
                .tokens
                .iter()
                .filter(|token| token.node_id.as_str() == "input.1")
                .count(),
            2,
            "continuation must append one Input token rather than reseed a Run"
        );

        let exact_retry = pipeline
            .execute(follow_up.clone())
            .expect("exact continuation retry");
        assert!(exact_retry.replayed);
        assert_eq!(continuation_calls.load(Ordering::SeqCst), 1);
        drop(pipeline);
        let restart_calls = Arc::new(AtomicUsize::new(0));
        let pipeline = SimpleChatExecutionPipeline::compose(
            root.path(),
            credential_store,
            Arc::new(ScriptedProviderFactory {
                calls: restart_calls.clone(),
                behavior: ScriptedBehavior::Succeed,
                saw_secret: Arc::new(Mutex::new(false)),
                observed_inputs: None,
            }),
        )
        .expect("second reopen");
        let restart_retry = pipeline
            .execute(follow_up)
            .expect("restart continuation retry");
        assert!(restart_retry.replayed);
        assert_eq!(restart_calls.load(Ordering::SeqCst), 0);

        let mut drifted = request(metadata);
        drifted.request_id = stable("command.pipeline-drifted").expect("drifted request");
        drifted.frozen_context_hash = format!("sha256:{}", "b".repeat(64));
        assert!(matches!(
            pipeline.execute(drifted),
            Err(SimpleChatPipelineError::Store(_))
        ));
        assert_eq!(restart_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn missing_credential_is_a_durable_definite_no_start_without_provider_execution() {
        let root = TempDir::new().expect("root");
        let (pipeline, credential_store, metadata, calls, saw_secret) =
            setup(&root, ScriptedBehavior::Succeed);
        credential_store
            .delete(&metadata.credential)
            .expect("remove test credential");

        let first = pipeline
            .execute(request(metadata.clone()))
            .expect("materialization failure is a settled outcome");
        assert_eq!(
            first.status,
            SimpleChatExecutionStatusV1::FailedDefinitelyNotStarted
        );
        assert!(
            first
                .error
                .as_deref()
                .is_some_and(|error| error.contains("materialization failed"))
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!((first.model_turns, first.tool_calls), (0, 0));
        assert!(!*saw_secret.lock().expect("secret observation"));

        let replay = pipeline
            .execute(request(metadata))
            .expect("settled failure replay");
        assert_eq!(
            replay.status,
            SimpleChatExecutionStatusV1::FailedDefinitelyNotStarted
        );
        assert!(replay.replayed);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn accepted_response_without_assistant_text_is_not_committed_as_success() {
        let root = TempDir::new().expect("root");
        let (pipeline, _credential_store, metadata, calls, _) =
            setup(&root, ScriptedBehavior::EmptyAcceptedOutput);
        let result = pipeline
            .execute(request(metadata))
            .expect("accepted empty outcome is durably classified");
        assert_eq!(
            result.status,
            SimpleChatExecutionStatusV1::FailedKnownStarted
        );
        assert!(result.assistant_text.is_none());
        assert_eq!((result.input_units, result.output_units), (7, 0));
        assert_eq!((result.model_turns, result.tool_calls), (1, 0));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn ambiguous_provider_acceptance_is_settled_and_never_falls_back_or_replays() {
        let root = TempDir::new().expect("root");
        let (pipeline, _credential_store, metadata, calls, _) =
            setup(&root, ScriptedBehavior::Ambiguous);
        let first = pipeline
            .execute(request(metadata.clone()))
            .expect("ambiguous");
        assert_eq!(first.status, SimpleChatExecutionStatusV1::OutcomeUncertain);
        assert_eq!((first.model_turns, first.tool_calls), (1, 0));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let replay = pipeline
            .execute(request(metadata))
            .expect("ambiguous replay");
        assert_eq!(replay.status, SimpleChatExecutionStatusV1::OutcomeUncertain);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn request_identity_cannot_be_reused_with_changed_messages() {
        let root = TempDir::new().expect("root");
        let (pipeline, _credential_store, metadata, calls, _) =
            setup(&root, ScriptedBehavior::Succeed);
        pipeline.execute(request(metadata.clone())).expect("first");
        let mut changed = request(metadata);
        changed.messages[0].content = "different semantics".to_owned();
        assert!(matches!(
            pipeline.execute(changed),
            Err(SimpleChatPipelineError::Store(_))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn saved_agent_instructions_are_the_exact_leading_provider_system_layer() {
        let root = TempDir::new().expect("root");
        let credential_store = Arc::new(MemoryCredentialStore::default());
        let mut secret_broker = SecretBroker::with_store(credential_store.clone());
        let metadata = secret_broker
            .put_credential(
                CredentialRef(stable("credential.instructions-test").expect("credential ID")),
                BTreeMap::from([(API_KEY_FIELD.to_owned(), b"test-secret".to_vec())]),
            )
            .expect("credential");
        let calls = Arc::new(AtomicUsize::new(0));
        let observed_inputs = Arc::new(Mutex::new(Vec::new()));
        let pipeline = SimpleChatExecutionPipeline::compose(
            root.path(),
            credential_store,
            Arc::new(ScriptedProviderFactory {
                calls: calls.clone(),
                behavior: ScriptedBehavior::Succeed,
                saw_secret: Arc::new(Mutex::new(false)),
                observed_inputs: Some(observed_inputs.clone()),
            }),
        )
        .expect("pipeline");
        let instructions = "Use only evidence from the saved project.";
        let mut execution_request = request(metadata.clone());
        execution_request.workflow_snapshot["nodes"][1]["configuration"]["instructions"] =
            json!(instructions);
        execution_request.messages.insert(
            0,
            SimpleChatMessageV1 {
                role: "system".into(),
                content: instructions.into(),
            },
        );
        pipeline
            .execute(execution_request)
            .expect("instruction-bound execution");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            observed_inputs.lock().expect("provider input")[0]["messages"][0],
            json!({"role":"system","content":instructions})
        );

        let mut mismatched = request(metadata.clone());
        mismatched.request_id = stable("command.instructions-mismatch").expect("request");
        mismatched.chat_id = stable("chat.instructions-mismatch").expect("chat");
        mismatched.run_id = stable("run.instructions-mismatch").expect("run");
        mismatched.workflow_snapshot["nodes"][1]["configuration"]["instructions"] =
            json!(instructions);
        mismatched.messages.insert(
            0,
            SimpleChatMessageV1 {
                role: "system".into(),
                content: "different system layer".into(),
            },
        );
        assert!(matches!(
            pipeline.execute(mismatched.clone()),
            Err(SimpleChatPipelineError::InvalidInput(_))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            pipeline
                .records
                .execution(&mismatched.request_id)
                .expect("execution lookup")
                .is_none()
        );

        let mut unknown_config = request(metadata);
        unknown_config.request_id = stable("command.instructions-unknown").expect("request");
        unknown_config.chat_id = stable("chat.instructions-unknown").expect("chat");
        unknown_config.run_id = stable("run.instructions-unknown").expect("run");
        unknown_config.workflow_snapshot["nodes"][1]["configuration"]["future"] = json!(true);
        assert!(matches!(
            pipeline.execute(unknown_config.clone()),
            Err(SimpleChatPipelineError::InvalidInput(_))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            pipeline
                .records
                .execution(&unknown_config.request_id)
                .expect("execution lookup")
                .is_none()
        );
    }

    #[test]
    fn restart_after_dispatch_attempt_without_terminal_evidence_never_replays_provider() {
        let root = TempDir::new().expect("root");
        let (pipeline, credential_store, metadata, calls, _) =
            setup(&root, ScriptedBehavior::Succeed);
        let execution_request = request(metadata.clone());
        let protocol =
            ProviderProtocolV1::parse(&execution_request.provider.kind).expect("provider protocol");
        let descriptor = pipeline
            .descriptors
            .get(&protocol)
            .expect("provider descriptor");
        let prepared = pipeline
            .prepare(&execution_request, protocol, descriptor)
            .expect("prepare");
        pipeline
            .records
            .record_execution(&prepared)
            .expect("record execution");
        let lease_authority = Arc::new(PipelineLeaseAuthority::new(
            pipeline.generation,
            credential_store.clone(),
        ));
        let broker = DurableInvocationBroker::new(pipeline.ledger.clone(), APPROVAL_TTL_MILLIS)
            .with_lease_port(Arc::new(PreparedLeaseIssuer {
                authority: lease_authority,
                secret: prepared.secret.clone(),
            }));
        let dispatch = match broker
            .propose(
                &prepared.legacy_manifest(),
                prepared.broker_proposal.clone(),
                execution_request.now_epoch_millis,
            )
            .expect("authorize")
        {
            BrokerDecisionV1::DispatchReady(dispatch) => dispatch,
            other => panic!("unexpected broker decision: {other:?}"),
        };
        pipeline
            .ledger
            .append_atomic(
                &[InvocationLedgerEventV1::DispatchAttempted {
                    invocation_id: dispatch.invocation_id,
                }],
                None,
            )
            .expect("durable attempted fence");
        drop(pipeline);
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        let reopened_calls = Arc::new(AtomicUsize::new(0));
        let reopened = SimpleChatExecutionPipeline::compose(
            root.path(),
            credential_store,
            Arc::new(ScriptedProviderFactory {
                calls: reopened_calls.clone(),
                behavior: ScriptedBehavior::Succeed,
                saw_secret: Arc::new(Mutex::new(false)),
                observed_inputs: None,
            }),
        )
        .expect("reopen");
        let result = reopened.execute(request(metadata)).expect("recover");
        assert_eq!(result.status, SimpleChatExecutionStatusV1::OutcomeUncertain);
        assert!(result.replayed);
        assert_eq!(reopened_calls.load(Ordering::SeqCst), 0);
    }
}
