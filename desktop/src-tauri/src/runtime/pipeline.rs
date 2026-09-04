//! Authority-first execution pipeline for frozen JSON workflow graphs.
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
    McpCapabilitySnapshotV1, McpPeerPort, McpPeerTransportConfigV1, McpServerManifestV1,
    ModelToolExchangeV1, OpenAiCompatibleLimitsV1, OpenAiCompatibleProvider,
    OpenAiCompatibleProviderConfig, ProductionMcpPeer, ProviderEnginePortV1,
    RedeemLeaseRequestV1 as HostRedeemLeaseRequestV1, SecretDeliveryV1 as HostSecretDeliveryV1,
    SecretFieldPlanV1, SecretLeaseClientV1, SecretLeaseHandleV1, SecretMaterializationError,
    SecretMaterializationPlanV1, SecretMaterializer, SideEffectClass,
};
use aworkit_local_store::{
    CommitBatch, Deduplication, Event, LocalHistoryStore, OutboxEntry, StoreError,
};
use aworkit_protocol::{
    AttestedExtensionSetV1, HistoryBackendV1, ProcessGeneration, SchemaVersion, StableId,
    WorkerBudgetV1, WorkerExecutorKindV1,
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
    scheduler::SchedulerCheckpointV1,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::Zeroizing;

use super::{
    cancellation::WorkflowCancellationController,
    documents::validate_v1_executable_catalog,
    graph_pass::{
        GraphApprovalRequestV1, GraphNodeActivityV1, GraphPassBudgetV1, GraphPassStatusV1,
        PendingGraphPassStateV1, compile_graph_pass, execute_graph_pass_observed,
    },
    mcp_tools::{MCP_CAPABILITY_PREFIX, McpRunServerPreparationV1},
    model_tool_loop::PROVIDER_TIMEOUT_RECOVERIES_V1,
    project_scope::revalidate_git_branch,
    run_events::{ModelRunEventObserver, RunEventStream},
    semantic_events::{SemanticEventCommitter, ephemeral_semantic_event_committer},
    tool_loop::{
        FileToolAuthorityRuntimeV1, FrozenFileToolAuthorityContextV1, StoredFileToolBindingV1,
        ToolApprovalChallengeV1, WorkflowToolActivityV1, WorkflowToolBindingV1,
        file_tool_capability_binding_with_nodes, file_tool_descriptors, freeze_file_tool_bindings,
        mcp_tool_descriptor,
    },
};

const MODEL_ADAPTER_VERSION: &str = "1.0.0";
const API_KEY_FIELD: &str = "api_key";
const MODEL_SCOPE: &str = "model.invoke";
pub(crate) const WORKFLOW_MAX_MESSAGE_CONTEXT_BYTES: usize = 256 * 1024;
pub(crate) const WORKFLOW_MAX_ASSISTANT_TEXT_BYTES: usize = 16 * 1024;
const MAXIMUM_INPUT_BYTES: usize = 384 * 1024;
const MAXIMUM_OUTPUT_BYTES: usize = WORKFLOW_MAX_ASSISTANT_TEXT_BYTES;
pub(crate) const MAXIMUM_WORKFLOW_SNAPSHOT_BYTES: usize = 128 * 1024;
const MAXIMUM_PREPARED_RECORD_BYTES: usize = 768 * 1024;
const MAXIMUM_PROVIDER_OUTCOME_BYTES: usize = 896 * 1024;
const MAXIMUM_ERROR_BYTES: usize = 16 * 1024;
const APPROVAL_TTL_MILLIS: u64 = 60_000;
const DEFAULT_WORKFLOW_DEADLINE_MILLIS: u64 = 10 * 60_000;
const LEASE_TTL: Duration = Duration::from_secs(2 * 60);
const PIPELINE_CHAT_ID: &str = "pipeline.execution";
const BROKER_CHAT_ID: &str = "broker.invocations";
const STORE_BRANCH_ID: &str = "main";
const HOST_DESTINATION: &str = "aworkit.capability-host";
const WORKER_DESTINATION: &str = "aworkit.workflow-worker";
const DEFAULT_FROZEN_CONTEXT_HASH: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";
const MAXIMUM_PROVIDER_REQUEST_TIMEOUT_SECONDS_V1: u64 = 3_600;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ProviderProtocolV1 {
    OpenAiCompatible,
    Anthropic,
    Gemini,
}

impl ProviderProtocolV1 {
    const ALL: [Self; 3] = [Self::OpenAiCompatible, Self::Anthropic, Self::Gemini];

    fn parse(kind: &str) -> Result<Self, WorkflowPipelineError> {
        match kind {
            "openai_compatible" => Ok(Self::OpenAiCompatible),
            "anthropic" => Ok(Self::Anthropic),
            "gemini" => Ok(Self::Gemini),
            _ => Err(WorkflowPipelineError::InvalidInput(format!(
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
pub struct WorkflowMessageV1 {
    pub role: String,
    pub content: String,
}

/// Frozen binding for an installed native provider protocol.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowProviderBindingV1 {
    /// Exact protocol kind: `openai_compatible`, `anthropic`, or `gemini`.
    pub kind: String,
    pub base_url: String,
    pub model: String,
    pub request_timeout_seconds: u64,
    pub maximum_tool_output_bytes: usize,
    /// Opaque metadata only. The secret value remains in the platform store.
    pub credential: Option<CredentialMetadataV1>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkflowExecutionRequestV1 {
    pub request_id: StableId,
    pub chat_id: StableId,
    pub run_id: StableId,
    pub provider: WorkflowProviderBindingV1,
    /// Closed non-secret model parameters frozen from Settings.
    pub model_parameters: BTreeMap<String, Value>,
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
    pub tools: Vec<WorkflowToolBindingV1>,
    /// Additional provider attempts reserved only for typed request timeouts.
    pub maximum_timeout_recoveries: u32,
    /// Exact saved workflow JSON document frozen at the first input.
    pub workflow_snapshot: Value,
    pub messages: Vec<WorkflowMessageV1>,
    pub now_epoch_millis: u64,
    pub deadline_epoch_millis: u64,
    pub budget: WorkerBudgetV1,
    /// Core-attested MCP manifests frozen with the Run so the dispatcher can
    /// open exact sessions on demand. Empty for tool sets without MCP.
    pub mcp_servers: Vec<McpServerManifestV1>,
}

impl WorkflowExecutionRequestV1 {
    #[must_use]
    pub fn bounded(
        request_id: StableId,
        chat_id: StableId,
        run_id: StableId,
        provider: WorkflowProviderBindingV1,
        messages: Vec<WorkflowMessageV1>,
        now_epoch_millis: u64,
    ) -> Self {
        Self {
            request_id,
            chat_id,
            run_id,
            provider,
            model_parameters: BTreeMap::new(),
            frozen_context_hash: DEFAULT_FROZEN_CONTEXT_HASH.to_owned(),
            workspace: None,
            project_branch: None,
            tools: Vec::new(),
            maximum_timeout_recoveries: 0,
            workflow_snapshot: Value::Null,
            messages,
            now_epoch_millis,
            deadline_epoch_millis: now_epoch_millis
                .saturating_add(DEFAULT_WORKFLOW_DEADLINE_MILLIS),
            mcp_servers: Vec::new(),
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
                deadline_ms: DEFAULT_WORKFLOW_DEADLINE_MILLIS,
            },
        }
    }

    /// Replaces the bounded constructor's default with the exact duration
    /// frozen from the selected workflow.
    pub fn set_deadline_millis(&mut self, deadline_millis: u64) {
        self.deadline_epoch_millis = self.now_epoch_millis.saturating_add(deadline_millis);
        self.budget.deadline_ms = deadline_millis;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowExecutionStatusV1 {
    Succeeded,
    FailedDefinitelyNotStarted,
    FailedKnownStarted,
    OutcomeUncertain,
    AwaitingApproval,
}

/// Bounded provider-supplied reasoning retained with the canonical Run result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowReasoningActivityV1 {
    pub body: String,
    pub category: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowExecutionResultV1 {
    pub request_id: StableId,
    pub chat_id: StableId,
    pub run_id: StableId,
    pub snapshot_id: StableId,
    pub snapshot_hash: String,
    pub authority_manifest_id: StableId,
    pub worker_invocation_id: StableId,
    pub broker_invocation_id: StableId,
    pub outcome_hash: String,
    pub status: WorkflowExecutionStatusV1,
    pub assistant_text: Option<String>,
    pub reasoning: Option<WorkflowReasoningActivityV1>,
    pub error: Option<String>,
    pub model: String,
    pub input_units: u64,
    pub output_units: u64,
    pub model_turns: u64,
    pub tool_calls: u64,
    pub tool_activity: Vec<WorkflowToolActivityV1>,
    pub node_activity: Vec<GraphNodeActivityV1>,
    pub approval: Option<GraphApprovalRequestV1>,
    pub replayed: bool,
}

#[derive(Debug, Error)]
pub enum WorkflowPipelineError {
    #[error("workflow pipeline input is invalid: {0}")]
    InvalidInput(String),
    #[error("workflow authority freezing failed: {0}")]
    Authority(String),
    #[error("workflow durable execution store failed: {0}")]
    Store(String),
    #[error("workflow invocation broker failed: {0}")]
    Broker(String),
    #[error("workflow invocation requires an approval flow that is not supplied by this API")]
    ApprovalRequired,
    #[error("tool invocation requires a user approval decision")]
    ToolApproval(ToolApprovalChallengeV1),
    #[error("workflow invocation was denied by its frozen authority")]
    AuthorityDenied,
    #[error("workflow worker contract failed: {0}")]
    Worker(String),
    #[error("workflow host composition failed: {0}")]
    Host(String),
    #[error("workflow durable evidence is incomplete or internally inconsistent")]
    IncompleteEvidence,
}

/// Long-lived service seam. It owns no editable settings representation.
pub struct WorkflowExecutionPipeline {
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
    event_committer: Arc<dyn SemanticEventCommitter>,
    cancellation_controller: WorkflowCancellationController,
}

impl WorkflowExecutionPipeline {
    pub fn open(data_root: impl AsRef<Path>) -> Result<Self, WorkflowPipelineError> {
        Self::open_with_credential_store(data_root, Arc::new(NativeCredentialStore::new()))
    }

    pub fn open_with_credential_store(
        data_root: impl AsRef<Path>,
        credential_store: Arc<dyn PlatformCredentialStorePort>,
    ) -> Result<Self, WorkflowPipelineError> {
        Self::open_with_credential_store_and_event_committer(
            data_root,
            credential_store,
            ephemeral_semantic_event_committer(),
        )
    }

    pub(crate) fn open_with_credential_store_and_event_committer(
        data_root: impl AsRef<Path>,
        credential_store: Arc<dyn PlatformCredentialStorePort>,
        event_committer: Arc<dyn SemanticEventCommitter>,
    ) -> Result<Self, WorkflowPipelineError> {
        Self::compose_with_event_committer(
            data_root.as_ref(),
            credential_store,
            Arc::new(BuiltInProviderFactory),
            event_committer,
        )
    }

    #[cfg(test)]
    fn compose(
        data_root: &Path,
        credential_store: Arc<dyn PlatformCredentialStorePort>,
        provider_factory: Arc<dyn ProviderFactoryV1>,
    ) -> Result<Self, WorkflowPipelineError> {
        Self::compose_with_event_committer(
            data_root,
            credential_store,
            provider_factory,
            ephemeral_semantic_event_committer(),
        )
    }

    fn compose_with_event_committer(
        data_root: &Path,
        credential_store: Arc<dyn PlatformCredentialStorePort>,
        provider_factory: Arc<dyn ProviderFactoryV1>,
        event_committer: Arc<dyn SemanticEventCommitter>,
    ) -> Result<Self, WorkflowPipelineError> {
        fs::create_dir_all(data_root).map_err(store_error)?;
        let root = fs::canonicalize(data_root).map_err(store_error)?;
        let projects = ProjectCoordinator::open(root.join("core").join("workflow-execution"))
            .map_err(|error| WorkflowPipelineError::Authority(error.to_string()))?;
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
                .map_err(|error| WorkflowPipelineError::Host(error.to_string()))?;
        }
        for descriptor in file_tool_descriptors.values() {
            registry
                .register_capability(descriptor.clone())
                .map_err(|error| WorkflowPipelineError::Host(error.to_string()))?;
        }
        let mut attested = AttestedExtensionSetV1 {
            host_id: stable("host.workflow-execution")?,
            host_generation: generation,
            host_protocol: 1,
            extensions: Vec::new(),
            set_hash: String::new(),
        };
        attested.set_hash = attested_extension_set_hash_v1(&attested)
            .map_err(|error| WorkflowPipelineError::Host(error.to_string()))?;
        let frozen = registry
            .materialize_attested_set(&attested)
            .map_err(|error| WorkflowPipelineError::Host(error.to_string()))?;
        let host = Arc::new(
            CapabilityHost::from_attested_registry(frozen, core_key.copy(), 8)
                .map_err(|error| WorkflowPipelineError::Host(error.to_string()))?,
        );
        let file_tool_authority = FileToolAuthorityRuntimeV1::open(
            &database,
            projects.clone(),
            host.clone(),
            file_tool_descriptors.clone(),
            generation,
            core_key.clone(),
            credential_store.clone(),
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
            event_committer,
            cancellation_controller: WorkflowCancellationController::default(),
        })
    }

    pub(crate) fn with_cancellation_controller(
        mut self,
        cancellation_controller: WorkflowCancellationController,
    ) -> Self {
        self.cancellation_controller = cancellation_controller;
        self
    }

    /// Performs every deterministic validation and constructs the exact
    /// bounded frozen execution record without writing records, proposing to
    /// the broker, materializing a secret, or invoking a provider/tool.
    pub fn preflight(
        &self,
        request: &WorkflowExecutionRequestV1,
    ) -> Result<(), WorkflowPipelineError> {
        self.validated_prepared(request).map(|_| ())
    }

    /// Latest durable Run task list recorded by the todo tool, if any. The
    /// newest stored snapshot is the live list shown in the editor.
    pub(crate) fn run_todo_state(
        &self,
        run_id: &StableId,
    ) -> Result<Option<Value>, WorkflowPipelineError> {
        self.file_tool_authority.todo_state(run_id)
    }

    fn validated_prepared(
        &self,
        request: &WorkflowExecutionRequestV1,
    ) -> Result<(PreparedExecutionRecordV1, bool), WorkflowPipelineError> {
        let protocol = ProviderProtocolV1::parse(&request.provider.kind)?;
        let descriptor = self
            .descriptors
            .get(&protocol)
            .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
        validate_request(request, protocol, descriptor)?;
        if let Some(existing) = self.records.execution(&request.request_id)? {
            self.validate_existing_request_semantics(request, &existing)?;
            if self.existing_request_can_still_start_effect(&existing)? {
                let workspace = existing
                    .workspace
                    .as_ref()
                    .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
                self.projects
                    .revalidate_workspace_v1(workspace)
                    .map_err(|error| WorkflowPipelineError::Authority(error.to_string()))?;
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
            return Err(WorkflowPipelineError::Store(
                "Chat/Run identity was reused with changed frozen authority".to_owned(),
            ));
        }
        Ok((prepared, false))
    }

    fn validate_existing_request_semantics(
        &self,
        request: &WorkflowExecutionRequestV1,
        existing: &PreparedExecutionRecordV1,
    ) -> Result<(), WorkflowPipelineError> {
        let provider = StoredProviderBindingV1 {
            kind: request.provider.kind.clone(),
            base_url: request.provider.base_url.clone(),
            model: request.provider.model.clone(),
            parameters: request.model_parameters.clone(),
            request_timeout_seconds: request.provider.request_timeout_seconds,
            maximum_tool_output_bytes: request.provider.maximum_tool_output_bytes,
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
            .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
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
            node.config.get("frozenContextHash").and_then(Value::as_str)
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
            || existing.project_branch != request.project_branch
            || existing.snapshot.budget != request.budget
            || stored_messages != request_messages
            || !workspace_matches
            || !workspace_identity_matches
            || !saved_nodes_match
            || !saved_edges_match
            || !frozen_context_matches
        {
            return Err(WorkflowPipelineError::Store(
                "request ID was reused with changed frozen execution semantics".to_owned(),
            ));
        }
        Ok(())
    }

    fn existing_request_can_still_start_effect(
        &self,
        existing: &PreparedExecutionRecordV1,
    ) -> Result<bool, WorkflowPipelineError> {
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
        }) || self.records.outcome(&invocation_id)?.is_some()
            || self
                .records
                .pending_approval_for_invocation(&invocation_id)?
                .is_some();
        Ok(!effect_fenced)
    }

    /// Executes one exact workflow pass through every authority and
    /// settlement boundary. Reusing `request_id` with changed semantics fails;
    /// an exact retry returns durable evidence without calling the provider.
    pub fn execute(
        &self,
        request: WorkflowExecutionRequestV1,
    ) -> Result<WorkflowExecutionResultV1, WorkflowPipelineError> {
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
            BrokerDecisionV1::Denied => return Err(WorkflowPipelineError::AuthorityDenied),
            BrokerDecisionV1::AwaitingApproval(_) => {
                return Err(WorkflowPipelineError::ApprovalRequired);
            }
            BrokerDecisionV1::DispatchReady(dispatch) => dispatch.invocation_id,
            BrokerDecisionV1::AlreadySettled(_) => self
                .ledger
                .invocation_for_proposal(&prepared.broker_proposal.proposal_id)?
                .ok_or(WorkflowPipelineError::IncompleteEvidence)?,
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
                event_committer: self.event_committer.clone(),
                cancellation_controller: self.cancellation_controller.clone(),
            };
            // The broker commits DispatchAttempted before this call. A transport
            // error or an old attempted dispatch is conservatively settled by
            // the broker and is never automatically replayed.
            let _ = broker.deliver_dispatches(&host_port);
            self.reconcile_persisted_outcomes(&broker)?;
        }

        if self.ledger.settlement(&broker_invocation_id)?.is_none()
            && let Some(pending) = self
                .records
                .pending_approval_for_invocation(&broker_invocation_id)?
        {
            // The graph pass is durably suspended at an approval gate.
            return Ok(WorkflowExecutionResultV1 {
                request_id: prepared.request_id,
                chat_id: prepared.snapshot.chat_id.clone(),
                run_id: prepared.snapshot.run_id.clone(),
                snapshot_id: prepared.snapshot.snapshot_id.clone(),
                snapshot_hash: prepared.snapshot.snapshot_hash.clone(),
                authority_manifest_id: prepared.manifest.manifest_id.clone(),
                worker_invocation_id: prepared.worker_proposal.invocation_id.clone(),
                broker_invocation_id,
                outcome_hash: String::new(),
                status: WorkflowExecutionStatusV1::AwaitingApproval,
                assistant_text: None,
                reasoning: pending
                    .reasoning_body
                    .clone()
                    .map(|body| WorkflowReasoningActivityV1 {
                        body,
                        category: pending
                            .reasoning_category
                            .clone()
                            .unwrap_or_else(|| "source_provided".to_owned()),
                    }),
                error: None,
                model: prepared.provider.model.clone(),
                input_units: pending.input_units,
                output_units: pending.output_units,
                model_turns: u64::from(pending.attempted_model_turns),
                tool_calls: u64::from(pending.settled_tool_calls),
                tool_activity: pending.tool_activity.clone(),
                node_activity: pending.activity.clone(),
                approval: Some(GraphApprovalRequestV1 {
                    decision_id: pending.decision_id.clone(),
                    node_id: pending.pending_node_id.clone(),
                    title: pending.title.clone(),
                    message: pending.message.clone(),
                }),
                replayed: record_existing,
            });
        }

        let (outcome_hash, uncertain) = self
            .ledger
            .settlement(&broker_invocation_id)?
            .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
        let durable_outcome = self
            .records
            .outcome(&broker_invocation_id)?
            .filter(|outcome| outcome_hash_v1(outcome).ok().as_deref() == Some(&outcome_hash));
        let outcome = if let Some(outcome) = durable_outcome {
            outcome
        } else {
            let outcome = ProviderOutcomeRecordV1 {
                schema_version: 1,
                invocation_id: broker_invocation_id.clone(),
                status: if uncertain {
                    WorkflowExecutionStatusV1::OutcomeUncertain
                } else {
                    WorkflowExecutionStatusV1::FailedDefinitelyNotStarted
                },
                assistant_text: None,
                reasoning: None,
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
                legacy_run_activity: Vec::new(),
                node_activity: Vec::new(),
                approval: None,
                scheduler_checkpoint: None,
                scheduler_trace: Vec::new(),
            };
            outcome
        };
        let _ = broker.deliver_worker_results(&CommittedWorkerAck);
        Ok(WorkflowExecutionResultV1 {
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
            reasoning: outcome.reasoning,
            error: outcome.error,
            model: outcome.model,
            input_units: outcome.input_units,
            output_units: outcome.output_units,
            model_turns: u64::from(outcome.attempted_model_turns),
            tool_calls: u64::from(outcome.settled_tool_calls),
            tool_activity: outcome.tool_activity,
            node_activity: outcome.node_activity,
            approval: outcome.approval,
            replayed: record_existing,
        })
    }

    /// Applies a committed user decision to a durably suspended graph-pass
    /// approval gate. Approve continues the pass from the stored prefix;
    /// reject fails the pass. The terminal outcome is recorded and the outer
    /// invocation settled exactly like a fresh dispatch, so replaying the same
    /// decision is idempotent and no model or tool work is recomputed.
    pub fn resume_approval(
        &self,
        decision_id: &str,
        approved: bool,
    ) -> Result<WorkflowExecutionResultV1, WorkflowPipelineError> {
        if self.records.approval_resolved(decision_id)? {
            return Err(WorkflowPipelineError::Store(
                "approval decision was already applied".to_owned(),
            ));
        }
        let decision = stable(decision_id)?;
        let pending =
            self.records
                .pending_approval(decision_id)?
                .ok_or(WorkflowPipelineError::Store(
                    "unknown approval decision".to_owned(),
                ))?;
        let request_id = stable(&pending.request_id)?;
        let prepared = self
            .records
            .execution(&request_id)?
            .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
        let broker_invocation_id = stable(&pending.invocation_id)?;
        let recorded_invocation = self
            .ledger
            .invocation_for_proposal(&prepared.broker_proposal.proposal_id)?
            .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
        if recorded_invocation != broker_invocation_id
            || prepared.snapshot.chat_id.as_str() != pending.chat_id
            || prepared.snapshot.run_id.as_str() != pending.run_id
        {
            return Err(WorkflowPipelineError::IncompleteEvidence);
        }
        let workspace = prepared
            .workspace
            .as_ref()
            .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
        if self.projects.revalidate_workspace_v1(workspace).is_err()
            || revalidate_optional_project_branch(workspace, prepared.project_branch.as_deref())
                .is_err()
        {
            return Err(WorkflowPipelineError::Authority(
                "frozen project workspace or Git branch drifted before approval resume".into(),
            ));
        }
        let protocol = ProviderProtocolV1::parse(&prepared.provider.kind)?;
        let descriptor = self
            .descriptors
            .get(&protocol)
            .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
        let lease_authority = Arc::new(PipelineLeaseAuthority::new(
            self.generation,
            self.credential_store.clone(),
        ));
        let materialized = if let Some(secret) = prepared.secret.as_ref() {
            let lease = lease_id(&broker_invocation_id, secret)?;
            lease_authority.prepare(
                Some(secret),
                &broker_invocation_id,
                &prepared.snapshot.run_id,
                &[lease.clone()],
            )?;
            let materializer = SecretMaterializer::new(CoreSecretLeaseClient {
                authority: lease_authority,
            });
            match materializer.materialize(&SecretMaterializationPlanV1 {
                decision_id: broker_invocation_id.clone(),
                invocation_id: broker_invocation_id.clone(),
                host_generation: self.generation,
                lease: SecretLeaseHandleV1 { lease_id: lease },
                fields: vec![SecretFieldPlanV1 {
                    field: API_KEY_FIELD.to_owned(),
                    target: InjectionTargetV1::Header(protocol.api_key_header().to_owned()),
                }],
            }) {
                Ok(materialized) => Some(materialized),
                Err(error) => {
                    return Err(WorkflowPipelineError::Store(format!(
                        "credential lease materialization failed: {error}"
                    )));
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
                    return Err(WorkflowPipelineError::Store(
                        "credential API-key field is not valid UTF-8".to_owned(),
                    ));
                }
            },
            None => None,
        };
        let provider = self
            .provider_factory
            .create(descriptor, &prepared.provider, api_key)
            .map_err(|error| WorkflowPipelineError::Store(redact_error(&materialized, &error)))?;
        let cancellation = CancellationToken::default();
        let _active_workflow = self
            .cancellation_controller
            .register(
                prepared.snapshot.chat_id.as_str(),
                prepared.snapshot.run_id.as_str(),
                cancellation.clone(),
            )
            .map_err(WorkflowPipelineError::Host)?;
        let run_events = Arc::new(RunEventStream::new(
            prepared.request_id.to_string(),
            prepared.snapshot.run_id.to_string(),
            self.event_committer.clone(),
            cancellation.clone(),
        ));
        run_events
            .ensure_healthy()
            .map_err(WorkflowPipelineError::Store)?;
        let model_observer = Arc::new(ModelRunEventObserver::new(run_events.clone()));
        let gateway =
            Arc::new(FrozenModelGateway::new(vec![provider]).with_observer(model_observer.clone()));
        let workflow = prepared
            .worker_proposal
            .payload
            .get("config")
            .and_then(|config| config.get("workflow"))
            .cloned()
            .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
        let compiled = compile_graph_pass(&workflow, &prepared.tool_bindings)
            .map_err(WorkflowPipelineError::InvalidInput)?;
        let authority = self.file_tool_authority.bind_with_run_events(
            FrozenFileToolAuthorityContextV1 {
                manifest: prepared.manifest.clone(),
                run_id: prepared.snapshot.run_id.clone(),
                request_id: prepared.request_id.clone(),
                node_id: prepared.worker_proposal.node_id.clone(),
                workspace: workspace.clone(),
                project_branch: prepared.project_branch.clone(),
                bindings: prepared.tool_bindings.clone(),
                deadline_epoch_millis: prepared.deadline_epoch_millis,
                model_gateway: Some(gateway.clone()),
                model_binding_id: Some(descriptor.capability_id.clone()),
                model_version_hash: Some(descriptor.version_hash.clone()),
                maximum_tool_output_bytes: prepared.provider.maximum_tool_output_bytes,
                mcp_manifests: prepared.mcp_manifests.clone(),
                cancellation: cancellation.clone(),
            },
            run_events.clone(),
        );
        let graph_observer = |activity: &GraphNodeActivityV1| {
            if !matches!(activity.status.as_str(), "started" | "waiting") {
                model_observer.settle(&activity.status);
            }
            run_events.publish_graph_activity(activity);
        };
        let pass = execute_graph_pass_observed(
            &compiled,
            &pending.conversation,
            GraphPassBudgetV1 {
                tokens: prepared.snapshot.budget.tokens,
                maximum_timeout_recoveries: prepared.maximum_timeout_recoveries,
                maximum_tool_output_bytes: prepared.provider.maximum_tool_output_bytes,
            },
            &gateway,
            &authority,
            &broker_invocation_id,
            prepared.request_id.as_str(),
            prepared.snapshot.chat_id.as_str(),
            prepared.snapshot.run_id.as_str(),
            &descriptor.capability_id,
            &descriptor.version_hash,
            current_epoch_millis(),
            prepared.deadline_epoch_millis,
            Some(&pending),
            Some(approved),
            &cancellation,
            Some(&graph_observer),
        );
        model_observer.settle(graph_pass_live_status(pass.status));
        run_events
            .ensure_healthy()
            .map_err(WorkflowPipelineError::Store)?;
        let reasoning = model_observer
            .reasoning_snapshot()
            .map(|(body, category)| WorkflowReasoningActivityV1 { body, category });
        match pass.status {
            GraphPassStatusV1::AwaitingApproval => {
                let mut next = pass
                    .pending_state
                    .clone()
                    .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
                next.reasoning_body = reasoning.as_ref().map(|item| item.body.clone());
                next.reasoning_category = reasoning.as_ref().map(|item| item.category.clone());
                self.records.mark_approval_resolved(&decision)?;
                self.records.store_pending_approval(&next)?;
                Ok(WorkflowExecutionResultV1 {
                    request_id: prepared.request_id,
                    chat_id: prepared.snapshot.chat_id.clone(),
                    run_id: prepared.snapshot.run_id.clone(),
                    snapshot_id: prepared.snapshot.snapshot_id.clone(),
                    snapshot_hash: prepared.snapshot.snapshot_hash.clone(),
                    authority_manifest_id: prepared.manifest.manifest_id.clone(),
                    worker_invocation_id: prepared.worker_proposal.invocation_id.clone(),
                    broker_invocation_id,
                    outcome_hash: String::new(),
                    status: WorkflowExecutionStatusV1::AwaitingApproval,
                    assistant_text: None,
                    reasoning: reasoning.clone(),
                    error: None,
                    model: prepared.provider.model.clone(),
                    input_units: next.input_units,
                    output_units: next.output_units,
                    model_turns: u64::from(next.attempted_model_turns),
                    tool_calls: u64::from(next.settled_tool_calls),
                    tool_activity: next.tool_activity.clone(),
                    node_activity: next.activity.clone(),
                    approval: pass.approval,
                    replayed: false,
                })
            }
            GraphPassStatusV1::Succeeded | GraphPassStatusV1::Failed => {
                let status = if matches!(pass.status, GraphPassStatusV1::Succeeded) {
                    WorkflowExecutionStatusV1::Succeeded
                } else {
                    WorkflowExecutionStatusV1::FailedKnownStarted
                };
                let mut record = ProviderOutcomeRecordV1 {
                    schema_version: 1,
                    invocation_id: broker_invocation_id.clone(),
                    status,
                    assistant_text: pass.assistant_text,
                    reasoning,
                    error: pass.error,
                    model: prepared.provider.model.clone(),
                    input_units: pass.input_units,
                    output_units: pass.output_units,
                    attempted_model_turns: pass.attempted_model_turns,
                    settled_tool_calls: pass.settled_tool_calls,
                    tool_exchanges: pass.exchanges.clone(),
                    tool_activity: pass.tool_activity.clone(),
                    legacy_run_activity: Vec::new(),
                    node_activity: pass.activity.clone(),
                    approval: None,
                    scheduler_checkpoint: None,
                    scheduler_trace: Vec::new(),
                };
                validate_provider_outcome_accounting(&prepared, &record)?;
                compact_provider_outcome_if_needed(&mut record)?;
                enforce_serialized_bound(
                    &record,
                    MAXIMUM_PROVIDER_OUTCOME_BYTES,
                    "provider outcome record",
                )?;
                self.records.record_outcome(&record)?;
                let broker = DurableInvocationBroker::new(self.ledger.clone(), APPROVAL_TTL_MILLIS);
                self.reconcile_persisted_outcomes(&broker)?;
                let (outcome_hash, _uncertain) = self
                    .ledger
                    .settlement(&broker_invocation_id)?
                    .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
                let _ = broker.deliver_worker_results(&CommittedWorkerAck);
                self.records.mark_approval_resolved(&decision)?;
                Ok(WorkflowExecutionResultV1 {
                    request_id: prepared.request_id,
                    chat_id: prepared.snapshot.chat_id.clone(),
                    run_id: prepared.snapshot.run_id.clone(),
                    snapshot_id: prepared.snapshot.snapshot_id.clone(),
                    snapshot_hash: prepared.snapshot.snapshot_hash.clone(),
                    authority_manifest_id: prepared.manifest.manifest_id.clone(),
                    worker_invocation_id: prepared.worker_proposal.invocation_id.clone(),
                    broker_invocation_id,
                    outcome_hash,
                    status: record.status,
                    assistant_text: record.assistant_text,
                    reasoning: record.reasoning,
                    error: record.error,
                    model: record.model,
                    input_units: record.input_units,
                    output_units: record.output_units,
                    model_turns: u64::from(record.attempted_model_turns),
                    tool_calls: u64::from(record.settled_tool_calls),
                    tool_activity: record.tool_activity,
                    node_activity: record.node_activity,
                    approval: None,
                    replayed: false,
                })
            }
        }
    }

    /// Installs a scripted MCP peer for tests. Fails closed when a peer is
    /// already installed for this application generation.
    pub fn install_mcp_peer(&self, peer: Arc<dyn McpPeerPort>) -> Result<(), String> {
        self.file_tool_authority.mcp.install_scripted_peer(peer)
    }

    /// Prepares production MCP sessions for one frozen Run: installs the
    /// production peer on first use, stages every server's materialized
    /// credential slots, opens the exact core-attested sessions, and returns
    /// their discovery snapshots. Binding drift fails closed without touching
    /// an active session.
    pub(crate) fn prepare_mcp_sessions(
        &self,
        run_id: &StableId,
        servers: &mut [McpRunServerPreparationV1],
    ) -> Result<Vec<McpCapabilitySnapshotV1>, String> {
        let needs_install = self.file_tool_authority.mcp.needs_install()?;
        if needs_install && !servers.is_empty() {
            let configs = servers
                .iter()
                .map(|server| McpPeerTransportConfigV1 {
                    server_id: server.manifest.server_id.clone(),
                    binding_hash: server.manifest.binding_hash.clone(),
                    endpoint: server.endpoint.clone(),
                })
                .collect();
            let peer = Arc::new(
                ProductionMcpPeer::with_limits(configs, super::mcp::production_peer_limits())
                    .map_err(|error| format!("MCP transport configuration is invalid: {error}"))?,
            );
            self.file_tool_authority.mcp.install_production_peer(peer)?;
        }
        let mut snapshots = Vec::with_capacity(servers.len());
        for server in servers {
            if let Some(materialization) = server.materialization.take() {
                self.file_tool_authority
                    .mcp
                    .stage_secrets(&server.manifest.server_id, materialization)?;
            }
            let snapshot = self
                .file_tool_authority
                .mcp
                .open_frozen(run_id, &server.manifest)?;
            snapshots.push(snapshot);
        }
        Ok(snapshots)
    }

    fn prepare_pending_leases(
        &self,
        broker: &DurableInvocationBroker,
        authority: &PipelineLeaseAuthority,
    ) -> Result<(), WorkflowPipelineError> {
        for outbox in broker.pending_dispatches().map_err(broker_error)? {
            let record = self
                .records
                .execution_for_dispatch(&outbox.dispatch)?
                .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
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
        request: &WorkflowExecutionRequestV1,
        protocol: ProviderProtocolV1,
        descriptor: &CapabilityDescriptor,
    ) -> Result<PreparedExecutionRecordV1, WorkflowPipelineError> {
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
            parameters: request.model_parameters.clone(),
            request_timeout_seconds: request.provider.request_timeout_seconds,
            maximum_tool_output_bytes: request.provider.maximum_tool_output_bytes,
        };
        let tool_bindings = freeze_file_tool_bindings(&request.tools)?;
        let (nodes, transitions, entry_nodes, model_node_id) = compile_graph_snapshot(
            request,
            descriptor,
            &provider,
            secret.as_ref(),
            &tool_bindings,
        )?;
        let workflow_hash =
            workflow_graph_hash_v1(&nodes, &transitions, &entry_nodes, &[], &[], &[])
                .map_err(|error| WorkflowPipelineError::Authority(error.to_string()))?;
        let workspace = request.workspace.clone().map_or_else(
            || {
                self.projects
                    .resolve_workspace_v1(self.root.join("core").join("unscoped-workspace"))
                    .map_err(|error| WorkflowPipelineError::Authority(error.to_string()))
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
            allowed_node_types: vec!["agent".to_owned(), "model_call".to_owned()],
        };
        let mut dynamic_descriptors = BTreeMap::new();
        for tool in &tool_bindings {
            if tool.capability_id.starts_with(MCP_CAPABILITY_PREFIX) {
                dynamic_descriptors.insert(
                    tool.internal_id.clone(),
                    mcp_tool_descriptor(&tool.internal_id)?,
                );
            }
        }
        let mut capability_bindings = vec![model_binding];
        for tool in &tool_bindings {
            let descriptor_key = if tool.capability_id.starts_with(MCP_CAPABILITY_PREFIX) {
                &tool.internal_id
            } else {
                &tool.capability_id
            };
            let descriptor = self
                .file_tool_descriptors
                .get(descriptor_key)
                .or_else(|| dynamic_descriptors.get(descriptor_key))
                .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
            let binding = file_tool_capability_binding_with_nodes(
                tool,
                descriptor,
                vec!["agent".to_owned(), "tool".to_owned()],
            )?;
            capability_bindings.push(binding);
        }
        let entry_node_id = entry_nodes.first().cloned();
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
        .map_err(|error| WorkflowPipelineError::Authority(error.to_string()))?;

        let scheduler_checkpoint = None;
        let scheduler_trace = Vec::new();
        let scheduler_continuation = 0;
        let agent_token_id = None;

        let budget_ref = digest_id("budget", request.request_id.as_str())?;
        let scope_id = format!("run.{}", &digest_hex(request.run_id.as_str())[..24]);
        let limits = LimitLedger::new(
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
        .map_err(|error| WorkflowPipelineError::Worker(error.to_string()))?;
        let agent = AgentLoopV1::new(AgentLoopConfigV1 {
            loop_id: digest_id("agent.loop", request.request_id.as_str())?,
            node_id: model_node_id,
            model_capability_ref: capability_id.clone(),
            authority_manifest_ref: manifest.manifest_id.clone(),
            budget_ref: budget_ref.clone(),
            scope_id,
            // The worker loop represents one outer, durably brokered graph
            // execution. Provider calls inside Agent nodes are not turn-capped.
            legacy_maximum_turns: None,
            turn_reservation: Usage {
                turns: request.budget.turns,
                attempts: request.budget.attempts,
                tool_calls: request.budget.tool_calls,
                tokens: request.budget.tokens,
                cost_micros: request.budget.cost_micros,
                actions: request.budget.actions,
            },
            context_pointers: Vec::new(),
            allowed_tool_capability_refs: tool_bindings
                .iter()
                .map(|binding| {
                    stable(
                        if binding.capability_id.starts_with(MCP_CAPABILITY_PREFIX) {
                            &binding.internal_id
                        } else {
                            &binding.capability_id
                        },
                    )
                })
                .collect::<Result<Vec<_>, _>>()?,
        })
        .map_err(|error| WorkflowPipelineError::Worker(error.to_string()))?;
        let context = json!({"messages": request.messages});
        let invocation_id = digest_id("pass", request.request_id.as_str())?;
        let node_id = entry_node_id.ok_or(WorkflowPipelineError::IncompleteEvidence)?;
        let payload = json!({
            "context": context,
            "config": {"workflow": request.workflow_snapshot},
        });
        let worker_proposal = WorkerInvocationProposalContractV1 {
            invocation_id: invocation_id.clone(),
            node_id,
            attempt_id: digest_id("pass.attempt", request.request_id.as_str())?,
            capability_ref: capability_id.clone(),
            authority_manifest_ref: manifest.manifest_id.clone(),
            budget_ref: budget_ref.clone(),
            payload,
        };
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
            legacy_maximum_turns: None,
            maximum_timeout_recoveries: request.maximum_timeout_recoveries,
            secret,
            worker_proposal,
            broker_proposal,
            agent_checkpoint: agent.checkpoint(),
            limit_checkpoint: limits.checkpoint(),
            scheduler_checkpoint,
            scheduler_trace,
            scheduler_continuation,
            agent_token_id,
            mcp_manifests: request
                .mcp_servers
                .iter()
                .map(|manifest| (manifest.server_id.to_string(), manifest.clone()))
                .collect(),
            deadline_epoch_millis: request.deadline_epoch_millis,
        };
        enforce_serialized_bound(
            &prepared,
            MAXIMUM_PREPARED_RECORD_BYTES,
            "prepared execution record",
        )?;
        Ok(prepared)
    }

    fn reconcile_persisted_outcomes(
        &self,
        broker: &DurableInvocationBroker,
    ) -> Result<(), WorkflowPipelineError> {
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
                    outcome.status == WorkflowExecutionStatusV1::OutcomeUncertain,
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
    #[serde(default)]
    parameters: BTreeMap<String, Value>,
    #[serde(default = "default_provider_request_timeout_seconds")]
    request_timeout_seconds: u64,
    #[serde(default = "default_maximum_tool_output_bytes")]
    maximum_tool_output_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredSecretBindingV1 {
    opaque_ref: StableId,
    field_names: BTreeSet<String>,
    revision: u64,
}

impl StoredSecretBindingV1 {
    fn from_metadata(metadata: &CredentialMetadataV1) -> Result<Self, WorkflowPipelineError> {
        if metadata.revision == 0 || !metadata.field_names.contains(API_KEY_FIELD) {
            return Err(WorkflowPipelineError::InvalidInput(
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
    /// Compatibility sink for prepared records written before Agent model
    /// turn caps were removed. New records omit this obsolete field.
    #[serde(default, rename = "maximumTurns", skip_serializing)]
    legacy_maximum_turns: Option<u32>,
    #[serde(default)]
    maximum_timeout_recoveries: u32,
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
    /// Core-attested MCP manifests frozen with this Run, keyed by server id.
    #[serde(default)]
    mcp_manifests: BTreeMap<String, McpServerManifestV1>,
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
            && self.maximum_timeout_recoveries == other.maximum_timeout_recoveries
            && self.secret == other.secret
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderOutcomeRecordV1 {
    schema_version: u16,
    invocation_id: StableId,
    status: WorkflowExecutionStatusV1,
    assistant_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reasoning: Option<WorkflowReasoningActivityV1>,
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
    tool_activity: Vec<WorkflowToolActivityV1>,
    /// Read-only migration sink for provider outcomes stored before semantic
    /// events became canonical. New records never serialize this field.
    #[serde(default, rename = "runActivity", skip_serializing)]
    legacy_run_activity: Vec<Value>,
    #[serde(default)]
    node_activity: Vec<GraphNodeActivityV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    approval: Option<GraphApprovalRequestV1>,
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

pub(super) struct CoreAuthenticationKey(Zeroizing<Vec<u8>>);

impl CoreAuthenticationKey {
    pub(super) fn random() -> Result<Self, WorkflowPipelineError> {
        let mut bytes = vec![0_u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|_| WorkflowPipelineError::Host("random key generation failed".into()))?;
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
                let limits = OpenAiCompatibleLimitsV1 {
                    request_timeout: Duration::from_secs(provider.request_timeout_seconds),
                    ..OpenAiCompatibleLimitsV1::default()
                };
                let config = OpenAiCompatibleProviderConfig::new(
                    protocol.capability_id(),
                    descriptor.version_hash.clone(),
                    &provider.base_url,
                    provider.model.clone(),
                    api_key,
                    limits,
                )
                .and_then(|config| config.with_request_parameters(&provider.parameters))
                .map_err(|error| error.to_string())?;
                OpenAiCompatibleProvider::new(config)
                    .map(|provider| Box::new(provider) as Box<dyn ProviderEnginePortV1>)
                    .map_err(|error| error.to_string())
            }
            ProviderProtocolV1::Anthropic => {
                let limits = AnthropicMessagesLimitsV1 {
                    request_timeout: Duration::from_secs(provider.request_timeout_seconds),
                    ..AnthropicMessagesLimitsV1::default()
                };
                let config = AnthropicMessagesProviderConfig::new(
                    protocol.capability_id(),
                    descriptor.version_hash.clone(),
                    &provider.base_url,
                    provider.model.clone(),
                    api_key,
                    limits,
                )
                .map_err(|error| error.to_string())?;
                AnthropicMessagesProvider::new(config)
                    .map(|provider| Box::new(provider) as Box<dyn ProviderEnginePortV1>)
                    .map_err(|error| error.to_string())
            }
            ProviderProtocolV1::Gemini => {
                let limits = GoogleGeminiLimitsV1 {
                    request_timeout: Duration::from_secs(provider.request_timeout_seconds),
                    ..GoogleGeminiLimitsV1::default()
                };
                let config = GoogleGeminiProviderConfig::new(
                    protocol.capability_id(),
                    descriptor.version_hash.clone(),
                    &provider.base_url,
                    provider.model.clone(),
                    api_key,
                    limits,
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
    ) -> Result<(), WorkflowPipelineError> {
        let Some(secret) = secret else {
            return if lease_ids.is_empty() {
                Ok(())
            } else {
                Err(WorkflowPipelineError::IncompleteEvidence)
            };
        };
        if lease_ids.len() != 1 {
            return Err(WorkflowPipelineError::IncompleteEvidence);
        }
        let expected = lease_id(invocation_id, secret)?;
        if lease_ids[0] != expected {
            return Err(WorkflowPipelineError::IncompleteEvidence);
        }

        let mut broker = SecretBroker::with_store(self.store.clone());
        broker
            .restore_credential_metadata(secret.metadata())
            .map_err(|error| WorkflowPipelineError::Host(error.to_string()))?;
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
            .map_err(|error| WorkflowPipelineError::Host(error.to_string()))?;
        self.brokers
            .lock()
            .map_err(|_| WorkflowPipelineError::Host("credential lease lock poisoned".into()))?
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
    event_committer: Arc<dyn SemanticEventCommitter>,
    cancellation_controller: WorkflowCancellationController,
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
            event_committer: self.event_committer.clone(),
            cancellation_controller: self.cancellation_controller.clone(),
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
    event_committer: Arc<dyn SemanticEventCommitter>,
    cancellation_controller: WorkflowCancellationController,
}

impl AdmittedInvocationDispatcherV1 for ModelInvocationDispatcher {
    type Output = Result<ProviderOutcomeRecordV1, WorkflowPipelineError>;

    fn dispatch(
        &self,
        envelope: &ApprovedInvocationEnvelopeV1,
        _admission: &AdmissionReceipt,
        cancellation: &CancellationToken,
    ) -> Self::Output {
        let _active_workflow = self
            .cancellation_controller
            .register(
                self.prepared.snapshot.chat_id.as_str(),
                self.prepared.snapshot.run_id.as_str(),
                cancellation.clone(),
            )
            .map_err(WorkflowPipelineError::Host)?;
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
                status: WorkflowExecutionStatusV1::FailedDefinitelyNotStarted,
                assistant_text: None,
                reasoning: None,
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
                legacy_run_activity: Vec::new(),
                node_activity: Vec::new(),
                approval: None,
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
                    status: WorkflowExecutionStatusV1::FailedDefinitelyNotStarted,
                    assistant_text: None,
                    reasoning: None,
                    error: Some(error.to_string()),
                    model: self.provider.model.clone(),
                    input_units: 0,
                    output_units: 0,
                    attempted_model_turns: 0,
                    settled_tool_calls: 0,
                    tool_exchanges: Vec::new(),
                    tool_activity: Vec::new(),
                    legacy_run_activity: Vec::new(),
                    node_activity: Vec::new(),
                    approval: None,
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
                        status: WorkflowExecutionStatusV1::FailedDefinitelyNotStarted,
                        assistant_text: None,
                        reasoning: None,
                        error: Some(format!("credential lease materialization failed: {error}")),
                        model: self.provider.model.clone(),
                        input_units: 0,
                        output_units: 0,
                        attempted_model_turns: 0,
                        settled_tool_calls: 0,
                        tool_exchanges: Vec::new(),
                        tool_activity: Vec::new(),
                        legacy_run_activity: Vec::new(),
                        node_activity: Vec::new(),
                        approval: None,
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
                        status: WorkflowExecutionStatusV1::FailedDefinitelyNotStarted,
                        assistant_text: None,
                        reasoning: None,
                        error: Some("credential API-key field is not valid UTF-8".to_owned()),
                        model: self.provider.model.clone(),
                        input_units: 0,
                        output_units: 0,
                        attempted_model_turns: 0,
                        settled_tool_calls: 0,
                        tool_exchanges: Vec::new(),
                        tool_activity: Vec::new(),
                        legacy_run_activity: Vec::new(),
                        node_activity: Vec::new(),
                        approval: None,
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
                    status: WorkflowExecutionStatusV1::FailedDefinitelyNotStarted,
                    assistant_text: None,
                    reasoning: None,
                    error: Some(redact_error(&materialized, &error)),
                    model: self.provider.model.clone(),
                    input_units: 0,
                    output_units: 0,
                    attempted_model_turns: 0,
                    settled_tool_calls: 0,
                    tool_exchanges: Vec::new(),
                    tool_activity: Vec::new(),
                    legacy_run_activity: Vec::new(),
                    node_activity: Vec::new(),
                    approval: None,
                    scheduler_checkpoint: None,
                    scheduler_trace: Vec::new(),
                });
            }
        };
        let Some(context) = envelope.payload.get("context") else {
            return self.persist(ProviderOutcomeRecordV1 {
                schema_version: 1,
                invocation_id: envelope.invocation_id.clone(),
                status: WorkflowExecutionStatusV1::FailedDefinitelyNotStarted,
                assistant_text: None,
                reasoning: None,
                error: Some("worker proposal did not contain a model context".to_owned()),
                model: self.provider.model.clone(),
                input_units: 0,
                output_units: 0,
                attempted_model_turns: 0,
                settled_tool_calls: 0,
                tool_exchanges: Vec::new(),
                tool_activity: Vec::new(),
                legacy_run_activity: Vec::new(),
                node_activity: Vec::new(),
                approval: None,
                scheduler_checkpoint: None,
                scheduler_trace: Vec::new(),
            });
        };
        let run_events = Arc::new(RunEventStream::new(
            self.prepared.request_id.to_string(),
            self.prepared.snapshot.run_id.to_string(),
            self.event_committer.clone(),
            cancellation.clone(),
        ));
        run_events
            .ensure_healthy()
            .map_err(WorkflowPipelineError::Store)?;
        let model_observer = Arc::new(ModelRunEventObserver::new(run_events.clone()));
        let gateway =
            Arc::new(FrozenModelGateway::new(vec![provider]).with_observer(model_observer.clone()));
        let workflow = envelope
            .payload
            .get("config")
            .and_then(|config| config.get("workflow"))
            .cloned()
            .ok_or_else(|| {
                WorkflowPipelineError::InvalidInput(
                    "worker proposal did not contain a frozen workflow JSON document".to_owned(),
                )
            })?;
        let outcome = {
            let conversation: Vec<WorkflowMessageV1> = serde_json::from_value(
                context
                    .get("messages")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
            )
            .map_err(json_error)?;
            let workspace = self
                .prepared
                .workspace
                .clone()
                .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
            let authority = self.file_tool_authority.bind_with_run_events(
                FrozenFileToolAuthorityContextV1 {
                    manifest: self.prepared.manifest.clone(),
                    run_id: self.prepared.snapshot.run_id.clone(),
                    request_id: self.prepared.request_id.clone(),
                    node_id: self.prepared.worker_proposal.node_id.clone(),
                    workspace,
                    project_branch: self.prepared.project_branch.clone(),
                    bindings: self.prepared.tool_bindings.clone(),
                    deadline_epoch_millis: self.prepared.deadline_epoch_millis,
                    model_gateway: Some(gateway.clone()),
                    model_binding_id: Some(self.descriptor.capability_id.clone()),
                    model_version_hash: Some(self.descriptor.version_hash.clone()),
                    maximum_tool_output_bytes: self.prepared.provider.maximum_tool_output_bytes,
                    mcp_manifests: self.prepared.mcp_manifests.clone(),
                    cancellation: cancellation.clone(),
                },
                run_events.clone(),
            );
            let compiled = compile_graph_pass(&workflow, &self.prepared.tool_bindings)
                .map_err(|error| WorkflowPipelineError::InvalidInput(error))?;
            let graph_observer = |activity: &GraphNodeActivityV1| {
                if !matches!(activity.status.as_str(), "started" | "waiting") {
                    model_observer.settle(&activity.status);
                }
                run_events.publish_graph_activity(activity);
            };
            let pass = execute_graph_pass_observed(
                &compiled,
                &conversation,
                GraphPassBudgetV1 {
                    tokens: self.prepared.snapshot.budget.tokens,
                    maximum_timeout_recoveries: self.prepared.maximum_timeout_recoveries,
                    maximum_tool_output_bytes: self.prepared.provider.maximum_tool_output_bytes,
                },
                &gateway,
                &authority,
                &envelope.invocation_id,
                self.prepared.request_id.as_str(),
                self.prepared.snapshot.chat_id.as_str(),
                self.prepared.snapshot.run_id.as_str(),
                &self.descriptor.capability_id,
                &self.descriptor.version_hash,
                current_epoch_millis(),
                self.prepared.deadline_epoch_millis,
                None,
                None,
                cancellation,
                Some(&graph_observer),
            );
            model_observer.settle(graph_pass_live_status(pass.status));
            run_events
                .ensure_healthy()
                .map_err(WorkflowPipelineError::Store)?;
            let reasoning = model_observer
                .reasoning_snapshot()
                .map(|(body, category)| WorkflowReasoningActivityV1 { body, category });
            let base = ProviderOutcomeRecordV1 {
                schema_version: 1,
                invocation_id: envelope.invocation_id.clone(),
                status: WorkflowExecutionStatusV1::FailedKnownStarted,
                assistant_text: None,
                reasoning: reasoning.clone(),
                error: None,
                model: self.provider.model.clone(),
                input_units: pass.input_units,
                output_units: pass.output_units,
                attempted_model_turns: pass.attempted_model_turns,
                settled_tool_calls: pass.settled_tool_calls,
                tool_exchanges: pass.exchanges.clone(),
                tool_activity: pass.tool_activity.clone(),
                legacy_run_activity: Vec::new(),
                node_activity: pass.activity.clone(),
                approval: pass.approval.clone(),
                scheduler_checkpoint: None,
                scheduler_trace: Vec::new(),
            };
            match pass.status {
                GraphPassStatusV1::Succeeded => ProviderOutcomeRecordV1 {
                    status: WorkflowExecutionStatusV1::Succeeded,
                    assistant_text: pass.assistant_text,
                    error: None,
                    ..base
                },
                GraphPassStatusV1::Failed => ProviderOutcomeRecordV1 {
                    status: graph_failure_status(pass.error.as_deref()),
                    assistant_text: None,
                    error: pass.error,
                    ..base
                },
                GraphPassStatusV1::AwaitingApproval => {
                    let mut pending = pass
                        .pending_state
                        .clone()
                        .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
                    pending.reasoning_body = reasoning.as_ref().map(|item| item.body.clone());
                    pending.reasoning_category =
                        reasoning.as_ref().map(|item| item.category.clone());
                    self.records.store_pending_approval(&pending)?;
                    ProviderOutcomeRecordV1 {
                        status: WorkflowExecutionStatusV1::AwaitingApproval,
                        assistant_text: None,
                        error: None,
                        ..base
                    }
                }
            }
        };
        if outcome.status == WorkflowExecutionStatusV1::AwaitingApproval {
            // The pending approval record is the durable suspension point; the
            // invocation stays unsettled until a decision resumes the pass.
            return Ok(outcome);
        }
        self.persist(outcome)
    }
}

impl ModelInvocationDispatcher {
    fn persist(
        &self,
        mut outcome: ProviderOutcomeRecordV1,
    ) -> Result<ProviderOutcomeRecordV1, WorkflowPipelineError> {
        validate_provider_outcome_accounting(&self.prepared, &outcome)?;
        compact_provider_outcome_if_needed(&mut outcome)?;
        enforce_serialized_bound(
            &outcome,
            MAXIMUM_PROVIDER_OUTCOME_BYTES,
            "provider outcome record",
        )?;
        self.records.record_outcome(&outcome)?;
        Ok(outcome)
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
    fn open(path: &Path) -> Result<Self, WorkflowPipelineError> {
        Ok(Self {
            store: LocalHistoryStore::open(path).map_err(local_store_error)?,
            write_lock: Arc::new(Mutex::new(())),
        })
    }

    fn record_execution(
        &self,
        record: &PreparedExecutionRecordV1,
    ) -> Result<bool, WorkflowPipelineError> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| WorkflowPipelineError::Store("record lock poisoned".into()))?;
        for value in self.execution_values()? {
            if value.get("requestId").and_then(Value::as_str)
                == Some(record.request_id.as_str())
            {
                let existing = decode_execution(value)?;
                return if existing == *record {
                    Ok(true)
                } else {
                    Err(WorkflowPipelineError::Store(
                        "request ID was reused with changed frozen execution semantics".to_owned(),
                    ))
                };
            }
            let same_run = value.pointer("/snapshot/chatId").and_then(Value::as_str)
                == Some(record.snapshot.chat_id.as_str())
                && value.pointer("/snapshot/runId").and_then(Value::as_str)
                    == Some(record.snapshot.run_id.as_str());
            let same_continuation = value
                .get("schedulerCheckpoint")
                .is_some_and(|checkpoint| !checkpoint.is_null())
                && value
                    .get("schedulerContinuation")
                    .and_then(Value::as_u64)
                    .unwrap_or_default()
                    == record.scheduler_continuation;
            if same_run && same_continuation {
                return Err(WorkflowPipelineError::Store(
                    "this frozen Run continuation already has a different durable request"
                        .to_owned(),
                ));
            }
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
    ) -> Result<bool, WorkflowPipelineError> {
        if let Some(existing) = self.outcome(&outcome.invocation_id)? {
            return if existing == *outcome {
                Ok(true)
            } else {
                Err(WorkflowPipelineError::Store(
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
    ) -> Result<(), WorkflowPipelineError> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| WorkflowPipelineError::Store("record lock poisoned".into()))?;
        self.append_record_without_lock(kind, dedup_key, record)
    }

    fn append_record_without_lock(
        &self,
        kind: &str,
        dedup_key: &StableId,
        record: Value,
    ) -> Result<(), WorkflowPipelineError> {
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
    ) -> Result<Option<PreparedExecutionRecordV1>, WorkflowPipelineError> {
        for value in self.execution_values()? {
            if value.get("requestId").and_then(Value::as_str) == Some(request_id.as_str()) {
                return decode_execution(value).map(Some);
            }
        }
        Ok(None)
    }

    fn execution_values(&self) -> Result<Vec<Value>, WorkflowPipelineError> {
        self.events_of_kind("pipeline.execution-prepared")
    }

    fn execution_for_dispatch(
        &self,
        dispatch: &ApprovedDispatchV1,
    ) -> Result<Option<PreparedExecutionRecordV1>, WorkflowPipelineError> {
        for value in self.execution_values()? {
            if value
                .pointer("/brokerProposal/proposal_id")
                .and_then(Value::as_str)
                == Some(dispatch.proposal_id.as_str())
            {
                return decode_execution(value).map(Some);
            }
        }
        Ok(None)
    }

    fn execution_for_chat_or_run(
        &self,
        chat_id: &StableId,
        run_id: &StableId,
    ) -> Result<Option<PreparedExecutionRecordV1>, WorkflowPipelineError> {
        for value in self.execution_values()? {
            let chat_matches = value.pointer("/snapshot/chatId").and_then(Value::as_str)
                == Some(chat_id.as_str());
            let run_matches = value.pointer("/snapshot/runId").and_then(Value::as_str)
                == Some(run_id.as_str());
            if chat_matches || run_matches {
                let record = decode_execution(value)?;
                if record.snapshot.chat_id != *chat_id || record.snapshot.run_id != *run_id {
                    return Err(WorkflowPipelineError::Store(
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
    ) -> Result<Option<ProviderOutcomeRecordV1>, WorkflowPipelineError> {
        Ok(self
            .outcomes()?
            .into_iter()
            .find(|outcome| &outcome.invocation_id == invocation_id))
    }

    fn outcomes(&self) -> Result<Vec<ProviderOutcomeRecordV1>, WorkflowPipelineError> {
        self.events_of_kind("pipeline.provider-outcome")?
            .into_iter()
            .map(|event| serde_json::from_value(event).map_err(json_error))
            .collect()
    }

    fn events_of_kind(&self, kind: &str) -> Result<Vec<Value>, WorkflowPipelineError> {
        Ok(self
            .store
            .events(PIPELINE_CHAT_ID, STORE_BRANCH_ID)
            .map_err(local_store_error)?
            .into_iter()
            .filter(|event| event.kind == kind)
            .filter_map(|event| event.payload.get("record").cloned())
            .collect())
    }

    fn store_pending_approval(
        &self,
        pending: &PendingGraphPassStateV1,
    ) -> Result<bool, WorkflowPipelineError> {
        let decision_id = stable(&pending.decision_id)?;
        self.append_record(
            "pipeline.pending-approval",
            &decision_id,
            serde_json::to_value(pending).map_err(json_error)?,
        )?;
        Ok(false)
    }

    fn pending_approvals(&self) -> Result<Vec<PendingGraphPassStateV1>, WorkflowPipelineError> {
        self.events_of_kind("pipeline.pending-approval")?
            .into_iter()
            .map(|event| serde_json::from_value(event).map_err(json_error))
            .collect()
    }

    fn pending_approval(
        &self,
        decision_id: &str,
    ) -> Result<Option<PendingGraphPassStateV1>, WorkflowPipelineError> {
        Ok(self
            .pending_approvals()?
            .into_iter()
            .find(|pending| pending.decision_id == decision_id))
    }

    fn pending_approval_for_invocation(
        &self,
        invocation_id: &StableId,
    ) -> Result<Option<PendingGraphPassStateV1>, WorkflowPipelineError> {
        Ok(self
            .pending_approvals()?
            .into_iter()
            .find(|pending| pending.invocation_id == invocation_id.as_str()))
    }

    fn mark_approval_resolved(
        &self,
        decision_id: &StableId,
    ) -> Result<bool, WorkflowPipelineError> {
        if self
            .events_of_kind("pipeline.approval-resolved")?
            .iter()
            .any(|event| {
                event.get("decisionId").and_then(Value::as_str) == Some(decision_id.as_str())
            })
        {
            return Ok(true);
        }
        self.append_record(
            "pipeline.approval-resolved",
            decision_id,
            json!({"decisionId": decision_id.as_str()}),
        )?;
        Ok(false)
    }

    fn approval_resolved(&self, decision_id: &str) -> Result<bool, WorkflowPipelineError> {
        Ok(self
            .events_of_kind("pipeline.approval-resolved")?
            .iter()
            .any(|event| event.get("decisionId").and_then(Value::as_str) == Some(decision_id)))
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
    fn open(path: &Path) -> Result<Self, WorkflowPipelineError> {
        Self::open_scoped(path, BROKER_CHAT_ID, HOST_DESTINATION, WORKER_DESTINATION)
    }

    pub(super) fn open_scoped(
        path: &Path,
        aggregate_id: &str,
        host_destination: &str,
        worker_destination: &str,
    ) -> Result<Self, WorkflowPipelineError> {
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
    ) -> Result<Option<(String, bool)>, WorkflowPipelineError> {
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
    ) -> Result<Option<StableId>, WorkflowPipelineError> {
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

fn validate_provider_outcome_accounting(
    prepared: &PreparedExecutionRecordV1,
    outcome: &ProviderOutcomeRecordV1,
) -> Result<(), WorkflowPipelineError> {
    if outcome.input_units.saturating_add(outcome.output_units) > prepared.snapshot.budget.tokens {
        return Err(WorkflowPipelineError::IncompleteEvidence);
    }
    Ok(())
}

fn compact_provider_outcome_if_needed(
    outcome: &mut ProviderOutcomeRecordV1,
) -> Result<(), WorkflowPipelineError> {
    if serialized_len(outcome)? <= MAXIMUM_PROVIDER_OUTCOME_BYTES {
        return Ok(());
    }
    outcome.status = if outcome.attempted_model_turns == 0 {
        WorkflowExecutionStatusV1::FailedDefinitelyNotStarted
    } else {
        WorkflowExecutionStatusV1::FailedKnownStarted
    };
    outcome.assistant_text = None;
    outcome.error = Some(
        "Provider/tool evidence exceeded the durable outcome bound; large exchange bodies were omitted after their individual authority outcomes were committed."
            .into(),
    );
    outcome.tool_exchanges.clear();
    if serialized_len(outcome)? > MAXIMUM_PROVIDER_OUTCOME_BYTES {
        return Err(WorkflowPipelineError::IncompleteEvidence);
    }
    Ok(())
}

fn serialized_len<T: Serialize>(value: &T) -> Result<usize, WorkflowPipelineError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(json_error)
}

fn enforce_serialized_bound<T: Serialize>(
    value: &T,
    maximum: usize,
    label: &str,
) -> Result<(), WorkflowPipelineError> {
    if serialized_len(value)? <= maximum {
        Ok(())
    } else {
        Err(WorkflowPipelineError::InvalidInput(format!(
            "{label} exceeds its persistence-safe byte bound"
        )))
    }
}

/// Compiles any catalog-valid v1 workflow document into a frozen worker graph.
/// Every node keeps its saved source for identity validation; agent nodes carry
/// their resolved tool subset and maximum turn count; condition edges carry
/// route predicates.
#[allow(clippy::too_many_lines)]
fn compile_graph_snapshot(
    request: &WorkflowExecutionRequestV1,
    descriptor: &CapabilityDescriptor,
    provider: &StoredProviderBindingV1,
    secret: Option<&StoredSecretBindingV1>,
    tool_bindings: &[StoredFileToolBindingV1],
) -> Result<
    (
        Vec<WorkerNodeV1>,
        Vec<WorkerTransitionV1>,
        Vec<StableId>,
        StableId,
    ),
    WorkflowPipelineError,
> {
    let model_capability = stable(descriptor.capability_id.as_str())?;
    let source_nodes = request
        .workflow_snapshot
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_workflow("nodes are missing"))?;
    let port = || WorkerPortV1 {
        name: "value".to_owned(),
        schema_ref: None,
        required: true,
    };
    let mut nodes = Vec::with_capacity(source_nodes.len());
    let mut entry_nodes = Vec::new();
    let mut model_node_id: Option<StableId> = None;
    for source in source_nodes {
        let object = source
            .as_object()
            .ok_or_else(|| invalid_workflow("node must be an object"))?;
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_workflow("node id is missing"))?;
        let node_type = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_workflow("node type is missing"))?;
        let node_id = stable(id)?;
        let configuration = object
            .get("configuration")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let mut config = serde_json::Map::new();
        config.insert("savedNode".into(), source.clone());
        config.insert(
            "frozenContextHash".into(),
            Value::String(request.frozen_context_hash.clone()),
        );
        let (executor, capability_ref, node_tools) = match node_type {
            "input" => (WorkerExecutorKindV1::Pure, None, Vec::new()),
            "output" => (WorkerExecutorKindV1::Pure, None, Vec::new()),
            "parallel" => (WorkerExecutorKindV1::Pure, None, Vec::new()),
            "approval" => (WorkerExecutorKindV1::Pure, None, Vec::new()),
            "wait" => (WorkerExecutorKindV1::Wait, None, Vec::new()),
            "completion" => (WorkerExecutorKindV1::Terminal, None, Vec::new()),
            "condition" => (WorkerExecutorKindV1::Router, None, Vec::new()),
            "model_call" => {
                let mut model_config = config.clone();
                model_config.insert(
                    "provider".into(),
                    serde_json::to_value(provider).map_err(json_error)?,
                );
                model_config.insert(
                    "opaqueSecretRef".into(),
                    secret
                        .map(|binding| Value::String(binding.opaque_ref.to_string()))
                        .unwrap_or(Value::Null),
                );
                model_config.insert(
                    "secretRevision".into(),
                    secret
                        .map(|binding| Value::from(binding.revision))
                        .unwrap_or(Value::Null),
                );
                nodes.push(WorkerNodeV1 {
                    node_id: node_id.clone(),
                    node_type: node_type.to_owned(),
                    node_version: 1,
                    contribution_hash: canonical_digest(&json!({
                        "savedNode": source,
                        "modelDescriptor": descriptor.version_hash,
                    }))?,
                    inputs: vec![port()],
                    outputs: vec![port()],
                    executor: WorkerExecutorKindV1::Model,
                    config: Value::Object(model_config),
                    capability_ref: Some(model_capability.clone()),
                    result_schema_ref: None,
                });
                if model_node_id.is_none() {
                    model_node_id = Some(node_id.clone());
                }
                continue;
            }
            "agent" => {
                let tool_ids = configuration
                    .get("toolIds")
                    .and_then(Value::as_array)
                    .map(|tool_ids| {
                        tool_ids
                            .iter()
                            .map(|tool_id| tool_id.as_str().map(str::to_owned))
                            .collect::<Option<Vec<String>>>()
                    })
                    .ok_or_else(|| invalid_workflow("agent toolIds must be an array of strings"))?
                    .unwrap_or_default();
                let mut node_tools = Vec::with_capacity(tool_ids.len());
                for tool_id in tool_ids {
                    let binding = tool_bindings
                        .iter()
                        .find(|binding| binding.capability_id == tool_id)
                        .cloned()
                        .ok_or_else(|| {
                            invalid_workflow("agent binds a tool with no frozen native binding")
                        })?;
                    node_tools.push(binding);
                }
                (
                    WorkerExecutorKindV1::Agent,
                    Some(model_capability.clone()),
                    node_tools,
                )
            }
            "tool" => {
                let tool_id = configuration
                    .get("toolId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid_workflow("tool node has no toolId"))?;
                let binding = tool_bindings
                    .iter()
                    .find(|binding| binding.capability_id == tool_id)
                    .cloned()
                    .ok_or_else(|| {
                        invalid_workflow("tool node binds a tool with no frozen native binding")
                    })?;
                (
                    WorkerExecutorKindV1::Brokered,
                    Some(stable(
                        if binding.capability_id.starts_with(MCP_CAPABILITY_PREFIX) {
                            &binding.internal_id
                        } else {
                            &binding.capability_id
                        },
                    )?),
                    vec![binding],
                )
            }
            other => {
                return Err(invalid_workflow(&format!(
                    "node type '{other}' has no installed executor in this build"
                )));
            }
        };
        let mut config = config.clone();
        config.insert(
            "provider".into(),
            serde_json::to_value(provider).map_err(json_error)?,
        );
        config.insert(
            "opaqueSecretRef".into(),
            secret
                .map(|binding| Value::String(binding.opaque_ref.to_string()))
                .unwrap_or(Value::Null),
        );
        config.insert(
            "secretRevision".into(),
            secret
                .map(|binding| Value::from(binding.revision))
                .unwrap_or(Value::Null),
        );
        config.insert(
            "tools".into(),
            serde_json::to_value(&node_tools).map_err(json_error)?,
        );
        let contribution = json!({
            "savedNode": source,
            "modelDescriptor": descriptor.version_hash,
            "tools": node_tools,
        });
        nodes.push(WorkerNodeV1 {
            node_id: node_id.clone(),
            node_type: node_type.to_owned(),
            node_version: 1,
            contribution_hash: canonical_digest(&contribution)?,
            inputs: vec![port()],
            outputs: vec![port()],
            executor,
            config: Value::Object(config),
            capability_ref,
            result_schema_ref: None,
        });
        if node_type == "input" {
            entry_nodes.push(node_id.clone());
        }
        if node_type == "agent" && model_node_id.is_none() {
            model_node_id = Some(node_id.clone());
        }
    }
    if entry_nodes.len() != 1 {
        return Err(invalid_workflow(
            "an executable v1 workflow requires exactly one input node",
        ));
    }
    let source_edges = request
        .workflow_snapshot
        .get("edges")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_workflow("edges are missing"))?;
    let mut transitions = Vec::with_capacity(source_edges.len());
    for edge in source_edges {
        let object = edge
            .as_object()
            .ok_or_else(|| invalid_workflow("transition must be an object"))?;
        let source = object
            .get("source")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_workflow("transition source is missing"))?;
        let target = object
            .get("target")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_workflow("transition target is missing"))?;
        let transition_id = stable(
            object
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_workflow("transition ID is missing"))?,
        )?;
        let route = object
            .get("configuration")
            .and_then(|configuration| configuration.get("route"))
            .and_then(Value::as_str);
        let predicate = match route {
            Some(route) => json!({"kind": "route", "route": route}),
            None => json!({"always": true}),
        };
        transitions.push(WorkerTransitionV1 {
            transition_id,
            from_node: stable(source)?,
            from_port: "value".into(),
            to_node: stable(target)?,
            to_port: "value".into(),
            priority: 0,
            predicate: Some(predicate),
            declared_loop_id: None,
        });
    }
    Ok((
        nodes,
        transitions,
        entry_nodes,
        model_node_id.ok_or_else(|| {
            invalid_workflow("an executable v1 workflow requires an agent or model_call node")
        })?,
    ))
}

const fn default_provider_request_timeout_seconds() -> u64 {
    300
}

const fn default_maximum_tool_output_bytes() -> usize {
    64 * 1024
}

fn invalid_workflow(message: &str) -> WorkflowPipelineError {
    WorkflowPipelineError::InvalidInput(format!("frozen workflow JSON: {message}"))
}

fn validate_request(
    request: &WorkflowExecutionRequestV1,
    protocol: ProviderProtocolV1,
    descriptor: &CapabilityDescriptor,
) -> Result<(), WorkflowPipelineError> {
    validate_v1_executable_catalog(&request.workflow_snapshot).map_err(|error| {
        WorkflowPipelineError::InvalidInput(format!("workflow graph is not executable: {error}"))
    })?;
    let frozen_tools = freeze_file_tool_bindings(&request.tools)?;
    if request.maximum_timeout_recoveries > PROVIDER_TIMEOUT_RECOVERIES_V1
        || request.provider.request_timeout_seconds == 0
        || request.provider.request_timeout_seconds > MAXIMUM_PROVIDER_REQUEST_TIMEOUT_SECONDS_V1
        || !(1024..=512 * 1024).contains(&request.provider.maximum_tool_output_bytes)
        || frozen_tools.len() != request.tools.len()
    {
        return Err(WorkflowPipelineError::InvalidInput(
            "workflow provider and tool limits do not match the frozen JSON bindings".to_owned(),
        ));
    }
    if request.messages.is_empty()
        || request.deadline_epoch_millis <= request.now_epoch_millis
        || request.deadline_epoch_millis
            != request
                .now_epoch_millis
                .saturating_add(request.budget.deadline_ms)
        || request.budget.turns == 0
        || request.budget.attempts == 0
        || request.budget.tokens == 0
        || request.budget.actions == 0
        || request.budget.deadline_ms == 0
    {
        return Err(WorkflowPipelineError::InvalidInput(
            "messages, deadline, and model budget must be non-empty".to_owned(),
        ));
    }
    if !is_sha256(&request.frozen_context_hash) {
        return Err(WorkflowPipelineError::InvalidInput(
            "frozen Chat context hash must be a canonical sha256 identity".to_owned(),
        ));
    }
    if request.project_branch.as_ref().is_some_and(|branch| {
        request.workspace.is_none()
            || branch.trim().is_empty()
            || branch.len() > 1024
            || branch.chars().any(char::is_control)
    }) {
        return Err(WorkflowPipelineError::InvalidInput(
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
        return Err(WorkflowPipelineError::InvalidInput(
            "messages require supported roles, non-empty content, and a final user turn".to_owned(),
        ));
    }
    // Node instructions belong to the frozen workflow JSON. Conversation
    // history cannot inject a competing provider system layer.
    if request
        .messages
        .iter()
        .any(|message| message.role == "system")
    {
        return Err(WorkflowPipelineError::InvalidInput(
            "workflow conversations must not embed a system message; instructions belong to the frozen JSON nodes"
                .to_owned(),
        ));
    }
    if serde_json::to_vec(&request.messages)
        .map_err(json_error)?
        .len()
        > WORKFLOW_MAX_MESSAGE_CONTEXT_BYTES
    {
        return Err(WorkflowPipelineError::InvalidInput(
            "message context exceeds the frozen input bound".to_owned(),
        ));
    }
    if serialized_len(&request.workflow_snapshot)? > MAXIMUM_WORKFLOW_SNAPSHOT_BYTES {
        return Err(WorkflowPipelineError::InvalidInput(
            "saved workflow snapshot exceeds the executable persistence bound".to_owned(),
        ));
    }
    if descriptor.capability_id != protocol.capability_id() {
        return Err(WorkflowPipelineError::IncompleteEvidence);
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
            .map_err(|error| WorkflowPipelineError::InvalidInput(error.to_string()))?;
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
            .map_err(|error| WorkflowPipelineError::InvalidInput(error.to_string()))?;
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
            .map_err(|error| WorkflowPipelineError::InvalidInput(error.to_string()))?;
        }
    }
    if let Some(metadata) = &request.provider.credential {
        StoredSecretBindingV1::from_metadata(metadata)?;
    }
    Ok(())
}

fn model_descriptor(
    protocol: ProviderProtocolV1,
) -> Result<CapabilityDescriptor, WorkflowPipelineError> {
    let mut descriptor = CapabilityDescriptor::build(
        protocol.capability_id(),
        MODEL_ADAPTER_VERSION,
        CapabilityKind::Model,
        SideEffectClass::NonIdempotent,
    )
    .map_err(|error| WorkflowPipelineError::Host(error.to_string()))?;
    descriptor.guarantees_same_id_deduplication = false;
    descriptor.supports_streaming = matches!(protocol, ProviderProtocolV1::OpenAiCompatible);
    descriptor.supports_cancellation = false;
    descriptor.allowed_scopes = vec![MODEL_SCOPE.to_owned()];
    descriptor.secret_slots = vec![API_KEY_FIELD.to_owned()];
    descriptor.maximum_concurrency = 8;
    descriptor.max_input_bytes = MAXIMUM_INPUT_BYTES;
    descriptor.max_output_bytes = MAXIMUM_OUTPUT_BYTES;
    descriptor
        .rehash()
        .map_err(|error| WorkflowPipelineError::Host(error.to_string()))?;
    Ok(descriptor)
}

fn graph_pass_live_status(status: GraphPassStatusV1) -> &'static str {
    match status {
        GraphPassStatusV1::Succeeded => "completed",
        GraphPassStatusV1::Failed => "failed",
        GraphPassStatusV1::AwaitingApproval => "awaiting_approval",
    }
}

fn graph_failure_status(error: Option<&str>) -> WorkflowExecutionStatusV1 {
    let error = error.unwrap_or_default().to_ascii_lowercase();
    if error.contains("acceptance") && error.contains("ambiguous")
        || error.contains("provider failed")
        || error.contains("cancelled")
    {
        WorkflowExecutionStatusV1::OutcomeUncertain
    } else {
        WorkflowExecutionStatusV1::FailedKnownStarted
    }
}

fn revalidate_optional_project_branch(
    workspace: &WorkspaceBindingV1,
    expected_branch: Option<&str>,
) -> Result<(), WorkflowPipelineError> {
    expected_branch.map_or(Ok(()), |expected| {
        revalidate_git_branch(&workspace.root, expected).map_err(WorkflowPipelineError::Authority)
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

fn outcome_hash_v1(outcome: &ProviderOutcomeRecordV1) -> Result<String, WorkflowPipelineError> {
    canonical_hash(outcome)
}

fn canonical_hash<T: Serialize>(value: &T) -> Result<String, WorkflowPipelineError> {
    let bytes = serde_jcs::to_vec(value).map_err(json_error)?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn canonical_digest<T: Serialize>(value: &T) -> Result<String, WorkflowPipelineError> {
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

fn digest_id(prefix: &str, material: &str) -> Result<StableId, WorkflowPipelineError> {
    stable(&format!("{prefix}.{}", &digest_hex(material)[..40]))
}

fn lease_id(
    invocation_id: &StableId,
    secret: &StoredSecretBindingV1,
) -> Result<StableId, WorkflowPipelineError> {
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

fn stable(value: &str) -> Result<StableId, WorkflowPipelineError> {
    StableId::parse(value.to_owned())
        .map_err(|error| WorkflowPipelineError::InvalidInput(error.to_string()))
}

fn random_generation() -> Result<ProcessGeneration, WorkflowPipelineError> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes)
        .map_err(|_| WorkflowPipelineError::Host("generation randomness failed".into()))?;
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

fn local_store_error(error: StoreError) -> WorkflowPipelineError {
    WorkflowPipelineError::Store(error.to_string())
}

fn store_error(error: std::io::Error) -> WorkflowPipelineError {
    WorkflowPipelineError::Store(error.to_string())
}

fn json_error(error: impl std::fmt::Display) -> WorkflowPipelineError {
    WorkflowPipelineError::Store(error.to_string())
}

fn decode_execution(value: Value) -> Result<PreparedExecutionRecordV1, WorkflowPipelineError> {
    serde_json::from_value(value).map_err(json_error)
}

fn broker_error(error: BrokerError) -> WorkflowPipelineError {
    WorkflowPipelineError::Broker(error.to_string())
}

#[cfg(test)]
mod tests {
    use crate::runtime::documents::bundled_workflow_template;
    use crate::runtime::{
        PROJECT_FILE_GREP_MAXIMUM_MATCHES_V1, PROJECT_FILE_LIST_MAXIMUM_ENTRIES_V1,
        PROJECT_FILE_READ_MAXIMUM_BYTES_V1, PROJECT_FILE_SEARCH_MAXIMUM_RESULTS_V1,
        PROJECT_FILE_WRITE_MAXIMUM_BYTES_V1, WEB_FETCH_MAXIMUM_DOWNLOAD_BYTES_V1,
        WEB_FETCH_MAXIMUM_EXTRACT_BYTES_V1,
    };

    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::runtime::tool_loop::{
        FILE_EDIT_CAPABILITY_ID, FILE_GREP_CAPABILITY_ID, FILE_LIST_CAPABILITY_ID,
        FILE_READ_CAPABILITY_ID, FILE_SEARCH_CAPABILITY_ID, MAXIMUM_TOOL_RESULT_BYTES,
        SUBAGENT_CAPABILITY_ID, TODO_CAPABILITY_ID, WEB_FETCH_CAPABILITY_ID,
        WEB_EXTRACT_CAPABILITY_ID, WEB_SEARCH_CAPABILITY_ID,
    };
    use aworkit_capability_host::{
        McpCallV1, McpCancellationEvidenceV1, McpCatalogV1, McpFeatureSetV1,
        McpInitializeRequestV1, McpInitializeResponseV1, McpPeerCallResultV1, McpPeerErrorV1,
        McpPeerPort, McpServerManifestV1, McpToolDescriptorV1, ModelEventV1, ModelRequestV1,
        ModelToolCallV1, ModelToolDefinitionV1, ModelToolEventV1, ModelToolRequestV1,
        ProviderAcceptanceV1, ProviderError,
    };
    use aworkit_trusted_core::{MemoryCredentialStore, SecretBroker};
    use tempfile::TempDir;

    use super::*;

    mod credentialed_web_search;

    type ToolPipelineSetupV1 = (
        WorkflowExecutionPipeline,
        Arc<MemoryCredentialStore>,
        CredentialMetadataV1,
        Arc<AtomicUsize>,
        Arc<Mutex<Vec<Value>>>,
    );

    #[derive(Clone, Copy)]
    enum ScriptedBehavior {
        Succeed,
        TimeoutThenSucceed,
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
            let call_index = self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(observed) = &self.observed_inputs {
                observed
                    .lock()
                    .expect("observed provider input")
                    .push(request.input.clone());
            }
            match self.behavior {
                ScriptedBehavior::TimeoutThenSucceed if call_index == 0 => {
                    return Err(ProviderError::RequestTimedOut);
                }
                ScriptedBehavior::TimeoutThenSucceed => {
                    assert!(request.input.to_string().contains("timed out"));
                }
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
                        "x".repeat(WORKFLOW_MAX_ASSISTANT_TEXT_BYTES + 1),
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
        WebSearch,
        ReadAndSearch,
        ReadOnly,
        ReadLoop,
        TimeoutThenRead,
        LargeAggregate,
        Escape,
        Malformed,
        ReadThenProviderFailure,
        Edit,
        EditLoop,
        Todo,
        Subagent,
        SubagentNest,
        SubagentLoop,
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
            request: &ModelRequestV1,
            emit: &mut dyn FnMut(ModelEventV1) -> Result<(), ProviderError>,
        ) -> Result<ProviderAcceptanceV1, ProviderError> {
            // Plain model calls (plan/model_call nodes) answer with fixed text;
            // the tool loop is driven through `execute_tool_turn_cancellable`.
            self.calls.fetch_add(1, Ordering::SeqCst);
            let output = if request.input.to_string().contains("openQuestions") {
                r#"{"goal":"Complete the requested project task","openQuestions":[],"evidenceNeeded":["Project files"],"toolOrder":["Read the project files","Search for relevant content"]}"#
            } else {
                "working answer"
            };
            emit(ModelEventV1::AssistantOutput(output.to_owned()))?;
            emit(ModelEventV1::Usage {
                input_tokens: 7,
                output_tokens: 3,
            })?;
            Ok(ProviderAcceptanceV1::Accepted)
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
            let call_index = self.calls.fetch_add(1, Ordering::SeqCst);
            if matches!(self.script, ToolScriptV1::TimeoutThenRead)
                && request.exchanges.is_empty()
                && call_index == 0
            {
                assert!(request.retry_notice.is_none());
                return Err(ProviderError::RequestTimedOut);
            }
            if request.exchanges.is_empty() {
                match self.script {
                    ToolScriptV1::WebSearch => emit(tool_call(
                        "call.web-search",
                        WEB_SEARCH_CAPABILITY_ID,
                        "aworkit_web_search",
                        json!({
                            "query": "current product price",
                            "limit": 5,
                            "freshness": "current",
                        }),
                    ))?,
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
                    ToolScriptV1::ReadOnly
                    | ToolScriptV1::ReadLoop
                    | ToolScriptV1::TimeoutThenRead => {
                        if matches!(self.script, ToolScriptV1::TimeoutThenRead) {
                            assert!(
                                request
                                    .retry_notice
                                    .as_deref()
                                    .is_some_and(|notice| notice.contains("timed out"))
                            );
                        }
                        emit(tool_call(
                            "call.read",
                            FILE_READ_CAPABILITY_ID,
                            "aworkit_read_project_file",
                            json!({"path":"notes.txt"}),
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
                    ToolScriptV1::Edit | ToolScriptV1::EditLoop => {
                        emit(ModelToolEventV1::ReasoningRaw {
                            text: "I need to request approval before editing.\n".into(),
                        })?;
                        emit(tool_call(
                            if matches!(self.script, ToolScriptV1::EditLoop) {
                                "call.edit.1"
                            } else {
                                "call.edit"
                            },
                            "tool.files.edit",
                            "aworkit_edit_project_file",
                            json!({"path":"notes.txt","old_string":"alpha","new_string":"beta"}),
                        ))?;
                    }
                    ToolScriptV1::Todo => emit(tool_call(
                        "call.todo",
                        "tool.todo",
                        "aworkit_todo",
                        json!({"todos":[
                            {"content":"Write tests","status":"in_progress"},
                            {"content":"Fix pipeline","status":"completed"},
                        ]}),
                    ))?,
                    ToolScriptV1::Subagent => {
                        // The child conversation is recognized by the context
                        // marker appended to its single user message; the parent
                        // delegates once, the child reads and searches, and both
                        // loops finish with the fixed completion text.
                        let child_turn = request.input["messages"]
                            .as_array()
                            .and_then(|messages| messages.first())
                            .and_then(|message| message["content"].as_str())
                            .is_some_and(|content| content.contains("Relevant context:"));
                        if child_turn {
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
                        } else {
                            emit(tool_call(
                                "call.subagent",
                                SUBAGENT_CAPABILITY_ID,
                                "aworkit_spawn_subagent",
                                json!({"task":"Summarize the project notes.","context":"notes.txt mentions alpha beta alpha."}),
                            ))?;
                        }
                    }
                    ToolScriptV1::SubagentNest => emit(tool_call(
                        "call.nested",
                        SUBAGENT_CAPABILITY_ID,
                        "aworkit_spawn_subagent",
                        json!({"task":"Nested delegation attempt."}),
                    ))?,
                    ToolScriptV1::SubagentLoop => {
                        let child_turn = request.input["messages"]
                            .as_array()
                            .and_then(|messages| messages.first())
                            .and_then(|message| message["content"].as_str())
                            .is_some_and(|content| content.contains("Relevant context:"));
                        if child_turn {
                            emit(tool_call(
                                "call.loop-read",
                                FILE_READ_CAPABILITY_ID,
                                "aworkit_read_project_file",
                                json!({"path":"notes.txt"}),
                            ))?;
                        } else {
                            emit(tool_call(
                                "call.subagent",
                                SUBAGENT_CAPABILITY_ID,
                                "aworkit_spawn_subagent",
                                json!({"task":"Never finish.","context":"notes.txt"}),
                            ))?;
                        }
                    }
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
            if matches!(self.script, ToolScriptV1::ReadLoop) {
                if !request
                    .retry_notice
                    .as_deref()
                    .is_some_and(|notice| notice.contains("repeating the exact same tool call"))
                {
                    emit(tool_call(
                        &format!("call.read.{}", request.exchanges.len() + 1),
                        FILE_READ_CAPABILITY_ID,
                        "aworkit_read_project_file",
                        json!({"path":"notes.txt"}),
                    ))?;
                    emit(ModelToolEventV1::Usage {
                        input_tokens: 5,
                        output_tokens: 2,
                    })?;
                    return Ok(ProviderAcceptanceV1::Accepted);
                }
            }
            if matches!(self.script, ToolScriptV1::EditLoop)
                && !request
                    .retry_notice
                    .as_deref()
                    .is_some_and(|notice| notice.contains("repeating the exact same tool call"))
            {
                emit(tool_call(
                    &format!("call.edit.{}", request.exchanges.len() + 1),
                    "tool.files.edit",
                    "aworkit_edit_project_file",
                    json!({"path":"notes.txt","old_string":"alpha","new_string":"beta"}),
                ))?;
                emit(ModelToolEventV1::Usage {
                    input_tokens: 5,
                    output_tokens: 2,
                })?;
                return Ok(ProviderAcceptanceV1::Accepted);
            }
            if matches!(self.script, ToolScriptV1::ReadThenProviderFailure) {
                return Err(ProviderError::Failed(
                    "provider failed after one settled tool call".into(),
                ));
            }
            if matches!(self.script, ToolScriptV1::SubagentLoop) {
                // The child keeps requesting the same read until the advisory
                // repeat reminder prompts it to finish normally.
                let child_turn = request.input["messages"]
                    .as_array()
                    .and_then(|messages| messages.first())
                    .and_then(|message| message["content"].as_str())
                    .is_some_and(|content| content.contains("Relevant context:"));
                if child_turn
                    && !request
                        .retry_notice
                        .as_deref()
                        .is_some_and(|notice| notice.contains("repeating the exact same tool call"))
                {
                    emit(tool_call(
                        &format!("call.loop-read.{}", request.exchanges.len() + 1),
                        FILE_READ_CAPABILITY_ID,
                        "aworkit_read_project_file",
                        json!({"path":"notes.txt"}),
                    ))?;
                    emit(ModelToolEventV1::Usage {
                        input_tokens: 5,
                        output_tokens: 2,
                    })?;
                    return Ok(ProviderAcceptanceV1::Accepted);
                }
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
        WorkflowExecutionPipeline,
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
        let pipeline = WorkflowExecutionPipeline::compose(
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

    fn request(metadata: CredentialMetadataV1) -> WorkflowExecutionRequestV1 {
        let mut request = WorkflowExecutionRequestV1::bounded(
            stable("command.pipeline-test").expect("request"),
            stable("chat.pipeline-test").expect("chat"),
            stable("run.pipeline-test").expect("run"),
            WorkflowProviderBindingV1 {
                kind: "openai_compatible".to_owned(),
                base_url: "http://127.0.0.1:9876/v1".to_owned(),
                model: "test-model".to_owned(),
                request_timeout_seconds: 300,
                maximum_tool_output_bytes: 512 * 1024,
                credential: Some(metadata),
            },
            vec![WorkflowMessageV1 {
                role: "user".to_owned(),
                content: "Please prove the pipeline works.".to_owned(),
            }],
            current_epoch_millis(),
        );
        request.workflow_snapshot =
            bundled_workflow_template("simple-chat").expect("bundled test workflow");
        request
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
        let pipeline = WorkflowExecutionPipeline::compose(
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
        pipeline: &WorkflowExecutionPipeline,
        metadata: CredentialMetadataV1,
        project: &Path,
        tool_ids: &[&str],
    ) -> WorkflowExecutionRequestV1 {
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
        request.budget.turns = 2;
        request.budget.attempts = 2;
        request.budget.tool_calls = 8;
        request.budget.actions = 10;
        request.tools = tool_ids
            .iter()
            .map(|tool_id| WorkflowToolBindingV1 {
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
                    FILE_LIST_CAPABILITY_ID => json!({
                        "authorityMode":"project_files",
                        "effect":"list",
                        "maximumEntries":PROJECT_FILE_LIST_MAXIMUM_ENTRIES_V1,
                    }),
                    FILE_GREP_CAPABILITY_ID => json!({
                        "authorityMode":"project_files",
                        "effect":"grep",
                        "maximumMatches":PROJECT_FILE_GREP_MAXIMUM_MATCHES_V1,
                    }),
                    TODO_CAPABILITY_ID => json!({"authorityMode":"run_todo"}),
                    WEB_SEARCH_CAPABILITY_ID => serde_json::to_value(
                        aworkit_capability_host::WebSearchConfigurationV1::default(),
                    )
                    .expect("web-search configuration"),
                    WEB_FETCH_CAPABILITY_ID | WEB_EXTRACT_CAPABILITY_ID => json!({
                        "maximumDownloadBytes":WEB_FETCH_MAXIMUM_DOWNLOAD_BYTES_V1,
                        "maximumExtractBytes":WEB_FETCH_MAXIMUM_EXTRACT_BYTES_V1,
                    }),
                    _ => json!({}),
                },
                credential_bindings: Vec::new(),
                definition: None,
            })
            .collect();
        request.workflow_snapshot["nodes"][1]["configuration"]["toolIds"] = json!(tool_ids);
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
            WorkflowExecutionStatusV1::Succeeded,
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
        assert!(prepared.scheduler_checkpoint.is_none());
        assert!(prepared.scheduler_trace.is_empty());
        let durable = pipeline
            .records
            .outcomes()
            .expect("durable outcomes")
            .into_iter()
            .find(|outcome| outcome.status == WorkflowExecutionStatusV1::Succeeded)
            .expect("successful outcome");
        assert!(durable.scheduler_checkpoint.is_none());
        assert!(durable.scheduler_trace.is_empty());
        assert!(
            durable
                .node_activity
                .iter()
                .any(|activity| { activity.node_id == "wait.1" && activity.status == "completed" })
        );

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
        let restarted = WorkflowExecutionPipeline::compose(
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
            WorkflowMessageV1 {
                role: "assistant".into(),
                content: "tool loop complete".into(),
            },
            WorkflowMessageV1 {
                role: "user".into(),
                content: "read it again".into(),
            },
        ]);
        assert!(matches!(
            restarted.preflight(&follow_up),
            Err(WorkflowPipelineError::Authority(_))
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
    fn repeated_tool_calls_receive_an_advisory_reminder_without_a_turn_cap() {
        let root = TempDir::new().expect("root");
        let project = root.path().join("project");
        fs::create_dir(&project).expect("project");
        fs::write(project.join("notes.txt"), b"enough evidence").expect("notes");
        let (pipeline, _store, metadata, calls, observed_results) =
            setup_tool_pipeline(&root, ToolScriptV1::ReadLoop);
        let request = tool_bound_request(&pipeline, metadata, &project, &[FILE_READ_CAPABILITY_ID]);

        let result = pipeline.execute(request).expect("best-effort completion");

        assert_eq!(result.status, WorkflowExecutionStatusV1::Succeeded);
        assert_eq!(result.assistant_text.as_deref(), Some("tool loop complete"));
        assert_eq!((result.model_turns, result.tool_calls), (4, 3));
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        let observed = observed_results.lock().expect("tool results");
        assert_eq!(observed.len(), 3);
        assert!(
            observed
                .iter()
                .all(|result| result["content"] == "enough evidence")
        );
    }

    #[test]
    fn typed_provider_timeout_is_reported_to_the_model_and_recovered_once() {
        let root = TempDir::new().expect("root");
        let project = root.path().join("project");
        fs::create_dir(&project).expect("project");
        fs::write(project.join("notes.txt"), b"recovery evidence").expect("notes");
        let (pipeline, _store, metadata, calls, observed_results) =
            setup_tool_pipeline(&root, ToolScriptV1::TimeoutThenRead);
        let mut request =
            tool_bound_request(&pipeline, metadata, &project, &[FILE_READ_CAPABILITY_ID]);
        request.maximum_timeout_recoveries = 1;
        request.budget.turns = 3;
        request.budget.attempts = 3;
        request.budget.actions = 11;

        let result = pipeline.execute(request).expect("timeout recovery");
        assert_eq!(result.status, WorkflowExecutionStatusV1::Succeeded);
        assert_eq!(result.assistant_text.as_deref(), Some("tool loop complete"));
        assert_eq!(result.model_turns, 3);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(
            observed_results.lock().unwrap()[0]["content"],
            "recovery evidence"
        );
    }

    #[test]
    fn tool_output_is_truncated_with_an_explicit_model_facing_marker() {
        let root = TempDir::new().expect("root");
        let project = root.path().join("project");
        fs::create_dir(&project).expect("project");
        fs::write(project.join("notes.txt"), "é".repeat(4_000)).expect("notes");
        let (pipeline, _store, metadata, _calls, observed_results) =
            setup_tool_pipeline(&root, ToolScriptV1::ReadOnly);
        let mut request =
            tool_bound_request(&pipeline, metadata, &project, &[FILE_READ_CAPABILITY_ID]);
        request.provider.maximum_tool_output_bytes = 1024;

        let result = pipeline.execute(request).expect("truncated tool result");
        assert_eq!(result.status, WorkflowExecutionStatusV1::Succeeded);
        let observed = observed_results.lock().expect("tool results");
        let text = observed[0].as_str().expect("truncated result becomes text");
        assert!(text.len() <= 1024);
        assert!(text.contains("Aworkit: tool output truncated"));
        assert!(text.is_char_boundary(text.len()));
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
            WorkflowExecutionStatusV1::Succeeded,
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
        let malformed = WorkflowExecutionPipeline::compose(
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
        assert_eq!(denied.status, WorkflowExecutionStatusV1::FailedKnownStarted);
        assert!(denied.tool_activity.is_empty());
        assert_eq!(malformed_calls.load(Ordering::SeqCst), 1);

        let mut missing_scope =
            tool_bound_request(&pipeline, metadata, &project, &[FILE_READ_CAPABILITY_ID]);
        missing_scope.request_id = stable("command.pipeline-tool-no-project").expect("request");
        missing_scope.chat_id = stable("chat.pipeline-tool-no-project").expect("chat");
        missing_scope.run_id = stable("run.pipeline-tool-no-project").expect("run");
        missing_scope.workspace = None;
        let missing_scope = pipeline
            .execute(missing_scope)
            .expect("missing project scope is a settled denied tool result");
        assert_eq!(missing_scope.status, WorkflowExecutionStatusV1::Succeeded);
        assert_eq!(missing_scope.tool_activity.len(), 1);
        assert_eq!(missing_scope.tool_activity[0].status, "failed");
        assert_eq!(calls.load(Ordering::SeqCst), 4);
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
        assert_eq!(result.status, WorkflowExecutionStatusV1::OutcomeUncertain);
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
        assert!(durable.scheduler_checkpoint.is_none());
        assert!(
            durable
                .node_activity
                .iter()
                .any(|activity| { activity.node_id == "agent.1" && activity.status == "failed" })
        );
    }

    #[test]
    fn aggregate_tool_context_bound_fails_explicitly_without_journal_failure() {
        let root = TempDir::new().expect("root");
        let project = root.path().join("project");
        fs::create_dir(&project).expect("project");
        // Each byte is valid UTF-8 but expands to a six-byte JSON escape. One
        // canonical 60 KiB read fits; adding another turn exceeds the frozen
        // provider context bound and must fail explicitly before journal commit.
        fs::write(project.join("large.txt"), "\u{1}".repeat(60 * 1024)).expect("large file");
        let (pipeline, _store, metadata, calls, _results) =
            setup_tool_pipeline(&root, ToolScriptV1::LargeAggregate);
        let mut execution_request =
            tool_bound_request(&pipeline, metadata, &project, &[FILE_READ_CAPABILITY_ID]);
        execution_request.budget.turns = 3;
        execution_request.budget.attempts = 3;
        execution_request.budget.actions = 11;

        let result = pipeline
            .execute(execution_request)
            .expect("bounded failure remains durably representable");
        assert_eq!(result.status, WorkflowExecutionStatusV1::FailedKnownStarted);
        assert!(
            result.error.as_deref().is_some_and(|error| {
                error.contains("history byte limit")
                    || error.contains("frozen provider plan is invalid")
            }),
            "{:?}",
            result.error
        );
        assert_eq!((result.model_turns, result.tool_calls), (2, 1));
        assert_eq!(result.tool_activity.len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
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
        assert_eq!(output.status, WorkflowExecutionStatusV1::FailedKnownStarted);
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
                    WorkflowMessageV1 {
                        role: "user".into(),
                        content: format!("{ordinal}:{}", "u".repeat(31 * 1024)),
                    },
                    WorkflowMessageV1 {
                        role: "assistant".into(),
                        content: "a".repeat(31 * 1024),
                    },
                ]
            })
            .chain(std::iter::once(WorkflowMessageV1 {
                role: "user".into(),
                content: "final".into(),
            }))
            .collect();
        assert!(matches!(
            pipeline.execute(oversized_input.clone()),
            Err(WorkflowPipelineError::InvalidInput(_))
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
            Err(WorkflowPipelineError::Authority(_))
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
        assert_eq!(first.status, WorkflowExecutionStatusV1::Succeeded);
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
        assert_eq!(replay.status, WorkflowExecutionStatusV1::Succeeded);
        assert!(replay.replayed);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let reopened_calls = Arc::new(AtomicUsize::new(0));
        let reopened = WorkflowExecutionPipeline::compose(
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
        assert_eq!(after_restart.status, WorkflowExecutionStatusV1::Succeeded);
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
            assert_eq!(result.status, WorkflowExecutionStatusV1::Succeeded);

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
            Err(WorkflowPipelineError::Store(_))
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
            WorkflowMessageV1 {
                role: "user".into(),
                content: "Please prove the pipeline works.".into(),
            },
            WorkflowMessageV1 {
                role: "assistant".into(),
                content: "working answer".into(),
            },
            WorkflowMessageV1 {
                role: "user".into(),
                content: "follow up".into(),
            },
        ];
        let continuation_calls = Arc::new(AtomicUsize::new(0));
        let pipeline = WorkflowExecutionPipeline::compose(
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
        assert_eq!(prepared.scheduler_continuation, 0);
        assert!(prepared.scheduler_checkpoint.is_none());
        assert!(prepared.scheduler_trace.is_empty());
        let outcome = pipeline
            .records
            .outcome(&second.broker_invocation_id)
            .expect("follow-up outcome")
            .expect("durable follow-up outcome");
        assert!(outcome.scheduler_checkpoint.is_none());
        assert!(outcome.scheduler_trace.is_empty());
        assert!(
            outcome
                .node_activity
                .iter()
                .any(|activity| { activity.node_id == "wait.1" && activity.status == "completed" })
        );

        let exact_retry = pipeline
            .execute(follow_up.clone())
            .expect("exact continuation retry");
        assert!(exact_retry.replayed);
        assert_eq!(continuation_calls.load(Ordering::SeqCst), 1);
        drop(pipeline);
        let restart_calls = Arc::new(AtomicUsize::new(0));
        let pipeline = WorkflowExecutionPipeline::compose(
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
            Err(WorkflowPipelineError::Store(_))
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
            WorkflowExecutionStatusV1::FailedDefinitelyNotStarted
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
            WorkflowExecutionStatusV1::FailedDefinitelyNotStarted
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
        assert_eq!(result.status, WorkflowExecutionStatusV1::FailedKnownStarted);
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
        assert_eq!(first.status, WorkflowExecutionStatusV1::OutcomeUncertain);
        assert_eq!((first.model_turns, first.tool_calls), (1, 0));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let replay = pipeline
            .execute(request(metadata))
            .expect("ambiguous replay");
        assert_eq!(replay.status, WorkflowExecutionStatusV1::OutcomeUncertain);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn typed_timeout_on_a_no_tool_model_turn_is_model_visible_and_recovered_once() {
        let root = TempDir::new().expect("root");
        let (pipeline, _credential_store, metadata, calls, _) =
            setup(&root, ScriptedBehavior::TimeoutThenSucceed);
        let mut execution_request = request(metadata);
        execution_request.maximum_timeout_recoveries = 1;
        execution_request.budget.turns = 2;
        execution_request.budget.attempts = 2;
        execution_request.budget.actions = 2;

        let result = pipeline
            .execute(execution_request)
            .expect("plain model timeout recovery");
        assert_eq!(result.status, WorkflowExecutionStatusV1::Succeeded);
        assert_eq!(result.assistant_text.as_deref(), Some("working answer"));
        assert_eq!(result.model_turns, 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn new_execution_does_not_decode_an_unrelated_incompatible_history_record() {
        let root = TempDir::new().expect("root");
        let (pipeline, _credential_store, metadata, calls, _) =
            setup(&root, ScriptedBehavior::Succeed);
        let legacy_request_id =
            stable("command.legacy-incompatible").expect("legacy request ID");
        pipeline
            .records
            .append_record_without_lock(
                "pipeline.execution-prepared",
                &legacy_request_id,
                json!({
                    "requestId": legacy_request_id,
                    "snapshot": {
                        "chatId": "chat.legacy-incompatible",
                        "runId": "run.legacy-incompatible",
                    },
                    "toolBindings": [{
                        "limit": {"kind":"web_search","unsupported_legacy_field":true}
                    }],
                }),
            )
            .expect("legacy history fixture");

        let result = pipeline
            .execute(request(metadata))
            .expect("unrelated new execution");
        assert_eq!(result.status, WorkflowExecutionStatusV1::Succeeded);
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
            Err(WorkflowPipelineError::Store(_))
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
        let pipeline = WorkflowExecutionPipeline::compose(
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
            WorkflowMessageV1 {
                role: "system".into(),
                content: "different system layer".into(),
            },
        );
        assert!(matches!(
            pipeline.execute(mismatched.clone()),
            Err(WorkflowPipelineError::InvalidInput(_))
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
            Err(WorkflowPipelineError::InvalidInput(_))
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
        let reopened = WorkflowExecutionPipeline::compose(
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
        assert_eq!(result.status, WorkflowExecutionStatusV1::OutcomeUncertain);
        assert!(result.replayed);
        assert_eq!(reopened_calls.load(Ordering::SeqCst), 0);
    }

    fn graph_workflow(approval: bool) -> Value {
        let mut nodes = vec![
            json!({"id":"input.1","label":"Input","type":"input","position":{"x":36,"y":205}}),
            json!({"id":"plan.1","label":"Plan","type":"model_call","position":{"x":245,"y":205},"configuration":{"modelTierId":"tier:balanced","instructions":"Produce a plan.","maximumTokens":1024}}),
        ];
        if approval {
            nodes.push(json!({"id":"gate.1","label":"Approve","type":"approval","position":{"x":454,"y":205},"configuration":{"title":"Continue?","message":"Approve the plan."}}));
        }
        nodes.extend([
            json!({"id":"agent.1","label":"Agent","type":"agent","position":{"x":663,"y":205},"configuration":{"modelTierId":"tier:balanced","toolIds":[]}}),
            json!({"id":"output.1","label":"Output","type":"output","position":{"x":872,"y":205}}),
            json!({"id":"wait.1","label":"Wait for input","type":"wait","position":{"x":1081,"y":205}}),
        ]);
        let mut edges = vec![
            json!({"id":"e1","source":"input.1","target":"plan.1"}),
            json!({"id":"e3","source":"agent.1","target":"output.1"}),
            json!({"id":"e4","source":"output.1","target":"wait.1"}),
        ];
        if approval {
            edges.insert(1, json!({"id":"e2a","source":"plan.1","target":"gate.1"}));
            edges.insert(2, json!({"id":"e2b","source":"gate.1","target":"agent.1"}));
        } else {
            edges.insert(1, json!({"id":"e2","source":"plan.1","target":"agent.1"}));
        }
        json!({
            "schemaVersion": 1,
            "id": "workflow.graph-test",
            "name": "Graph test",
            "nodes": nodes,
            "edges": edges
        })
    }

    fn graph_request(metadata: CredentialMetadataV1) -> WorkflowExecutionRequestV1 {
        let mut request = request(metadata);
        request.workflow_snapshot = graph_workflow(false);
        request.frozen_context_hash = format!("sha256:{}", "b".repeat(64));
        request.budget.turns = 4;
        request.budget.attempts = 4;
        request.budget.tool_calls = 0;
        request.budget.actions = 4;
        request
    }

    #[test]
    fn graph_pass_runs_plan_agent_output_wait_with_node_activity() {
        let root = TempDir::new().expect("temporary directory");
        let (pipeline, _store, metadata, calls, _saw_secret) =
            setup(&root, ScriptedBehavior::Succeed);
        let result = pipeline
            .execute(graph_request(metadata))
            .expect("graph pass");
        assert_eq!(result.status, WorkflowExecutionStatusV1::Succeeded);
        assert_eq!(result.assistant_text.as_deref(), Some("working answer"));
        assert_eq!(result.model_turns, 2, "plan + agent completions");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let completed: Vec<&str> = result
            .node_activity
            .iter()
            .filter(|activity| activity.status == "completed")
            .map(|activity| activity.node_id.as_str())
            .collect();
        assert_eq!(
            completed,
            vec!["input.1", "plan.1", "agent.1", "output.1", "wait.1"]
        );
        let input = result
            .node_activity
            .iter()
            .find(|activity| activity.node_id == "input.1" && activity.status == "started")
            .expect("input start activity");
        let output = result
            .node_activity
            .iter()
            .find(|activity| activity.node_id == "input.1" && activity.status == "completed")
            .expect("input completion activity");
        assert_eq!(input.input, output.output);
        assert!(input.input.as_ref().is_some_and(Value::is_string));
    }

    #[test]
    fn graph_pass_suspends_at_approval_and_resumes_with_decision() {
        let root = TempDir::new().expect("temporary directory");
        let (pipeline, _store, metadata, calls, _saw_secret) =
            setup(&root, ScriptedBehavior::Succeed);
        let mut request = graph_request(metadata.clone());
        request.workflow_snapshot = graph_workflow(true);
        let suspended = pipeline.execute(request).expect("suspend");
        assert_eq!(
            suspended.status,
            WorkflowExecutionStatusV1::AwaitingApproval
        );
        let approval = suspended.approval.expect("approval evidence");
        assert_eq!(approval.node_id, "gate.1");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "only the plan completes before the gate suspends"
        );

        let resumed = pipeline
            .resume_approval(&approval.decision_id, true)
            .expect("resume approve");
        assert_eq!(resumed.status, WorkflowExecutionStatusV1::Succeeded);
        assert_eq!(resumed.assistant_text.as_deref(), Some("working answer"));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "plan before the gate + agent after approval"
        );
        let resumed_completed: Vec<&str> = resumed
            .node_activity
            .iter()
            .filter(|activity| activity.status == "completed")
            .map(|activity| activity.node_id.as_str())
            .collect();
        assert_eq!(
            resumed_completed,
            vec![
                "input.1", "plan.1", "gate.1", "agent.1", "output.1", "wait.1"
            ]
        );
        // Reapplying the same decision is idempotent and rejected.
        assert!(
            pipeline
                .resume_approval(&approval.decision_id, true)
                .is_err()
        );
    }

    #[test]
    fn graph_pass_rejection_fails_without_completing_downstream_nodes() {
        let root = TempDir::new().expect("temporary directory");
        let (pipeline, _store, metadata, calls, _saw_secret) =
            setup(&root, ScriptedBehavior::Succeed);
        let mut request = graph_request(metadata);
        request.workflow_snapshot = graph_workflow(true);
        let suspended = pipeline.execute(request).expect("suspend");
        let approval = suspended.approval.expect("approval evidence");
        let rejected = pipeline
            .resume_approval(&approval.decision_id, false)
            .expect("resume reject");
        assert_eq!(
            rejected.status,
            WorkflowExecutionStatusV1::FailedKnownStarted
        );
        assert!(rejected.error.unwrap_or_default().contains("rejected"));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "only the pre-gate plan completed; rejection blocks downstream work"
        );
        assert!(rejected.assistant_text.is_none());
    }

    /// Creates a Git-backed project workspace containing notes.txt ("alpha")
    /// for the edit-approval integration tests.
    fn edit_approval_project(root: &TempDir) -> std::path::PathBuf {
        let project = root.path().join("project");
        fs::create_dir(&project).expect("project");
        fs::create_dir(project.join(".git")).expect("git metadata");
        fs::write(project.join(".git/HEAD"), b"ref: refs/heads/main\n").expect("Git HEAD");
        fs::write(project.join("notes.txt"), b"alpha").expect("notes");
        project
    }

    /// Graph workflow whose agent node holds the approval-required edit tool:
    /// input -> agent(tool.files.edit) -> output -> wait.
    fn edit_approval_workflow() -> Value {
        json!({
            "schemaVersion": 1,
            "id": "workflow.edit-approval-test",
            "name": "Edit approval test",
            "nodes": [
                json!({"id":"input.1","label":"Input","type":"input","position":{"x":36,"y":205}}),
                json!({"id":"agent.1","label":"Agent","type":"agent","position":{"x":245,"y":205},"configuration":{
                    "modelTierId":"tier:balanced",
                    "toolIds":["tool.files.edit"],
                    "instructions":"Edit the project file."
                }}),
                json!({"id":"output.1","label":"Output","type":"output","position":{"x":454,"y":205}}),
                json!({"id":"wait.1","label":"Wait for input","type":"wait","position":{"x":663,"y":205}}),
            ],
            "edges": [
                json!({"id":"e1","source":"input.1","target":"agent.1"}),
                json!({"id":"e2","source":"agent.1","target":"output.1"}),
                json!({"id":"e3","source":"output.1","target":"wait.1"}),
            ]
        })
    }

    /// Graph-mode execution request binding the edit tool against the frozen
    /// project workspace with per-invocation approval.
    fn edit_approval_request(
        pipeline: &WorkflowExecutionPipeline,
        metadata: CredentialMetadataV1,
        project: &Path,
    ) -> WorkflowExecutionRequestV1 {
        let mut request = request(metadata);
        request.request_id = stable("command.pipeline-edit-approval-test").expect("request");
        request.chat_id = stable("chat.pipeline-edit-approval-test").expect("chat");
        request.run_id = stable("run.pipeline-edit-approval-test").expect("run");
        request.workspace = Some(
            pipeline
                .projects
                .resolve_workspace_v1(project)
                .expect("workspace"),
        );
        request.budget.turns = 4;
        request.budget.attempts = 4;
        request.budget.tool_calls = 8;
        request.budget.actions = 8;
        request.tools = vec![WorkflowToolBindingV1 {
            capability_id: FILE_EDIT_CAPABILITY_ID.into(),
            configuration: json!({
                "authorityMode": "project_files",
                "effect": "write",
                "requiresApproval": true,
                "maximumBytes": PROJECT_FILE_WRITE_MAXIMUM_BYTES_V1,
            }),
            credential_bindings: Vec::new(),
            definition: None,
        }];
        request.workflow_snapshot = edit_approval_workflow();
        request.frozen_context_hash = format!("sha256:{}", "c".repeat(64));
        request
    }

    #[test]
    fn graph_agent_tool_approval_suspends_and_approval_edits_the_file() {
        let root = TempDir::new().expect("temporary directory");
        let project = edit_approval_project(&root);
        let (pipeline, _store, metadata, calls, observed_results) =
            setup_tool_pipeline(&root, ToolScriptV1::Edit);
        let mut execution_request = edit_approval_request(&pipeline, metadata.clone(), &project);
        execution_request.project_branch = Some("main".into());

        let suspended = pipeline
            .execute(execution_request)
            .expect("suspend on edit tool approval");
        assert_eq!(
            suspended.status,
            WorkflowExecutionStatusV1::AwaitingApproval,
            "{:?}",
            suspended.error
        );
        assert_eq!(
            suspended.reasoning.as_ref(),
            Some(&WorkflowReasoningActivityV1 {
                body: "I need to request approval before editing.\n".into(),
                category: "source_provided".into(),
            }),
            "thinking produced before an approval must survive the durable suspension"
        );
        let approval = suspended.approval.expect("tool approval evidence");
        assert_eq!(approval.node_id, "agent.1");
        assert_eq!(approval.title, "Allow project file edit?");
        assert!(approval.message.contains("notes.txt"));
        assert!(approval.message.contains("alpha"));
        assert!(approval.message.contains("beta"));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "exactly one model turn runs before the edit call suspends"
        );
        assert_eq!(
            fs::read_to_string(project.join("notes.txt")).expect("notes"),
            "alpha",
            "the file must not change before the user approves"
        );
        let waiting: Vec<&str> = suspended
            .node_activity
            .iter()
            .filter(|activity| activity.status == "waiting")
            .map(|activity| activity.node_id.as_str())
            .collect();
        assert_eq!(waiting, vec!["agent.1"]);

        let resumed = pipeline
            .resume_approval(&approval.decision_id, true)
            .expect("resume approve");
        assert_eq!(
            resumed.status,
            WorkflowExecutionStatusV1::Succeeded,
            "{:?}",
            resumed.error
        );
        assert_eq!(
            resumed.assistant_text.as_deref(),
            Some("tool loop complete")
        );
        assert_eq!(resumed.tool_calls, 1);
        assert_eq!(
            fs::read_to_string(project.join("notes.txt")).expect("edited notes"),
            "beta",
            "the approved edit replaces alpha with beta"
        );
        let edit = resumed
            .tool_activity
            .iter()
            .find(|activity| activity.capability_id == FILE_EDIT_CAPABILITY_ID)
            .expect("edit tool activity");
        assert_eq!(edit.status, "completed");
        let observed = observed_results.lock().expect("tool results");
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0]["path"], "notes.txt");
        assert_eq!(observed[0]["oldString"], "alpha");
        assert_eq!(observed[0]["newString"], "beta");
        drop(observed);
        // Reapplying the same decision is idempotent and rejected.
        assert!(
            pipeline
                .resume_approval(&approval.decision_id, true)
                .is_err()
        );
    }

    #[test]
    fn repeated_tool_reminder_survives_approval_suspension_and_resume() {
        let root = TempDir::new().expect("temporary directory");
        let project = edit_approval_project(&root);
        let (pipeline, _store, metadata, calls, observed_results) =
            setup_tool_pipeline(&root, ToolScriptV1::EditLoop);
        let mut execution_request = edit_approval_request(&pipeline, metadata, &project);
        execution_request.project_branch = Some("main".into());

        let first = pipeline
            .execute(execution_request)
            .expect("first edit approval");
        assert_eq!(first.status, WorkflowExecutionStatusV1::AwaitingApproval);

        let second = pipeline
            .resume_approval(&first.approval.expect("first approval").decision_id, true)
            .expect("second edit approval");
        assert_eq!(second.status, WorkflowExecutionStatusV1::AwaitingApproval);

        let third = pipeline
            .resume_approval(&second.approval.expect("second approval").decision_id, true)
            .expect("third edit approval");
        assert_eq!(third.status, WorkflowExecutionStatusV1::AwaitingApproval);

        let completed = pipeline
            .resume_approval(&third.approval.expect("third approval").decision_id, true)
            .expect("finish after repeated-call reminder");
        assert_eq!(completed.status, WorkflowExecutionStatusV1::Succeeded);
        assert_eq!(
            completed.assistant_text.as_deref(),
            Some("tool loop complete")
        );
        assert_eq!((completed.model_turns, completed.tool_calls), (4, 3));
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        assert_eq!(
            observed_results.lock().expect("tool results").len(),
            3,
            "all three settled approval-gated calls must reach the model"
        );
    }

    #[test]
    fn graph_agent_tool_approval_rejection_denies_the_call_without_mutation() {
        let root = TempDir::new().expect("temporary directory");
        let project = edit_approval_project(&root);
        let (pipeline, _store, metadata, calls, observed_results) =
            setup_tool_pipeline(&root, ToolScriptV1::Edit);
        let mut execution_request = edit_approval_request(&pipeline, metadata.clone(), &project);
        execution_request.project_branch = Some("main".into());

        let suspended = pipeline
            .execute(execution_request)
            .expect("suspend on edit tool approval");
        let approval = suspended.approval.expect("tool approval evidence");

        let resumed = pipeline
            .resume_approval(&approval.decision_id, false)
            .expect("resume reject");
        // A rejected tool call surfaces a denial result and the agent loop
        // continues instead of failing the whole pass.
        assert_eq!(
            resumed.status,
            WorkflowExecutionStatusV1::Succeeded,
            "{:?}",
            resumed.error
        );
        assert_eq!(
            resumed.assistant_text.as_deref(),
            Some("tool loop complete")
        );
        assert_eq!(
            fs::read_to_string(project.join("notes.txt")).expect("notes"),
            "alpha",
            "the rejected edit leaves the file untouched"
        );
        assert_eq!(resumed.tool_calls, 1);
        let edit = resumed
            .tool_activity
            .iter()
            .find(|activity| activity.capability_id == FILE_EDIT_CAPABILITY_ID)
            .expect("edit tool activity");
        assert_eq!(edit.status, "denied");
        let observed = observed_results.lock().expect("tool results");
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0]["error"], "user_rejected");
        drop(observed);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn provider_todo_call_records_run_local_todo_state() {
        let root = TempDir::new().expect("root");
        let project = root.path().join("project");
        fs::create_dir(&project).expect("project");
        fs::create_dir(project.join(".git")).expect("git metadata");
        fs::write(project.join(".git/HEAD"), b"ref: refs/heads/main\n").expect("Git HEAD");
        let (pipeline, _store, metadata, calls, observed_results) =
            setup_tool_pipeline(&root, ToolScriptV1::Todo);
        let mut execution_request =
            tool_bound_request(&pipeline, metadata.clone(), &project, &[TODO_CAPABILITY_ID]);
        execution_request.project_branch = Some("main".into());

        let first = pipeline
            .execute(execution_request.clone())
            .expect("todo execution");
        assert_eq!(
            first.status,
            WorkflowExecutionStatusV1::Succeeded,
            "{:?}",
            first.error
        );
        assert_eq!(first.assistant_text.as_deref(), Some("tool loop complete"));
        assert_eq!(first.tool_calls, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let todo = first
            .tool_activity
            .iter()
            .find(|activity| activity.capability_id == TODO_CAPABILITY_ID)
            .expect("todo tool activity");
        assert_eq!(todo.status, "completed");
        let stored = pipeline
            .run_todo_state(&execution_request.run_id)
            .expect("todo state")
            .expect("stored todo list");
        assert_eq!(
            stored,
            json!([
                {"content":"Write tests","status":"in_progress"},
                {"content":"Fix pipeline","status":"completed"},
            ])
        );
        let observed = observed_results.lock().expect("tool results");
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0]["todos"], stored);
    }

    #[test]
    fn standard_agent_workflow_runs_plan_agent_output_wait_end_to_end() {
        let root = TempDir::new().expect("temporary directory");
        let project = root.path().join("project");
        fs::create_dir(&project).expect("project");
        fs::create_dir(project.join(".git")).expect("git metadata");
        fs::write(project.join(".git/HEAD"), b"ref: refs/heads/main\n").expect("Git HEAD");
        fs::write(project.join("notes.txt"), b"alpha beta alpha").expect("notes");
        let (pipeline, _store, metadata, calls, observed_results) =
            setup_tool_pipeline(&root, ToolScriptV1::ReadAndSearch);
        let mut execution_request = tool_bound_request(
            &pipeline,
            metadata.clone(),
            &project,
            &[
                FILE_READ_CAPABILITY_ID,
                FILE_SEARCH_CAPABILITY_ID,
                FILE_LIST_CAPABILITY_ID,
                FILE_GREP_CAPABILITY_ID,
                TODO_CAPABILITY_ID,
                WEB_SEARCH_CAPABILITY_ID,
                WEB_EXTRACT_CAPABILITY_ID,
            ],
        );
        // The seeded production workflow: Input -> Plan -> Agent -> Output -> Wait.
        execution_request.workflow_snapshot =
            bundled_workflow_template("standard-agent").expect("bundled Standard Agent");
        execution_request.frozen_context_hash = format!("sha256:{}", "d".repeat(64));
        execution_request.project_branch = Some("main".into());
        execution_request.budget.turns = 9;
        execution_request.budget.attempts = 9;
        execution_request.budget.tool_calls = 64;
        execution_request.budget.actions = 73;

        let result = pipeline
            .execute(execution_request)
            .expect("standard agent pass");
        assert_eq!(
            result.status,
            WorkflowExecutionStatusV1::Succeeded,
            "{:?}",
            result.error
        );
        assert_eq!(result.assistant_text.as_deref(), Some("tool loop complete"));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "plan model call + two agent turns"
        );
        assert_eq!(result.tool_calls, 2, "read + search tool calls settle");
        let completed: Vec<&str> = result
            .node_activity
            .iter()
            .filter(|activity| activity.status == "completed")
            .map(|activity| activity.node_id.as_str())
            .collect();
        assert_eq!(
            completed,
            vec!["input.1", "plan.1", "agent.1", "output.1", "wait.1"]
        );
        let observed = observed_results.lock().expect("tool results");
        assert_eq!(observed.len(), 2);
        assert_eq!(observed[0]["content"], "alpha beta alpha");
        assert_eq!(observed[1]["offsets"], json!([0, 11]));
    }

    /// Git-backed project with notes.txt for the subagent delegation tests.
    fn subagent_project(root: &TempDir) -> std::path::PathBuf {
        let project = root.path().join("project");
        fs::create_dir(&project).expect("project");
        fs::create_dir(project.join(".git")).expect("git metadata");
        fs::write(project.join(".git/HEAD"), b"ref: refs/heads/main\n").expect("Git HEAD");
        fs::write(project.join("notes.txt"), b"alpha beta alpha").expect("notes");
        project
    }

    /// Workflow whose agent node binds exactly one tool id.
    fn single_agent_tool_workflow(tool_id: &str) -> Value {
        json!({
            "schemaVersion": 1,
            "id": "workflow.single-tool-test",
            "name": "Single tool test",
            "nodes": [
                json!({"id":"input.1","label":"Input","type":"input","position":{"x":36,"y":205}}),
                json!({"id":"agent.1","label":"Agent","type":"agent","position":{"x":245,"y":205},"configuration":{
                    "modelTierId":"tier:balanced",
                    "toolIds":[tool_id],
                    "instructions":"Use the tool and finish."
                }}),
                json!({"id":"output.1","label":"Output","type":"output","position":{"x":454,"y":205}}),
                json!({"id":"wait.1","label":"Wait for input","type":"wait","position":{"x":663,"y":205}}),
            ],
            "edges": [
                json!({"id":"e1","source":"input.1","target":"agent.1"}),
                json!({"id":"e2","source":"agent.1","target":"output.1"}),
                json!({"id":"e3","source":"output.1","target":"wait.1"}),
            ]
        })
    }

    /// Graph-mode request delegating through the subagent tool with the given
    /// frozen bindings.
    fn subagent_request(
        pipeline: &WorkflowExecutionPipeline,
        metadata: CredentialMetadataV1,
        project: &Path,
        tool_ids: &[&str],
    ) -> WorkflowExecutionRequestV1 {
        let mut request = request(metadata);
        request.request_id = stable("command.pipeline-subagent-test").expect("request");
        request.chat_id = stable("chat.pipeline-subagent-test").expect("chat");
        request.run_id = stable("run.pipeline-subagent-test").expect("run");
        request.workspace = Some(
            pipeline
                .projects
                .resolve_workspace_v1(project)
                .expect("workspace"),
        );
        request.budget.turns = 6;
        request.budget.attempts = 6;
        request.budget.tool_calls = 12;
        request.budget.actions = 16;
        request.tools = tool_ids
            .iter()
            .map(|tool_id| WorkflowToolBindingV1 {
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
                    SUBAGENT_CAPABILITY_ID => json!({
                        "authorityMode":"run_subagent",
                        "requiresApproval":true,
                    }),
                    _ => json!({}),
                },
                credential_bindings: Vec::new(),
                definition: None,
            })
            .collect();
        request.workflow_snapshot = single_agent_tool_workflow(SUBAGENT_CAPABILITY_ID);
        request.frozen_context_hash = format!("sha256:{}", "e".repeat(64));
        request
    }

    #[test]
    fn subagent_delegation_runs_the_child_loop_and_returns_the_final_text() {
        let root = TempDir::new().expect("temporary directory");
        let project = subagent_project(&root);
        let (pipeline, _store, metadata, calls, observed_results) =
            setup_tool_pipeline(&root, ToolScriptV1::Subagent);
        let mut execution_request = subagent_request(
            &pipeline,
            metadata.clone(),
            &project,
            &[
                FILE_READ_CAPABILITY_ID,
                FILE_SEARCH_CAPABILITY_ID,
                SUBAGENT_CAPABILITY_ID,
            ],
        );
        execution_request.project_branch = Some("main".into());

        // The subagent tool is approval-required: the pass suspends with a
        // durable challenge before the child run starts.
        let suspended = pipeline
            .execute(execution_request)
            .expect("suspend on subagent approval");
        assert_eq!(
            suspended.status,
            WorkflowExecutionStatusV1::AwaitingApproval,
            "{:?}",
            suspended.error
        );
        let approval = suspended.approval.expect("subagent approval");
        assert_eq!(approval.node_id, "agent.1");
        assert_eq!(approval.title, "Allow subagent task?");
        assert!(approval.message.contains(SUBAGENT_CAPABILITY_ID));
        assert!(approval.message.contains("Summarize the project notes."));

        let resumed = pipeline
            .resume_approval(&approval.decision_id, true)
            .expect("resume approve");
        assert_eq!(
            resumed.status,
            WorkflowExecutionStatusV1::Succeeded,
            "{:?}",
            resumed.error
        );
        assert_eq!(
            resumed.assistant_text.as_deref(),
            Some("tool loop complete")
        );
        assert_eq!(resumed.tool_calls, 1, "one subagent call settles");
        let subagent = resumed
            .tool_activity
            .iter()
            .find(|activity| activity.capability_id == SUBAGENT_CAPABILITY_ID)
            .expect("subagent activity");
        assert_eq!(subagent.status, "completed");
        assert!(subagent.summary.contains("Subagent completed"));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            4,
            "parent call + child call + child final + parent final"
        );
        let observed = observed_results.lock().expect("tool results");
        assert_eq!(
            observed.len(),
            3,
            "child results followed by the subagent result"
        );
        assert_eq!(observed[0]["content"], "alpha beta alpha");
        assert_eq!(observed[1]["offsets"], json!([0, 11]));
        assert_eq!(observed[2]["finalText"], "tool loop complete");
        assert_eq!(observed[2]["modelTurns"], 2, "child usage is recorded");
        assert_eq!(observed[2]["toolCalls"], 2);
        assert_eq!(observed[2]["inputTokens"], 14);
        assert_eq!(observed[2]["outputTokens"], 6);
        drop(observed);
        // Reapplying the same decision is idempotent and rejected.
        assert!(
            pipeline
                .resume_approval(&approval.decision_id, true)
                .is_err()
        );
    }

    #[test]
    fn subagent_nesting_is_denied_and_the_failure_reaches_the_parent_loop() {
        let root = TempDir::new().expect("temporary directory");
        let project = subagent_project(&root);
        let (pipeline, _store, metadata, calls, observed_results) =
            setup_tool_pipeline(&root, ToolScriptV1::SubagentNest);
        let mut execution_request = subagent_request(
            &pipeline,
            metadata.clone(),
            &project,
            &[FILE_READ_CAPABILITY_ID, SUBAGENT_CAPABILITY_ID],
        );
        execution_request.project_branch = Some("main".into());

        let suspended = pipeline
            .execute(execution_request)
            .expect("suspend on subagent approval");
        let approval = suspended.approval.expect("subagent approval");
        let resumed = pipeline
            .resume_approval(&approval.decision_id, true)
            .expect("resume approve");
        // The child tried to delegate again; the depth guard failed its loop
        // and the denied result flowed back without failing the parent pass.
        assert_eq!(
            resumed.status,
            WorkflowExecutionStatusV1::Succeeded,
            "{:?}",
            resumed.error
        );
        assert_eq!(
            resumed.assistant_text.as_deref(),
            Some("tool loop complete")
        );
        let subagent = resumed
            .tool_activity
            .iter()
            .find(|activity| activity.capability_id == SUBAGENT_CAPABILITY_ID)
            .expect("subagent activity");
        assert_eq!(subagent.status, "failed");
        let observed = observed_results.lock().expect("tool results");
        assert_eq!(observed.len(), 1);
        // The child's provider turn referenced a tool outside its restricted
        // definitions, so the gateway rejected the turn before the subagent
        // port guard could see the call.
        assert!(
            observed[0]["error"].as_str().is_some_and(
                |error| error.contains("provider tool response is invalid or unsupported")
            ),
            "{:?}",
            observed[0]
        );
        drop(observed);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn subagent_repeated_tools_receive_a_reminder_and_finish_without_a_turn_cap() {
        let root = TempDir::new().expect("temporary directory");
        let project = subagent_project(&root);
        let (pipeline, _store, metadata, calls, observed_results) =
            setup_tool_pipeline(&root, ToolScriptV1::SubagentLoop);
        let mut execution_request = subagent_request(
            &pipeline,
            metadata.clone(),
            &project,
            &[
                FILE_READ_CAPABILITY_ID,
                FILE_SEARCH_CAPABILITY_ID,
                SUBAGENT_CAPABILITY_ID,
            ],
        );
        execution_request.project_branch = Some("main".into());

        let suspended = pipeline
            .execute(execution_request)
            .expect("suspend on subagent approval");
        let approval = suspended.approval.expect("subagent approval");
        let resumed = pipeline
            .resume_approval(&approval.decision_id, true)
            .expect("resume approve");
        assert_eq!(
            resumed.status,
            WorkflowExecutionStatusV1::Succeeded,
            "{:?}",
            resumed.error
        );
        assert_eq!(
            resumed.assistant_text.as_deref(),
            Some("tool loop complete")
        );
        let subagent = resumed
            .tool_activity
            .iter()
            .find(|activity| activity.capability_id == SUBAGENT_CAPABILITY_ID)
            .expect("subagent activity");
        assert_eq!(subagent.status, "completed");
        let observed = observed_results.lock().expect("tool results");
        // The child executes all three reads, receives the advisory reminder,
        // and then completes; the parent receives the normal child result.
        assert_eq!(observed.len(), 4);
        assert!(
            observed[..3]
                .iter()
                .all(|result| result["content"] == "alpha beta alpha")
        );
        assert_eq!(observed[3]["finalText"], "tool loop complete");
        assert_eq!(observed[3]["modelTurns"], 4);
        assert_eq!(observed[3]["toolCalls"], 3);
        drop(observed);
        assert_eq!(calls.load(Ordering::SeqCst), 6);
    }

    // ---------------------------------------------------------------------
    // MCP tools in the agent loop (W6)
    // ---------------------------------------------------------------------

    const MCP_FIXTURE_SERVER: &str = "serv.fixture";
    const MCP_FIXTURE_TOOL: &str = "echo";
    const MCP_FIXTURE_CAPABILITY: &str = "mcp://serv.fixture/echo";
    const MCP_FIXTURE_NAME: &str = "mcp__serv_fixture__echo";

    fn mcp_echo_schema() -> Value {
        json!({
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"]
        })
    }

    fn mcp_echo_definition() -> ModelToolDefinitionV1 {
        ModelToolDefinitionV1 {
            capability_id: MCP_FIXTURE_CAPABILITY.into(),
            name: MCP_FIXTURE_NAME.into(),
            description: "Echo the given text.".into(),
            input_schema: mcp_echo_schema(),
        }
    }

    fn mcp_fixture_manifest(generation: ProcessGeneration) -> McpServerManifestV1 {
        McpServerManifestV1 {
            server_id: stable(MCP_FIXTURE_SERVER).expect("server id"),
            adapter_version: "rmcp-3.1.4".into(),
            binding_hash: format!("sha256:{}", "c".repeat(64)),
            host_generation: generation,
            configured: true,
            enabled: true,
            core_attested: true,
            transport: aworkit_capability_host::McpTransportKindV1::Stdio,
            minimum_protocol_version: 1,
            maximum_protocol_version: 5,
            maximum_in_flight: 1,
            maximum_progress_events: 16,
            secret_slots: Vec::new(),
            workspace_roots: Vec::new(),
        }
    }

    #[derive(Clone, Copy)]
    enum ScriptedMcpBehavior {
        Echo,
        CallFailure,
        OversizedResult,
    }

    struct ScriptedMcpPeer {
        behavior: ScriptedMcpBehavior,
        calls: Arc<AtomicUsize>,
        observed_arguments: Arc<Mutex<Vec<Value>>>,
    }

    impl McpPeerPort for ScriptedMcpPeer {
        fn initialize(
            &self,
            _manifest: &McpServerManifestV1,
            _request: &McpInitializeRequestV1,
        ) -> Result<McpInitializeResponseV1, McpPeerErrorV1> {
            let schema = serde_json::to_vec(&mcp_echo_schema()).unwrap_or_default();
            Ok(McpInitializeResponseV1 {
                server_id: stable(MCP_FIXTURE_SERVER).expect("server id"),
                protocol_version: 2,
                features: McpFeatureSetV1 {
                    tools: true,
                    resources: false,
                    prompts: false,
                    progress: false,
                    cancellation: false,
                },
                catalog: McpCatalogV1 {
                    tools: vec![McpToolDescriptorV1 {
                        name: MCP_FIXTURE_TOOL.into(),
                        input_schema_hash: format!("sha256:{:x}", Sha256::digest(schema)),
                        side_effect_known_read_only: false,
                        description: "Echo the given text.".into(),
                        input_schema: mcp_echo_schema(),
                    }],
                    resources: Vec::new(),
                    prompts: Vec::new(),
                },
            })
        }

        fn invoke(
            &self,
            _manifest: &McpServerManifestV1,
            call: &McpCallV1,
        ) -> Result<McpPeerCallResultV1, McpPeerErrorV1> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.observed_arguments
                .lock()
                .expect("observed mcp arguments")
                .push(call.arguments.clone());
            match self.behavior {
                ScriptedMcpBehavior::Echo => {
                    let text = call.arguments["text"].as_str().unwrap_or_default();
                    Ok(McpPeerCallResultV1 {
                        result: json!({"echo": text}),
                        progress: Vec::new(),
                    })
                }
                ScriptedMcpBehavior::CallFailure => Err(McpPeerErrorV1 {
                    code: "scripted_failure".into(),
                    message: "scripted MCP call failure".into(),
                    dispatch: aworkit_capability_host::McpDispatchMilestoneV1::DefinitelyNotStarted,
                    transport_lost: false,
                }),
                ScriptedMcpBehavior::OversizedResult => Ok(McpPeerCallResultV1 {
                    result: json!({"echo": "x".repeat(MAXIMUM_TOOL_RESULT_BYTES + 1)}),
                    progress: Vec::new(),
                }),
            }
        }

        fn cancel(
            &self,
            _manifest: &McpServerManifestV1,
            _invocation_id: &StableId,
        ) -> Result<McpCancellationEvidenceV1, McpPeerErrorV1> {
            Ok(McpCancellationEvidenceV1::Unsupported)
        }

        fn close(&self, _manifest: &McpServerManifestV1) -> Result<(), McpPeerErrorV1> {
            Ok(())
        }
    }

    struct McpToolProviderFactoryV1 {
        calls: Arc<AtomicUsize>,
        observed_results: Arc<Mutex<Vec<Value>>>,
    }

    impl ProviderFactoryV1 for McpToolProviderFactoryV1 {
        fn create(
            &self,
            descriptor: &CapabilityDescriptor,
            _provider: &StoredProviderBindingV1,
            _api_key: Option<Zeroizing<String>>,
        ) -> Result<Box<dyn ProviderEnginePortV1>, String> {
            Ok(Box::new(McpToolProvider {
                calls: self.calls.clone(),
                observed_results: self.observed_results.clone(),
                binding: descriptor.capability_id.clone(),
                version: descriptor.version_hash.clone(),
            }))
        }
    }

    struct McpToolProvider {
        calls: Arc<AtomicUsize>,
        observed_results: Arc<Mutex<Vec<Value>>>,
        binding: String,
        version: String,
    }

    impl ProviderEnginePortV1 for McpToolProvider {
        fn binding_id(&self) -> &str {
            &self.binding
        }

        fn version_hash(&self) -> &str {
            &self.version
        }

        fn execute(
            &self,
            _request: &ModelRequestV1,
            emit: &mut dyn FnMut(ModelEventV1) -> Result<(), ProviderError>,
        ) -> Result<ProviderAcceptanceV1, ProviderError> {
            // Plain model calls (plan/model_call nodes) answer with fixed text;
            // the MCP tool loop is driven through `execute_tool_turn_cancellable`.
            emit(ModelEventV1::AssistantOutput("working answer".to_owned()))?;
            emit(ModelEventV1::Usage {
                input_tokens: 7,
                output_tokens: 3,
            })?;
            Ok(ProviderAcceptanceV1::Accepted)
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
                emit(ModelToolEventV1::ToolCall {
                    call: ModelToolCallV1 {
                        call_id: "call.echo".into(),
                        provider_call_id: Some("call.echo".into()),
                        capability_id: MCP_FIXTURE_CAPABILITY.into(),
                        name: MCP_FIXTURE_NAME.into(),
                        arguments: json!({"text": "hello"}),
                        provider_context: None,
                    },
                })?;
                emit(ModelToolEventV1::Usage {
                    input_tokens: 5,
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
                .expect("observed mcp results")
                .extend(results);
            emit(ModelToolEventV1::AssistantOutput {
                text: "mcp loop complete".into(),
            })?;
            emit(ModelToolEventV1::Usage {
                input_tokens: 9,
                output_tokens: 4,
            })?;
            Ok(ProviderAcceptanceV1::Accepted)
        }
    }

    fn mcp_graph_request(
        pipeline: &WorkflowExecutionPipeline,
        metadata: CredentialMetadataV1,
    ) -> WorkflowExecutionRequestV1 {
        let mut request = request(metadata);
        request.request_id = stable("command.pipeline-mcp-test").expect("request");
        request.chat_id = stable("chat.pipeline-mcp-test").expect("chat");
        request.run_id = stable("run.pipeline-mcp-test").expect("run");
        request.budget.turns = 4;
        request.budget.attempts = 4;
        request.budget.tool_calls = 4;
        request.budget.actions = 8;
        request.tools = vec![WorkflowToolBindingV1 {
            capability_id: MCP_FIXTURE_CAPABILITY.into(),
            configuration: json!({"serverId": MCP_FIXTURE_SERVER, "tool": MCP_FIXTURE_TOOL}),
            credential_bindings: Vec::new(),
            definition: Some(mcp_echo_definition()),
        }];
        request.mcp_servers = vec![mcp_fixture_manifest(pipeline.generation)];
        request.workflow_snapshot = graph_workflow(false);
        request.workflow_snapshot["id"] = json!("workflow.mcp-test");
        request.workflow_snapshot["nodes"][2]["configuration"]["toolIds"] =
            json!([MCP_FIXTURE_CAPABILITY]);
        request.frozen_context_hash = format!("sha256:{}", "d".repeat(64));
        request
    }

    fn setup_mcp_pipeline(
        root: &TempDir,
        behavior: ScriptedMcpBehavior,
    ) -> (
        WorkflowExecutionPipeline,
        CredentialMetadataV1,
        Arc<AtomicUsize>,
        Arc<Mutex<Vec<Value>>>,
        Arc<Mutex<Vec<Value>>>,
    ) {
        let credential_store = Arc::new(MemoryCredentialStore::default());
        let mut secret_broker = SecretBroker::with_store(credential_store.clone());
        let metadata = secret_broker
            .put_credential(
                CredentialRef(stable("credential.mcp-pipeline-test").expect("credential ID")),
                BTreeMap::from([(API_KEY_FIELD.to_owned(), b"test-secret".to_vec())]),
            )
            .expect("credential");
        let calls = Arc::new(AtomicUsize::new(0));
        let observed_results = Arc::new(Mutex::new(Vec::new()));
        let pipeline = WorkflowExecutionPipeline::compose(
            root.path(),
            credential_store,
            Arc::new(McpToolProviderFactoryV1 {
                calls: calls.clone(),
                observed_results: observed_results.clone(),
            }),
        )
        .expect("mcp pipeline");
        let peer_calls = Arc::new(AtomicUsize::new(0));
        let observed_arguments = Arc::new(Mutex::new(Vec::new()));
        pipeline
            .install_mcp_peer(Arc::new(ScriptedMcpPeer {
                behavior,
                calls: peer_calls.clone(),
                observed_arguments: observed_arguments.clone(),
            }))
            .expect("scripted MCP peer");
        (
            pipeline,
            metadata,
            peer_calls,
            observed_arguments,
            observed_results,
        )
    }

    #[test]
    fn mcp_tool_settles_in_the_agent_loop_approval_free() {
        let root = TempDir::new().expect("temporary directory");
        let (pipeline, metadata, peer_calls, observed_arguments, observed_results) =
            setup_mcp_pipeline(&root, ScriptedMcpBehavior::Echo);
        let result = pipeline
            .execute(mcp_graph_request(&pipeline, metadata))
            .expect("mcp graph pass");
        assert_eq!(
            result.status,
            WorkflowExecutionStatusV1::Succeeded,
            "{:?}",
            result.error
        );
        assert_eq!(result.assistant_text.as_deref(), Some("mcp loop complete"));
        assert_eq!(peer_calls.load(Ordering::SeqCst), 1);
        let arguments = observed_arguments.lock().expect("arguments");
        assert_eq!(arguments.len(), 1);
        assert_eq!(arguments[0]["text"], "hello");
        let results = observed_results.lock().expect("results");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["result"]["echo"], "hello");
        assert_eq!(result.tool_activity.len(), 1);
        assert_eq!(
            result.tool_activity[0].capability_id,
            MCP_FIXTURE_CAPABILITY
        );
        assert_eq!(result.tool_activity[0].status, "completed");
    }

    #[test]
    fn mcp_call_failure_surfaces_as_error_result_and_the_pass_continues() {
        let root = TempDir::new().expect("temporary directory");
        let (pipeline, metadata, peer_calls, _arguments, observed_results) =
            setup_mcp_pipeline(&root, ScriptedMcpBehavior::CallFailure);
        let result = pipeline
            .execute(mcp_graph_request(&pipeline, metadata))
            .expect("mcp failure pass");
        assert_eq!(result.status, WorkflowExecutionStatusV1::Succeeded);
        assert_eq!(peer_calls.load(Ordering::SeqCst), 1);
        let results = observed_results.lock().expect("results");
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0]["error"],
            "MCP serv.fixture/echo failed: the call definitely did not start"
        );
        assert_eq!(result.tool_activity[0].status, "failed");
    }

    #[test]
    fn mcp_oversized_result_is_rejected_without_crashing_the_pass() {
        let root = TempDir::new().expect("temporary directory");
        let (pipeline, metadata, peer_calls, _arguments, observed_results) =
            setup_mcp_pipeline(&root, ScriptedMcpBehavior::OversizedResult);
        let result = pipeline
            .execute(mcp_graph_request(&pipeline, metadata))
            .expect("mcp oversize pass");
        assert_eq!(result.status, WorkflowExecutionStatusV1::Succeeded);
        assert_eq!(peer_calls.load(Ordering::SeqCst), 1);
        let results = observed_results.lock().expect("results");
        assert_eq!(results.len(), 1);
        assert!(
            results[0]["error"]
                .as_str()
                .is_some_and(|error| error.contains("provider continuation bound")),
            "{:?}",
            results[0]
        );
        assert_eq!(result.tool_activity[0].status, "failed");
    }

    #[test]
    fn mcp_dispatch_cancellation_scope_is_session_scoped_and_fails_closed() {
        let generation = ProcessGeneration(21);
        let runtime = crate::runtime::mcp_tools::McpToolRuntimeV1::new(generation);
        assert!(runtime.needs_install().expect("install state"));
        runtime
            .install_scripted_peer(Arc::new(ScriptedMcpPeer {
                behavior: ScriptedMcpBehavior::Echo,
                calls: Arc::new(AtomicUsize::new(0)),
                observed_arguments: Arc::new(Mutex::new(Vec::new())),
            }))
            .expect("scripted peer");
        // A second peer install fails closed: sessions are never hot-replaced.
        assert!(
            runtime
                .install_scripted_peer(Arc::new(ScriptedMcpPeer {
                    behavior: ScriptedMcpBehavior::Echo,
                    calls: Arc::new(AtomicUsize::new(0)),
                    observed_arguments: Arc::new(Mutex::new(Vec::new())),
                }))
                .is_err()
        );
        let run_id = stable("run.mcp-cancel").expect("run");
        let snapshot = runtime
            .open_frozen(&run_id, &mcp_fixture_manifest(generation))
            .expect("open frozen session");
        assert_eq!(snapshot.catalog.tools.len(), 1);

        let token_id = stable("cancel.mcp-scope").expect("token id");
        let token = runtime
            .register_dispatch_token(&token_id)
            .expect("token registration");
        assert!(!token.is_cancelled());
        let server = stable(MCP_FIXTURE_SERVER).expect("server");
        let invocation = stable("invoke.mcp-cancel").expect("invocation");
        // With no in-flight invocation the session manager rejects the cancel
        // (it can never invent evidence), but the scoped token is cancelled
        // first, so a pre-flight dispatch fails closed.
        assert!(
            runtime
                .cancel_dispatch(&token_id, &server, &invocation)
                .is_err()
        );
        assert!(token.is_cancelled());
        runtime.unregister_dispatch_token(&token_id);
        // The scope is gone: a later cancel for the same id cannot target it.
        assert!(
            runtime
                .cancel_dispatch(&token_id, &server, &invocation)
                .is_err()
        );
    }
}
