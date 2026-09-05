use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    io::{self, Write},
    path::Path,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use aworkit_capability_host::{McpCapabilitySnapshotV1, McpPeerPort, ModelToolDefinitionV1};
use aworkit_protocol::StableId;
use aworkit_trusted_core::{
    CredentialMetadataV1, CredentialRef, NativeCredentialStore, PlatformCredentialStorePort,
    ProjectCoordinator,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::management::{
    ManagementRepairCommandInput, ManagementRepairGateway, ManagementRepairProjectionDto,
    ManagementRepairReceipt,
};

use super::{
    cancellation::WorkflowCancellationController,
    credential_journal::{
        CredentialCrashPointV1, CredentialOperationJournal, CredentialOperationKindV1,
        random_credential_ref,
    },
    credentials::CredentialVault,
    documents::{CanonicalDocuments, ProviderDocument, SettingsDocument},
    dto::{
        CredentialDeleteInputV2, CredentialMutationOperationV2, CredentialMutationOutcomeV2,
        CredentialStoreInputV2, DiscoveredModelV2, ExtensionRegisterInputV2, McpProbeRequestV2,
        McpProbeResultV2, ModelDiscoveryRequestV2, ModelDiscoveryResultV2,
        ProviderHealthSnapshotV2, ProviderProbeRequestV2, ProviderProbeResultV2, ProviderTestInput,
        ProviderTestResult, RuntimeSnapshot, SettingsCommitInput, SettingsSnapshot,
        SettingsV2CommitInput, SettingsV2Snapshot, UiCommandInput, UiCommandReceipt,
        WorkflowCommitInput, WorkflowCreateInput, WorkflowCreateReceipt, WorkflowDuplicateInput,
        WorkflowLibrarySnapshot, WorkflowRenameInput, WorkflowSnapshot, WorkflowTargetInput,
    },
    extension_registration::{register_extension_installation_v2, verify_registered_extension_v2},
    external_agent::{
        ExternalAgentProbeRequestV2, ExternalAgentProbeResultV2, probe_external_agent,
    },
    history::{
        ChatHistory, ConversationMessage, FrozenChatExecutionContextV1,
        FrozenChatExecutionRecordV1, FrozenCredentialBindingV1, FrozenToolBindingV1,
        PendingChatCommandV1, canonical_hash, identity_for_seed, message_fact, now_label,
    },
    mcp::probe_mcp_server,
    mcp::{materialize_bindings, prepare_mcp_server},
    mcp_tools::{
        MCP_CAPABILITY_PREFIX, McpRunServerPreparationV1, mcp_provider_name, split_mcp_capability,
    },
    model_tool_loop::PROVIDER_TIMEOUT_RECOVERIES_V1,
    pipeline::{
        WORKFLOW_MAX_MESSAGE_CONTEXT_BYTES, WorkflowExecutionPipeline, WorkflowExecutionRequestV1,
        WorkflowExecutionResultV1, WorkflowExecutionStatusV1, WorkflowMessageV1,
        WorkflowProviderBindingV1,
    },
    project_scope::{resolve_project_scope, selectable_projects},
    provider::{ProviderPort, production_provider, provider_supports_tool_calls},
    provider_health::{ProviderHealth, ProviderHealthRegistry},
    semantic_events::{CommittedChatEventPort, SemanticEventCommitter, noop_committed_event_port},
    settings_diagnostics::{
        ProjectProbeRequestV2, ProjectProbeResultV2, ToolProbeRequestV2, ToolProbeResultV2,
        probe_project, probe_tool_with_api_key,
    },
    settings_v2::{
        AppearanceModeV2, BuiltInToolConfigurationV2, CredentialMetadataConfigurationV2,
        DEFAULT_PROVIDER_REQUEST_TIMEOUT_SECONDS_V1, ExtensionConfigurationV2,
        IntegrationTransportV2, ModelConfigurationV2, ModelTargetV2, ModelTierConfigurationV2,
        ModelTierResolutionV2, ProviderConfigurationV2, SETTINGS_SCHEMA_VERSION_V2,
        SettingsConfigurationV2, validate_extension_lifecycle_update, validate_http_url,
        validate_unavailable_executor_enablement_update,
    },
    tool_loop::{
        SUBAGENT_CAPABILITY_ID, SUBAGENT_CHILD_TOOL_IDS, WorkflowToolBindingV1,
        WorkflowToolCredentialBindingV1,
    },
};

struct ProcessedCommand {
    fingerprint: String,
    receipt: UiCommandReceipt,
}

// A first command is also embedded in the frozen context. Keeping the user
// body at half the full message-context budget leaves deterministic room for
// Agent instructions and durable envelopes before that first context commit.
const WORKFLOW_MAX_USER_INPUT_BYTES: usize = 128 * 1024;
const DEFAULT_MODEL_CALL_TIMEOUT_SECONDS: u64 = 120;

pub(crate) mod approval_control;
use approval_control::parse_approval_resolution;

trait WorkflowPipelinePort: Send + Sync {
    fn validate_approval_target(
        &self,
        _decision_id: &str,
        _chat_id: &str,
        _resolution: &super::approvals::ApprovalResolution,
    ) -> Result<(), String> {
        Ok(())
    }
    fn preflight(&self, _request: &WorkflowExecutionRequestV1) -> Result<(), String> {
        Ok(())
    }

    fn execute(
        &self,
        request: WorkflowExecutionRequestV1,
    ) -> Result<WorkflowExecutionResultV1, String>;

    fn resume_approval(
        &self,
        _decision_id: &str,
        _resolution: &super::approvals::ApprovalResolution,
    ) -> Result<WorkflowExecutionResultV1, String> {
        Err("approval resume is not available from this pipeline".into())
    }

    fn run_todo_state(&self, _run_id: &StableId) -> Result<Option<Value>, String> {
        Ok(None)
    }

    #[allow(dead_code)] // used by pipeline tests through the concrete type
    fn install_mcp_peer(&self, _peer: Arc<dyn McpPeerPort>) -> Result<(), String> {
        Err("MCP peer installation is not available from this pipeline".into())
    }

    fn prepare_mcp_sessions(
        &self,
        _run_id: &StableId,
        _servers: &mut [McpRunServerPreparationV1],
    ) -> Result<Vec<McpCapabilitySnapshotV1>, String> {
        Err("MCP session preparation is not available from this pipeline".into())
    }
}

impl WorkflowPipelinePort for WorkflowExecutionPipeline {
    fn validate_approval_target(
        &self,
        decision_id: &str,
        chat_id: &str,
        resolution: &super::approvals::ApprovalResolution,
    ) -> Result<(), String> {
        WorkflowExecutionPipeline::validate_approval_target(self, decision_id, chat_id)
            .map_err(|error| error.to_string())?;
        WorkflowExecutionPipeline::validate_approval_choice(self, decision_id, resolution)
            .map_err(|error| error.to_string())
    }
    fn preflight(&self, request: &WorkflowExecutionRequestV1) -> Result<(), String> {
        WorkflowExecutionPipeline::preflight(self, request).map_err(|error| error.to_string())
    }

    fn execute(
        &self,
        request: WorkflowExecutionRequestV1,
    ) -> Result<WorkflowExecutionResultV1, String> {
        self.execute(request).map_err(|error| error.to_string())
    }

    fn resume_approval(
        &self,
        decision_id: &str,
        resolution: &super::approvals::ApprovalResolution,
    ) -> Result<WorkflowExecutionResultV1, String> {
        WorkflowExecutionPipeline::resume_approval_choice(self, decision_id, resolution)
            .map_err(|error| error.to_string())
    }

    fn run_todo_state(&self, run_id: &StableId) -> Result<Option<Value>, String> {
        WorkflowExecutionPipeline::run_todo_state(self, run_id).map_err(|error| error.to_string())
    }

    #[allow(dead_code)] // exercised by pipeline tests through the concrete type
    fn install_mcp_peer(&self, peer: Arc<dyn McpPeerPort>) -> Result<(), String> {
        WorkflowExecutionPipeline::install_mcp_peer(self, peer)
    }

    fn prepare_mcp_sessions(
        &self,
        run_id: &StableId,
        servers: &mut [McpRunServerPreparationV1],
    ) -> Result<Vec<McpCapabilitySnapshotV1>, String> {
        WorkflowExecutionPipeline::prepare_mcp_sessions(self, run_id, servers)
    }
}

/// Native composition root for the currently supported desktop workflow.
pub struct DesktopRuntime {
    approvals: super::approvals::ApprovalStore,
    images: super::images::ChatImageStore,
    documents: CanonicalDocuments,
    history: ChatHistory,
    credentials: CredentialVault,
    credential_journal: CredentialOperationJournal,
    provider: Arc<dyn ProviderPort>,
    pipeline: Arc<dyn WorkflowPipelinePort>,
    project_coordinator: ProjectCoordinator,
    provider_health: ProviderHealthRegistry,
    legacy_provider_warning: Option<String>,
    management_repair: ManagementRepairGateway,
    processed: HashMap<String, ProcessedCommand>,
    cancellation_controller: WorkflowCancellationController,
}

impl DesktopRuntime {
    /// Independent immutable image-store handle for nonblocking renderer I/O.
    pub fn image_store(&self) -> super::images::ChatImageStore {
        self.images.clone()
    }
    /// Opens one durable desktop profile using the operating-system credential store.
    pub fn open(data_root: impl AsRef<Path>) -> Result<Self, String> {
        let store: Arc<dyn PlatformCredentialStorePort> = Arc::new(NativeCredentialStore::new());
        Self::open_with_store(data_root.as_ref(), store, production_provider())
    }

    /// Opens the desktop runtime with a subscriber for already committed Chat
    /// events. The subscriber never sees a draft or an uncommitted transition.
    pub fn open_with_committed_events(
        data_root: impl AsRef<Path>,
        committed_events: Arc<dyn CommittedChatEventPort>,
    ) -> Result<Self, String> {
        let store: Arc<dyn PlatformCredentialStorePort> = Arc::new(NativeCredentialStore::new());
        Self::open_with_store_and_committed_events(
            data_root.as_ref(),
            store,
            production_provider(),
            committed_events,
            None,
        )
    }

    /// Explicit composition seam used by hermetic native tests and the rescue QA runner.
    pub fn open_with_credential_store(
        data_root: impl AsRef<Path>,
        store: Arc<dyn PlatformCredentialStorePort>,
    ) -> Result<Self, String> {
        Self::open_with_store(data_root.as_ref(), store, production_provider())
    }

    fn open_with_store(
        data_root: &Path,
        store: Arc<dyn PlatformCredentialStorePort>,
        provider: Arc<dyn ProviderPort>,
    ) -> Result<Self, String> {
        Self::open_with_store_and_committed_events(
            data_root,
            store,
            provider,
            noop_committed_event_port(),
            None,
        )
    }

    /// Composes the optional native renderer without granting a global browser capability.
    pub fn open_with_web_renderer(
        data_root: impl AsRef<Path>,
        committed_events: Arc<dyn CommittedChatEventPort>,
        renderer: Arc<dyn aworkit_capability_host::WebRendererPort>,
    ) -> Result<Self, String> {
        Self::open_with_store_and_committed_events(
            data_root.as_ref(),
            Arc::new(NativeCredentialStore::new()),
            production_provider(),
            committed_events,
            Some(renderer),
        )
    }

    fn open_with_store_and_committed_events(
        data_root: &Path,
        store: Arc<dyn PlatformCredentialStorePort>,
        provider: Arc<dyn ProviderPort>,
        committed_events: Arc<dyn CommittedChatEventPort>,
        renderer: Option<Arc<dyn aworkit_capability_host::WebRendererPort>>,
    ) -> Result<Self, String> {
        let data_root = prepare_root(data_root)?;
        let documents = CanonicalDocuments::open(&data_root)?;
        let credentials =
            CredentialVault::with_store(store.clone(), &documents.settings().credentials)?;
        let credential_journal = CredentialOperationJournal::open(&data_root);
        let history = ChatHistory::open_with_committed_events(&data_root, committed_events)?;
        let event_committer: Arc<dyn SemanticEventCommitter> = Arc::new(history.clone());
        let cancellation_controller = WorkflowCancellationController::default();
        let pipeline = WorkflowExecutionPipeline::open_with_credential_store_and_event_committer(
            &data_root,
            store,
            event_committer,
        )
        .map_err(|error| error.to_string())?
        .with_cancellation_controller(cancellation_controller.clone())
        .with_web_renderer(renderer);
        Self::compose(
            data_root,
            documents,
            history,
            credentials,
            credential_journal,
            provider,
            Arc::new(pipeline),
            cancellation_controller,
        )
    }

    fn compose(
        data_root: std::path::PathBuf,
        documents: CanonicalDocuments,
        history: ChatHistory,
        credentials: CredentialVault,
        credential_journal: CredentialOperationJournal,
        provider: Arc<dyn ProviderPort>,
        pipeline: Arc<dyn WorkflowPipelinePort>,
        cancellation_controller: WorkflowCancellationController,
    ) -> Result<Self, String> {
        let provider_health =
            ProviderHealthRegistry::open(&data_root, &documents.settings().providers)?;
        let project_coordinator = ProjectCoordinator::open(data_root.join("core").join("projects"))
            .map_err(|error| format!("cannot open project coordination state: {error}"))?;
        let mut runtime = Self {
            images: super::images::ChatImageStore::new(&data_root),
            approvals: super::approvals::ApprovalStore::open(
                &data_root
                    .join("history")
                    .join("aworkit-invocations.sqlite3"),
            )?,
            documents,
            history,
            credentials,
            credential_journal,
            provider,
            pipeline,
            project_coordinator,
            provider_health,
            legacy_provider_warning: None,
            management_repair: ManagementRepairGateway::default(),
            processed: HashMap::new(),
            cancellation_controller,
        };
        let mut warnings = runtime
            .credential_journal
            .warning()
            .map(ToOwned::to_owned)
            .into_iter()
            .collect::<Vec<_>>();
        warnings.extend(runtime.reconcile_pending_credential_operations());
        runtime.project_credential_warnings(&warnings);
        Ok(runtime)
    }

    /// Cloneable reserved control path used by the Tauri host without taking
    /// the runtime mutex currently owned by workflow execution.
    #[must_use]
    pub fn cancellation_controller(&self) -> WorkflowCancellationController {
        self.cancellation_controller.clone()
    }

    /// Replays one explicitly resumed durable effect command after a process
    /// crash. Recovery never blocks profile startup: the snapshot projects the
    /// paused state and the user starts this bounded worker through `resume`.
    fn recover_pending_effect(
        &mut self,
        resume_command_id: &str,
        resume_fingerprint: &str,
        expected_version: u64,
    ) -> Result<UiCommandReceipt, String> {
        let history_head = self.history.head()?;
        if expected_version != history_head {
            return Err(format!(
                "desktop version conflict: expected {expected_version}, actual {history_head}"
            ));
        }
        let pending = self
            .history
            .pending_effect_command_at_head(history_head)?
            .ok_or_else(|| "there is no interrupted Chat command to resume".to_owned())?;
        let fingerprint = command_fingerprint(&pending.command)?;
        if fingerprint != pending.command_hash {
            return Err("pending Chat command failed command integrity validation".into());
        }
        let recovered = self.command(pending.command)?;
        let receipt = UiCommandReceipt {
            command_id: resume_command_id.to_owned(),
            accepted: recovered.accepted,
            current_version: recovered.current_version,
            reason: recovered.reason,
            credential_mutation: None,
        };
        self.processed.insert(
            resume_command_id.to_owned(),
            ProcessedCommand {
                fingerprint: resume_fingerprint.to_owned(),
                receipt: receipt.clone(),
            },
        );
        Ok(receipt)
    }

    /// Resolves an unrecoverable pending effect without pretending it never
    /// started. This never invokes the pipeline. It records the exact staged
    /// command as outcome-uncertain, returns control, and preserves evidence so
    /// the user can safely create another Chat without an invisible orphan.
    fn abandon_pending_effect(
        &mut self,
        command_id: &str,
        command_fingerprint: &str,
        expected_version: u64,
    ) -> Result<UiCommandReceipt, String> {
        self.history.ensure_expected(expected_version)?;
        let pending = self
            .history
            .pending_effect_command_at_head(expected_version)?
            .ok_or_else(|| "there is no interrupted Chat command to abandon".to_owned())?;
        let frozen = self
            .history
            .pending_context_at_head(expected_version)?
            .or(self.history.current_frozen_context()?)
            .ok_or_else(|| "pending Chat recovery has no frozen execution context".to_owned())?;
        if frozen.context_hash != pending.frozen_context_hash {
            return Err("pending Chat command does not match its frozen context".into());
        }
        let context = &frozen.context;
        let user_input = string_field(&pending.command.payload, "input")?;
        let created_at = now_label();
        let mut facts = Vec::new();
        let pending_started = self.history.command_started(&pending.command.command_id)?;
        if pending.command.action == "start" && !pending_started {
            facts.push((
                "chat.started",
                json!({
                    "workflowId":context.workflow_id,
                    "workflowVersion":context.workflow_version,
                    "frozenContextHash":frozen.context_hash,
                    "createdAt":created_at,
                    "chatId":context.identity.chat_id,
                    "runId":context.identity.run_id,
                    "projectId":context.project.as_ref().map(|project| project.project_id.as_str()),
                    "workspaceIdentityHash":context.project.as_ref().map(|project| project.workspace_identity_hash.as_str()),
                }),
            ));
        }
        if !pending_started {
            facts.push((
                "message.user",
                message_fact(&user_input, &created_at, None, None, None),
            ));
        } else {
            facts.extend(
                self.history
                    .open_span_terminal_facts(
                        "failed",
                        "Interrupted execution was explicitly abandoned as outcome-uncertain.",
                        &created_at,
                    )?
                    .into_iter()
                    .map(|fact| ("span.failed", fact)),
            );
        }
        facts.push((
            "execution.failed",
            json!({
                "createdAt":created_at,
                "status":"outcome_uncertain",
                "body":"Interrupted execution was explicitly abandoned without replay. A prior external effect may have completed; inspect provider/project state before continuing.",
                "providerId":context.provider_id,
                "modelId":context.model_id,
                "modelTierId":context.model_tier_id,
                "frozenContextHash":frozen.context_hash,
                "settlesCommandId":pending.command.command_id,
                "pendingCommandId":pending.command.command_id,
                "pendingCommandHash":pending.command_hash,
                "automaticReplayAllowed":false,
                "recoveryAbandoned":true,
            }),
        ));
        let _ = self.record_provider_health(
            &context.provider_snapshot,
            ProviderHealth::error(
                "Interrupted execution was abandoned as outcome-uncertain without replay.",
            ),
        );
        self.history
            .append(command_id, command_fingerprint, expected_version, facts)
    }

    /// Installs the separately composed Management repair ledger.
    #[must_use]
    pub fn with_management_repair(mut self, gateway: ManagementRepairGateway) -> Self {
        self.management_repair = gateway;
        self
    }

    pub fn snapshot(&self, after_sequence: u64) -> Result<RuntimeSnapshot, String> {
        let mut snapshot = self.history.snapshot(after_sequence)?;
        snapshot.projects = selectable_projects(&self.documents.settings().projects);
        let fallback_mode = self
            .history
            .current_frozen_context()?
            .and_then(|record| record.context.approval_mode)
            .unwrap_or(self.documents.settings().approvals.default_mode);
        snapshot.chat.approval_mode = self.approvals.mode(&snapshot.chat.chat_id, fallback_mode)?;
        let history_head = self.history.head()?;
        let pending = self
            .history
            .pending_effect_command_at_head(history_head)?
            .is_some();
        if pending {
            let frozen = self
                .history
                .pending_context_at_head(history_head)?
                .or(self.history.current_frozen_context()?);
            if let Some(frozen) = frozen {
                let context = frozen.context;
                snapshot.chat.chat_id = context.identity.chat_id.to_string();
                snapshot.chat.run_id = context.identity.run_id.to_string();
                snapshot.chat.scope = context.project.as_ref().map_or_else(
                    || "No project".into(),
                    |project| project.project_name.clone(),
                );
                snapshot.chat.workflow_id = Some(context.workflow_id);
                snapshot.chat.workflow_name = Some(context.workflow_name);
                snapshot.chat.branch = context
                    .project
                    .as_ref()
                    .and_then(|project| project.branch.clone());
                snapshot.chat.project_id = context
                    .project
                    .as_ref()
                    .map(|project| project.project_id.clone());
            }
            snapshot.chat.phase = "paused".into();
            snapshot.chat.locked_workflow = true;
            snapshot.chat.recovery_pending = true;
            snapshot.chat.disabled_reason = Some(
                "An effect-bearing Chat command was interrupted. Resume it with its original durable command ID; Aworkit will not issue a replacement effect."
                    .into(),
            );
        } else if snapshot.chat.phase == "draft" && !snapshot.chat.locked_workflow {
            snapshot.chat.disabled_reason = self.workflow_start_disabled_reason();
        }
        Ok(snapshot)
    }

    fn workflow_start_disabled_reason(&self) -> Option<String> {
        let readiness = (|| -> Result<(), String> {
            let library = self.documents.workflow_library();
            self.documents
                .require_executable_workflow(&library.default_workflow_id)?;
            let workflow = self.documents.workflow_snapshot();
            let mut resolved: Option<ResolvedWorkflowModel> = None;
            for tier_id in graph_model_tier_ids(&workflow.document) {
                let candidate = resolve_workflow_model(self.documents.settings(), &tier_id)?;
                match &resolved {
                    None => resolved = Some(candidate),
                    Some(previous)
                        if previous.provider.id == candidate.provider.id
                            && previous.model.id == candidate.model.id => {}
                    Some(_) => {
                        return Err(
                            "workflow model tiers resolve to different provider/model bindings"
                                .into(),
                        );
                    }
                }
            }
            let has_project = !selectable_projects(&self.documents.settings().projects).is_empty();
            let agent = freeze_graph_bindings(
                &workflow.document,
                self.documents.settings(),
                has_project,
                &BTreeMap::new(),
            )?;
            let model = resolved
                .as_ref()
                .ok_or_else(|| "workflow has no model-consuming node".to_owned())?;
            validate_model_capabilities(&model.provider, &model.model, !agent.tools.is_empty())?;
            validate_workflow_model_parameters(&workflow.document, &model.provider, &model.model)?;
            if !agent.tools.is_empty() && !has_project {
                return Err(
                    "the saved Agent uses project tools, but Settings has no eligible local project"
                        .into(),
                );
            }
            Ok(())
        })();
        readiness
            .err()
            .map(|reason| format!("Default workflow is unavailable: {reason}"))
    }

    pub fn command(&mut self, input: UiCommandInput) -> Result<UiCommandReceipt, String> {
        if input.schema_version != 1 {
            return Err(format!(
                "unsupported UI command schema {}",
                input.schema_version
            ));
        }
        validate_command_id(&input.command_id)?;
        let fingerprint = command_fingerprint(&input)?;
        if let Some(receipt) = self.history.replay(&input.command_id, &fingerprint)? {
            return Ok(receipt);
        }
        if let Some(processed) = self.processed.get(&input.command_id) {
            return replay_processed(processed, &fingerprint);
        }
        if let Some(pending) = self
            .history
            .pending_effect_command_at_head(self.history.head()?)?
            && !matches!(
                input.action.as_str(),
                "resume" | "abandon_recovery" | "approval"
            )
            && (input.command_id != pending.command.command_id
                || fingerprint != pending.command_hash)
        {
            return Err(
                "an interrupted effect-bearing Chat command must be resumed before another Chat command can run"
                    .into(),
            );
        }
        if matches!(
            input.action.as_str(),
            "new_chat"
                | "start"
                | "enqueue"
                | "resume"
                | "abandon_recovery"
                | "cancel"
                | "approval"
                | "approval_mode"
        ) {
            self.ensure_current_chat_target(input.target_id.as_deref())?;
        }
        match input.action.as_str() {
            "new_chat" => {
                self.history
                    .create_chat(&input.command_id, &fingerprint, input.expected_version)
            }
            "select_chat" => {
                let target_id = required_chat_target(&input)?;
                self.history.select_chat(
                    &input.command_id,
                    &fingerprint,
                    input.expected_version,
                    &target_id,
                )
            }
            "set_chat_pinned" => {
                let target_id = required_chat_target(&input)?;
                let pinned = input
                    .payload
                    .get("pinned")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| "set_chat_pinned requires a boolean pinned field".to_owned())?;
                self.history.set_chat_pinned(
                    &input.command_id,
                    &fingerprint,
                    input.expected_version,
                    &target_id,
                    pinned,
                )
            }
            "delete_chat" => {
                let target_id = required_chat_target(&input)?;
                self.history.delete_chat(
                    &input.command_id,
                    &fingerprint,
                    input.expected_version,
                    &target_id,
                )
            }
            "fork" => self.fork_chat(input, fingerprint),
            "start" | "enqueue" => self.complete_workflow_input(input, fingerprint),
            "approval" => self.complete_approval(input, fingerprint),
            "approval_mode" => self.change_approval_mode(input, fingerprint),
            "resume" => {
                self.recover_pending_effect(&input.command_id, &fingerprint, input.expected_version)
            }
            "abandon_recovery" => {
                self.abandon_pending_effect(&input.command_id, &fingerprint, input.expected_version)
            }
            "cancel" => {
                // A running workflow receives cancellation through the
                // reserved controller before this command can acquire the
                // runtime mutex. Its execution worker records
                // `chat.turn_stopped`; this command then only adds its own
                // idempotent acknowledgement at the current durable head.
                if self.history.was_stopped_by(&input.command_id)? {
                    let head = self.history.head()?;
                    return self.history.append(
                        &input.command_id,
                        &fingerprint,
                        head,
                        vec![(
                            "chat.stop_acknowledged",
                            json!({"createdAt":now_label(),"stopCommandId":input.command_id}),
                        )],
                    );
                }
                self.history.ensure_expected(input.expected_version)?;
                self.history.ensure_cancellable()?;
                let created_at = now_label();
                let mut facts = self
                    .history
                    .open_span_terminal_facts(
                        "cancelled",
                        "Run cancelled by the user.",
                        &created_at,
                    )?
                    .into_iter()
                    .map(|fact| ("span.cancelled", fact))
                    .collect::<Vec<_>>();
                facts.push((
                    "chat.turn_stopped",
                    json!({
                        "createdAt":created_at,
                        "stopCommandId":input.command_id,
                        "body":"Response stopped by the user."
                    }),
                ));
                self.history.append(
                    &input.command_id,
                    &fingerprint,
                    input.expected_version,
                    facts,
                )
            }
            other => Err(format!(
                "desktop action '{other}' is not implemented in the Chat runtime"
            )),
        }
    }

    /// Creates an independently selectable child Chat with an immutable copy
    /// of the parent's conversational context and frozen execution binding.
    /// Tool activity remains evidence on the parent; user/model messages are
    /// copied with provenance so continuing the child sends the same context.
    fn fork_chat(
        &mut self,
        input: UiCommandInput,
        fingerprint: String,
    ) -> Result<UiCommandReceipt, String> {
        self.history.ensure_expected(input.expected_version)?;
        let target_id = required_chat_target(&input)?;
        let parent = self.history.identity(&target_id)?;
        let child = identity_for_seed(&format!("{}:fork", input.command_id))?;
        let parent_events = self.history.events_for_chat(&parent.chat_id)?;
        let parent_context = self.history.frozen_context(&parent.chat_id)?;
        let child_context = if let Some(parent_context) = parent_context.as_ref() {
            let mut child_context = parent_context.context.clone();
            child_context.identity = child.clone();
            child_context.history_base_head = 0;
            child_context.start_command_id =
                StableId::parse(input.command_id.clone()).map_err(|error| error.to_string())?;
            child_context.start_command_hash = fingerprint.clone();
            child_context.pending_start_command = None;
            Some(self.history.freeze_context(child_context)?)
        } else {
            None
        };

        // A retry after a partial child-stream commit must reproduce the exact
        // first batch. Reuse its durable timestamp instead of generating a
        // different payload that the prefix-integrity check would reject.
        let created_at = self
            .history
            .events_for_chat(&child.chat_id)?
            .first()
            .and_then(|event| event.payload.get("createdAt"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(now_label);
        let mut facts = vec![
            (
                "chat.created",
                json!({
                    "createdAt": created_at,
                    "chatId": child.chat_id,
                    "runId": child.run_id,
                    "parentChatId": parent.chat_id,
                }),
            ),
            (
                "chat.continued",
                json!({
                    "createdAt": created_at,
                    "chatId": child.chat_id,
                    "runId": child.run_id,
                    "parentChatId": parent.chat_id,
                }),
            ),
        ];
        if let Some(context) = child_context.as_ref() {
            facts.push((
                "chat.started",
                json!({
                    "createdAt": created_at,
                    "chatId": child.chat_id,
                    "runId": child.run_id,
                    "workflowId": context.context.workflow_id,
                    "workflowVersion": context.context.workflow_version,
                    "frozenContextHash": context.context_hash,
                    "projectId": context.context.project.as_ref().map(|project| project.project_id.as_str()),
                    "workspaceIdentityHash": context.context.project.as_ref().map(|project| project.workspace_identity_hash.as_str()),
                    "parentChatId": parent.chat_id,
                }),
            ));
        }
        for event in parent_events
            .iter()
            .filter(|event| matches!(event.kind.as_str(), "message.user" | "message.assistant"))
        {
            facts.push((
                event.kind.as_str(),
                forked_message_payload(event, &parent.chat_id, &child.run_id),
            ));
        }
        let child_head =
            self.history
                .append_fork_content(&child, &input.command_id, &fingerprint, facts)?;
        self.history.record_fork(
            &input.command_id,
            &fingerprint,
            &parent.chat_id,
            &child,
            child_head,
        )
    }

    fn complete_workflow_input(
        &mut self,
        input: UiCommandInput,
        fingerprint: String,
    ) -> Result<UiCommandReceipt, String> {
        let command_started = self.history.command_started(&input.command_id)?;
        if !command_started {
            self.history.ensure_expected(input.expected_version)?;
        }
        if input.action == "start" {
            let workflow_id = string_field(&input.payload, "workflowId")?;
            let workflow = self.documents.workflow_snapshot_for(&workflow_id);
            if workflow.document.is_null() {
                return Err(format!(
                    "workflow '{workflow_id}' does not exist in the workflow library"
                ));
            }
            if !workflow.editable {
                return Err(format!(
                    "workflow '{workflow_id}' uses a read-only schema and cannot run"
                ));
            }
        } else if input.payload.get("projectId").is_some() {
            return Err("projectId can be supplied only by the first Chat start command".into());
        }
        let images = super::images::command_images(&input.payload)?;
        let user_input = super::images::command_text(&input.payload)?;
        for image in &images {
            aworkit_capability_host::model_images::ModelImageResolver::read(&self.images, image)
                .map_err(|e| e.to_string())?;
        }
        if user_input.len() > WORKFLOW_MAX_USER_INPUT_BYTES || user_input.contains('\0') {
            return Err(format!(
                "Chat input is empty, exceeds the durable {} KiB bound, or contains NUL",
                WORKFLOW_MAX_USER_INPUT_BYTES / 1024
            ));
        }
        let mut conversation = self.history.conversation()?;
        let (frozen, persist_frozen_context) = match input.action.as_str() {
            "start" => {
                let selected_project_id = optional_project_id(&input.payload)?;
                if !conversation.is_empty() && !command_started {
                    return Err("the current Chat is already started; enqueue follow-up input or start a New Chat".into());
                }
                if command_started {
                    (
                        self.history.current_frozen_context()?.ok_or_else(|| {
                            "started Chat command has no frozen execution context".to_owned()
                        })?,
                        false,
                    )
                } else if let Some(pending) = self
                    .history
                    .pending_context_at_head(input.expected_version)?
                {
                    if pending.context.start_command_id.as_str() != input.command_id
                        || pending.context.start_command_hash != fingerprint
                    {
                        return Err(
                            "this draft already has a pending frozen start; retry the original command or start a New Chat"
                                .into(),
                        );
                    }
                    (pending, false)
                } else {
                    (
                        self.prepare_workflow_context(
                            &input,
                            &input.command_id,
                            &fingerprint,
                            input.expected_version,
                            selected_project_id.as_deref(),
                        )?,
                        true,
                    )
                }
            }
            "enqueue" => {
                if conversation.is_empty() {
                    return Err("cannot enqueue before the first Chat message".into());
                }
                self.history.ensure_accepts_follow_up()?;
                (
                    self.history.current_frozen_context()?.ok_or_else(|| {
                        "this legacy Chat has no durable frozen execution context; start a New Chat before sending more input"
                            .to_owned()
                    })?,
                    false,
                )
            }
            _ => return Err("Chat accepts only start or enqueue actions".into()),
        };
        if !command_started {
            conversation.push(ConversationMessage {
                role: "user".into(),
                content: user_input.clone(),
                images: images.clone(),
            });
        }
        let request_id =
            StableId::parse(input.command_id.clone()).map_err(|error| error.to_string())?;
        let context = &frozen.context;
        if conversation
            .iter()
            .any(|message| !message.images.is_empty())
            && !context
                .model_snapshot
                .capabilities
                .iter()
                .any(|capability| capability == "vision")
        {
            return Err(format!(
                "Model '{}' is not configured for vision. Enable Vision in Settings for a compatible model, then start a New Chat.",
                context.model_name
            ));
        }
        let provider_runtime_limits = context.provider_snapshot.runtime_limits()?;
        let provider_messages = conversation
            .iter()
            .map(|message| WorkflowMessageV1 {
                role: message.role.clone(),
                content: message.content.clone(),
                images: message.images.clone(),
            })
            .collect();
        let mut execution_request = WorkflowExecutionRequestV1::bounded(
            request_id,
            context.identity.chat_id.clone(),
            context.identity.run_id.clone(),
            WorkflowProviderBindingV1 {
                kind: context.provider_kind.clone(),
                base_url: context.provider_base_url.clone(),
                model: context.remote_model_id.clone(),
                request_timeout_seconds: provider_runtime_limits.request_timeout_seconds,
                maximum_tool_output_bytes: provider_runtime_limits.maximum_tool_output_bytes,
                credential: context
                    .credential
                    .as_ref()
                    .map(|credential| CredentialMetadataV1 {
                        credential: CredentialRef(credential.credential_ref.clone()),
                        field_names: credential.field_names.clone(),
                        revision: credential.revision,
                    }),
            },
            provider_messages,
            current_epoch_millis()?,
        );
        execution_request.frozen_context_hash = frozen.context_hash.clone();
        execution_request.approvals = super::approvals::ApprovalContext {
            mode: context.approval_mode.unwrap_or_default(),
            chat_id: context.identity.chat_id.to_string(),
            project_key: context.project.as_ref().map(|project| {
                super::approvals::digest(&(project.project_id.as_str(), &project.workspace_binding))
            }),
            project_name: context
                .project
                .as_ref()
                .map(|project| project.project_name.clone()),
        };
        execution_request.model_parameters = context.model_snapshot.parameters.clone();
        execution_request.workspace = context
            .project
            .as_ref()
            .map(|project| project.workspace_binding.clone());
        execution_request.project_branch = context
            .project
            .as_ref()
            .and_then(|project| project.branch.clone());
        execution_request.tools = context
            .tools
            .iter()
            .map(|tool| {
                Ok(WorkflowToolBindingV1 {
                    capability_id: tool.tool_id.clone(),
                    configuration: serde_json::to_value(&tool.tool_snapshot.configuration)
                        .map_err(|error| format!("cannot encode frozen tool Settings: {error}"))?,
                    credential_bindings: tool
                        .tool_snapshot
                        .credential_bindings
                        .iter()
                        .map(|binding| {
                            let metadata = tool
                                .credentials
                                .iter()
                                .find(|metadata| {
                                    metadata.credential_ref.as_str() == binding.credential_ref
                                })
                                .ok_or_else(|| {
                                    format!(
                                        "frozen tool '{}' is missing credential metadata for '{}'",
                                        tool.tool_id, binding.credential_ref
                                    )
                                })?;
                            Ok(WorkflowToolCredentialBindingV1 {
                                name: binding.name.clone(),
                                credential_ref: metadata.credential_ref.clone(),
                                field: binding.field.clone(),
                                field_names: metadata.field_names.clone(),
                                revision: metadata.revision,
                            })
                        })
                        .collect::<Result<Vec<_>, String>>()?,
                    definition: tool.definition.clone(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        execution_request.mcp_servers = context.mcp_manifests.values().cloned().collect();
        execution_request.maximum_timeout_recoveries = PROVIDER_TIMEOUT_RECOVERIES_V1;
        execution_request.workflow_snapshot = context.workflow_snapshot.clone();
        // One outer graph execution is brokered. Agent-internal provider and
        // tool calls are telemetry, not termination budgets. The authority
        // deadline fields carry the pipeline's no-aggregate-deadline sentinel;
        // per-request and per-tool timeouts remain independently bounded.
        execution_request.budget.turns = 1;
        execution_request.budget.attempts = 1;
        execution_request.budget.tool_calls = 0;
        execution_request.budget.actions = 1;
        if serde_json::to_vec(&execution_request.messages)
            .map_err(|error| format!("cannot encode Chat message context: {error}"))?
            .len()
            > WORKFLOW_MAX_MESSAGE_CONTEXT_BYTES
        {
            return Err(format!(
                "the accumulated Chat message context exceeds the durable {} KiB bound; start a New Chat or reduce the input",
                WORKFLOW_MAX_MESSAGE_CONTEXT_BYTES / 1024
            ));
        }
        self.pipeline.preflight(&execution_request)?;
        if persist_frozen_context {
            let persisted = self.history.freeze_context(frozen.context.clone())?;
            if persisted != frozen {
                return Err(
                    "the preflighted Chat context changed before its durable freeze".into(),
                );
            }
        }
        self.history.stage_effect_command(PendingChatCommandV1 {
            schema_version: 1,
            frozen_context_hash: frozen.context_hash.clone(),
            command_hash: fingerprint.clone(),
            command: input.clone(),
        })?;
        if !command_started {
            let created_at = now_label();
            let mut initial_facts = vec![(
                "command.started",
                json!({
                    "schemaVersion": 1,
                    "requestId": input.command_id,
                    "runId": context.identity.run_id,
                    "status": "running",
                    "createdAt": created_at,
                }),
            )];
            if conversation.len() == 1 {
                initial_facts.push((
                    "chat.started",
                    json!({
                        "workflowId":context.workflow_id,
                        "workflowVersion":context.workflow_version,
                        "frozenContextHash":frozen.context_hash,
                        "createdAt":created_at,
                        "chatId":context.identity.chat_id,
                        "runId":context.identity.run_id,
                        "projectId":context.project.as_ref().map(|project| project.project_id.as_str()),
                        "workspaceIdentityHash":context.project.as_ref().map(|project| project.workspace_identity_hash.as_str()),
                    }),
                ));
            }
            let mut user_fact = message_fact(&user_input, &created_at, None, None, None);
            if let Some(object) = user_fact.as_object_mut() {
                if !images.is_empty() {
                    object.insert("attachments".into(), json!(images));
                }
                object.insert("requestId".into(), Value::String(input.command_id.clone()));
                object.insert(
                    "runId".into(),
                    Value::String(context.identity.run_id.to_string()),
                );
            }
            initial_facts.push(("message.user", user_fact));
            initial_facts.push((
                "span.started",
                json!({
                    "schemaVersion": 1,
                    "requestId": input.command_id,
                    "runId": context.identity.run_id,
                    "spanId": format!(
                        "span.run.{}.{}",
                        context.identity.run_id,
                        input.command_id
                    ),
                    "parentSpanId": Value::Null,
                    "spanKind": "run",
                    "semanticRole": "run",
                    "title": "Run",
                    "status": "running",
                    "createdAt": created_at,
                    "hasInput": true,
                    "input": user_input,
                }),
            ));
            self.history.begin_effect_command(
                &input.command_id,
                &fingerprint,
                input.expected_version,
                initial_facts,
            )?;
        }
        let result = self.pipeline.execute(execution_request)?;
        if let Some(receipt) = self.settle_requested_stop(&input, &fingerprint, &result)? {
            return Ok(receipt);
        }
        let created_at = now_label();
        let mut facts = Vec::new();
        match result.status {
            WorkflowExecutionStatusV1::Succeeded => {
                facts.extend(todo_state_fact(
                    self.pipeline.as_ref(),
                    &result,
                    &context.identity.run_id,
                    &created_at,
                )?);
                let assistant = result.assistant_text.as_deref().ok_or_else(|| {
                    "authority pipeline reported success without assistant output".to_owned()
                })?;
                let mut fact = message_fact(
                    assistant,
                    &created_at,
                    Some(&result.model),
                    Some(result.input_units),
                    Some(result.output_units),
                );
                if let Some(object) = fact.as_object_mut() {
                    object.insert("commandId".into(), Value::String(input.command_id.clone()));
                    object.insert(
                        "providerId".into(),
                        Value::String(context.provider_id.clone()),
                    );
                    object.insert("modelId".into(), Value::String(context.model_id.clone()));
                    object.insert(
                        "modelTierId".into(),
                        Value::String(context.model_tier_id.clone()),
                    );
                    object.insert(
                        "frozenContextHash".into(),
                        Value::String(frozen.context_hash.clone()),
                    );
                    object.insert(
                        "snapshotId".into(),
                        Value::String(result.snapshot_id.to_string()),
                    );
                    object.insert(
                        "snapshotHash".into(),
                        Value::String(result.snapshot_hash.clone()),
                    );
                    object.insert(
                        "authorityManifestId".into(),
                        Value::String(result.authority_manifest_id.to_string()),
                    );
                    object.insert(
                        "invocationId".into(),
                        Value::String(result.broker_invocation_id.to_string()),
                    );
                    object.insert("outcomeHash".into(), Value::String(result.outcome_hash));
                    object.insert("modelTurns".into(), Value::from(result.model_turns));
                    object.insert("toolCalls".into(), Value::from(result.tool_calls));
                    object.insert("replayed".into(), Value::Bool(result.replayed));
                }
                let _ = self.record_provider_health(
                    &context.provider_snapshot,
                    ProviderHealth::ready(format!(
                        "Last authority-checked completion succeeded with '{}'.",
                        result.model
                    )),
                );
                facts.push((
                    "span.completed",
                    run_terminal_fact(
                        &result.request_id,
                        &result.run_id,
                        "completed",
                        "Run completed.",
                        Some(Value::String(assistant.to_owned())),
                        &created_at,
                    ),
                ));
                facts.push(("message.assistant", fact));
            }
            WorkflowExecutionStatusV1::AwaitingApproval => {
                facts.extend(todo_state_fact(
                    self.pipeline.as_ref(),
                    &result,
                    &context.identity.run_id,
                    &created_at,
                )?);
                let approval = result.approval.ok_or_else(|| {
                    "authority pipeline reported an approval suspension without a decision identity"
                        .to_owned()
                })?;
                facts.push((
                    "span.updated",
                    run_waiting_fact(&result.request_id, &result.run_id, &created_at),
                ));
                facts.push((
                    "approval.requested",
                    json!({
                        "createdAt": created_at,
                        "commandId": input.command_id,
                        "decisionId": approval.decision_id,
                        "nodeId": approval.node_id,
                        "title": approval.title,
                        "body": approval.message,
                        "projectScope": approval.project_scope,
                        "frozenContextHash": frozen.context_hash,
                        "invocationId": result.broker_invocation_id,
                    }),
                ));
            }
            status => {
                facts.extend(todo_state_fact(
                    self.pipeline.as_ref(),
                    &result,
                    &context.identity.run_id,
                    &created_at,
                )?);
                let error = result.error.unwrap_or_else(|| {
                    "The authority pipeline did not produce a conclusive assistant response.".into()
                });
                let _ = self.record_provider_health(
                    &context.provider_snapshot,
                    ProviderHealth::error(
                        "Last authority-checked completion failed. Inspect Run details for more information.",
                    ),
                );
                facts.push((
                    "span.failed",
                    run_terminal_fact(
                        &result.request_id,
                        &result.run_id,
                        "failed",
                        &error,
                        None,
                        &created_at,
                    ),
                ));
                facts.push((
                    "execution.failed",
                    json!({
                        "createdAt": created_at,
                        "commandId": input.command_id,
                        "status": execution_status_name(status),
                        "body": error,
                        "providerId": context.provider_id,
                        "modelId": context.model_id,
                        "modelTierId": context.model_tier_id,
                        "frozenContextHash": frozen.context_hash,
                        "snapshotId": result.snapshot_id,
                        "snapshotHash": result.snapshot_hash,
                        "authorityManifestId": result.authority_manifest_id,
                        "invocationId": result.broker_invocation_id,
                        "outcomeHash": result.outcome_hash,
                        "modelTurns": result.model_turns,
                        "toolCalls": result.tool_calls,
                        "automaticReplayAllowed": false
                    }),
                ));
            }
        }
        self.history
            .append(&input.command_id, &fingerprint, self.history.head()?, facts)
    }

    /// Converts a controller-requested cancellation into a non-terminal Chat
    /// turn boundary. Provider/tool outcome evidence already committed before
    /// cancellation remains visible; no partial assistant message is invented.
    fn settle_requested_stop(
        &mut self,
        input: &UiCommandInput,
        fingerprint: &str,
        result: &WorkflowExecutionResultV1,
    ) -> Result<Option<UiCommandReceipt>, String> {
        let Some(stop) = self
            .cancellation_controller
            .take_request(result.chat_id.as_str(), result.run_id.as_str())
        else {
            return Ok(None);
        };
        let created_at = now_label();
        let mut facts = self
            .history
            .open_span_terminal_facts("cancelled", "Response stopped by the user.", &created_at)?
            .into_iter()
            .map(|fact| ("span.cancelled", fact))
            .collect::<Vec<_>>();
        facts.push((
            "chat.turn_stopped",
            json!({
                "createdAt": created_at,
                "stopCommandId": stop.command_id,
                "commandId": input.command_id,
                "chatId": stop.chat_id,
                "runId": stop.run_id,
                "body": "Response stopped by the user."
            }),
        ));
        self.history
            .append(
                input.command_id.as_str(),
                fingerprint,
                self.history.head()?,
                facts,
            )
            .map(Some)
    }

    /// Applies one committed approval decision to a durably suspended graph
    /// pass. Approve/reject resume the exact frozen pass from its stored
    /// prefix; the command is idempotent by command ID and recoverable through
    /// the same pending-effect staging as first inputs.
    fn complete_approval(
        &mut self,
        input: UiCommandInput,
        fingerprint: String,
    ) -> Result<UiCommandReceipt, String> {
        let command_started = self.history.command_started(&input.command_id)?;
        if !command_started {
            self.history.ensure_expected(input.expected_version)?;
        }
        let decision_id = string_field(&input.payload, "decisionId")?;
        let resolution = parse_approval_resolution(&input.payload)?;
        let approved = resolution.approved();
        let frozen = self.history.current_frozen_context()?.ok_or_else(|| {
            "the current Chat has no durable frozen execution context for approval".to_owned()
        })?;
        if !command_started {
            self.pipeline.validate_approval_target(
                &decision_id,
                frozen.context.identity.chat_id.as_str(),
                &resolution,
            )?;
        }
        self.history.stage_effect_command(PendingChatCommandV1 {
            schema_version: 1,
            frozen_context_hash: frozen.context_hash.clone(),
            command_hash: fingerprint.clone(),
            command: input.clone(),
        })?;
        if !command_started {
            let created_at = now_label();
            self.history.begin_effect_command(
                &input.command_id,
                &fingerprint,
                input.expected_version,
                vec![
                    (
                        "command.started",
                        json!({
                            "schemaVersion": 1,
                            "requestId": input.command_id,
                            "runId": frozen.context.identity.run_id,
                            "status": "running",
                            "createdAt": created_at,
                        }),
                    ),
                    (
                        "approval.resolved",
                        json!({
                            "createdAt": created_at,
                            "requestId": input.command_id,
                            "runId": frozen.context.identity.run_id,
                            "decisionId": decision_id,
                            "approved": approved,
                            "choice": resolution.choice,
                            "reason": resolution.reason,
                            "frozenContextHash": frozen.context_hash,
                        }),
                    ),
                ],
            )?;
        }
        let result = self
            .pipeline
            .resume_approval(&decision_id, &resolution)
            .map_err(|error| error.to_string())?;
        if let Some(receipt) = self.settle_requested_stop(&input, &fingerprint, &result)? {
            return Ok(receipt);
        }
        let created_at = now_label();
        let context = &frozen.context;
        let mut facts = Vec::new();
        match result.status {
            WorkflowExecutionStatusV1::Succeeded => {
                facts.extend(todo_state_fact(
                    self.pipeline.as_ref(),
                    &result,
                    &context.identity.run_id,
                    &created_at,
                )?);
                let assistant = result.assistant_text.as_deref().ok_or_else(|| {
                    "authority pipeline reported approval success without assistant output"
                        .to_owned()
                })?;
                let mut fact = message_fact(
                    assistant,
                    &created_at,
                    Some(&result.model),
                    Some(result.input_units),
                    Some(result.output_units),
                );
                if let Some(object) = fact.as_object_mut() {
                    object.insert("commandId".into(), Value::String(input.command_id.clone()));
                    object.insert(
                        "providerId".into(),
                        Value::String(context.provider_id.clone()),
                    );
                    object.insert("modelId".into(), Value::String(context.model_id.clone()));
                    object.insert(
                        "modelTierId".into(),
                        Value::String(context.model_tier_id.clone()),
                    );
                    object.insert(
                        "frozenContextHash".into(),
                        Value::String(frozen.context_hash.clone()),
                    );
                    object.insert(
                        "snapshotId".into(),
                        Value::String(result.snapshot_id.to_string()),
                    );
                    object.insert(
                        "snapshotHash".into(),
                        Value::String(result.snapshot_hash.clone()),
                    );
                    object.insert(
                        "authorityManifestId".into(),
                        Value::String(result.authority_manifest_id.to_string()),
                    );
                    object.insert(
                        "invocationId".into(),
                        Value::String(result.broker_invocation_id.to_string()),
                    );
                    object.insert("outcomeHash".into(), Value::String(result.outcome_hash));
                    object.insert("modelTurns".into(), Value::from(result.model_turns));
                    object.insert("toolCalls".into(), Value::from(result.tool_calls));
                }
                let _ = self.record_provider_health(
                    &context.provider_snapshot,
                    ProviderHealth::ready(format!(
                        "Last authority-checked completion succeeded with '{}'.",
                        result.model
                    )),
                );
                facts.push((
                    "span.completed",
                    run_terminal_fact(
                        &result.request_id,
                        &result.run_id,
                        "completed",
                        "Run completed.",
                        Some(Value::String(assistant.to_owned())),
                        &created_at,
                    ),
                ));
                facts.push(("message.assistant", fact));
            }
            WorkflowExecutionStatusV1::AwaitingApproval => {
                facts.extend(todo_state_fact(
                    self.pipeline.as_ref(),
                    &result,
                    &context.identity.run_id,
                    &created_at,
                )?);
                let approval = result.approval.ok_or_else(|| {
                    "authority pipeline reported a second approval without a decision identity"
                        .to_owned()
                })?;
                facts.push((
                    "span.updated",
                    run_waiting_fact(&result.request_id, &result.run_id, &created_at),
                ));
                facts.push((
                    "approval.requested",
                    json!({
                        "createdAt": created_at,
                        "commandId": input.command_id,
                        "decisionId": approval.decision_id,
                        "nodeId": approval.node_id,
                        "title": approval.title,
                        "body": approval.message,
                        "projectScope": approval.project_scope,
                        "frozenContextHash": frozen.context_hash,
                        "invocationId": result.broker_invocation_id,
                    }),
                ));
            }
            status => {
                facts.extend(todo_state_fact(
                    self.pipeline.as_ref(),
                    &result,
                    &context.identity.run_id,
                    &created_at,
                )?);
                let error = result.error.unwrap_or_else(|| {
                    "The authority pipeline did not produce a conclusive approval outcome.".into()
                });
                let _ = self.record_provider_health(
                    &context.provider_snapshot,
                    ProviderHealth::error(
                        "The approved workflow step failed. Inspect Run details for more information.",
                    ),
                );
                facts.push((
                    "span.failed",
                    run_terminal_fact(
                        &result.request_id,
                        &result.run_id,
                        "failed",
                        &error,
                        None,
                        &created_at,
                    ),
                ));
                facts.push((
                    "execution.failed",
                    json!({
                        "createdAt": created_at,
                        "commandId": input.command_id,
                        "status": execution_status_name(status),
                        "body": error,
                        "providerId": context.provider_id,
                        "modelId": context.model_id,
                        "modelTierId": context.model_tier_id,
                        "frozenContextHash": frozen.context_hash,
                        "snapshotId": result.snapshot_id,
                        "snapshotHash": result.snapshot_hash,
                        "authorityManifestId": result.authority_manifest_id,
                        "invocationId": result.broker_invocation_id,
                        "outcomeHash": result.outcome_hash,
                        "modelTurns": result.model_turns,
                        "toolCalls": result.tool_calls,
                        "automaticReplayAllowed": false
                    }),
                ));
            }
        }
        self.history
            .append(&input.command_id, &fingerprint, self.history.head()?, facts)
    }

    fn prepare_workflow_context(
        &mut self,
        command: &UiCommandInput,
        command_id: &str,
        command_hash: &str,
        history_base_head: u64,
        selected_project_id: Option<&str>,
    ) -> Result<FrozenChatExecutionRecordV1, String> {
        let identity = self
            .history
            .current_chat_identity()?
            .unwrap_or(identity_for_seed(command_id)?);
        if let Some(existing) = self.history.frozen_context(&identity.chat_id)? {
            return if existing.context.start_command_id.as_str() == command_id
                && existing.context.start_command_hash == command_hash
                && existing.context.history_base_head == history_base_head
            {
                Ok(existing)
            } else {
                Err("the current Chat already has a different frozen execution context".into())
            };
        }
        let workflow_id = string_field(&command.payload, "workflowId")?;
        let workflow = self.documents.workflow_snapshot_for(&workflow_id);
        if workflow.document.is_null() {
            return Err(format!(
                "workflow '{workflow_id}' does not exist in the workflow library"
            ));
        }
        if !workflow.editable {
            return Err(format!(
                "workflow '{workflow_id}' uses a read-only schema and cannot run"
            ));
        }
        self.documents
            .require_executable_workflow(&workflow_id)
            .map_err(|error| format!("workflow '{workflow_id}' is not executable: {error}"))?;
        let project = resolve_project_scope(
            &self.project_coordinator,
            &self.documents.settings().projects,
            selected_project_id,
        )?;
        // MCP resolution: every mcp:// tool id must name an enabled saved MCP
        // server whose exact manifest is opened, credential-staged, and
        // discovered at freeze. The discovery snapshot supplies the exact
        // model-facing definition; sessions then open on demand per Run.
        let mut mcp_definitions = BTreeMap::new();
        let mut mcp_manifests = BTreeMap::new();
        let mcp_ids = graph_mcp_tool_ids(&workflow.document);
        if !mcp_ids.is_empty() {
            let settings = self.documents.settings().clone();
            let credentials = settings.credentials.clone();
            let mut preparations = Vec::new();
            let mut resolved_servers = BTreeSet::new();
            for capability_id in mcp_ids {
                let (server_id, _tool) =
                    split_mcp_capability(&capability_id).map_err(|error| error.to_string())?;
                let server = settings
                        .mcp_servers
                        .iter()
                        .find(|server| server.id == server_id)
                        .ok_or_else(|| {
                            format!(
                                "workflow binds MCP server '{server_id}' which is not installed in saved Settings"
                            )
                        })?;
                if !server.enabled {
                    return Err(format!(
                        "workflow binds MCP server '{server_id}' which is disabled in saved Settings"
                    ));
                }
                if !resolved_servers.contains(server_id) {
                    let prepared = prepare_mcp_server(server, &credentials)?;
                    let materialization =
                        materialize_bindings(&mut self.credentials, &prepared.secret_bindings)?;
                    preparations.push(McpRunServerPreparationV1 {
                        manifest: prepared.manifest.clone(),
                        endpoint: prepared.endpoint.clone(),
                        materialization,
                    });
                    resolved_servers.insert(server_id.to_owned());
                }
            }
            let snapshots = self
                .pipeline
                .prepare_mcp_sessions(&identity.run_id, &mut preparations)?;
            for preparation in &preparations {
                mcp_manifests.insert(
                    preparation.manifest.server_id.to_string(),
                    preparation.manifest.clone(),
                );
            }
            for capability_id in graph_mcp_tool_ids(&workflow.document) {
                let (server_id, tool) =
                    split_mcp_capability(&capability_id).map_err(|error| error.to_string())?;
                let snapshot = snapshots
                    .iter()
                    .find(|snapshot| snapshot.server_id.as_str() == server_id)
                    .ok_or_else(|| format!("MCP server '{server_id}' has no discovery snapshot"))?;
                let descriptor = snapshot
                    .catalog
                    .tools
                    .iter()
                    .find(|descriptor| descriptor.name == tool)
                    .ok_or_else(|| {
                        format!("MCP server '{server_id}' did not discover tool '{tool}'")
                    })?;
                mcp_definitions.insert(
                    capability_id.clone(),
                    ModelToolDefinitionV1 {
                        capability_id: capability_id.clone(),
                        name: mcp_provider_name(server_id, tool),
                        description: if descriptor.description.is_empty() {
                            format!("Call MCP tool '{tool}' on server '{server_id}'.")
                        } else {
                            descriptor.description.clone()
                        },
                        input_schema: descriptor.input_schema.clone(),
                    },
                );
            }
        }
        // v1 model resolution: every referenced tier must resolve to the same
        // provider/model binding so the single frozen secret lease covers the
        // whole pass.
        let mut resolved: Option<ResolvedWorkflowModel> = None;
        for tier_id in graph_model_tier_ids(&workflow.document) {
            let candidate = resolve_workflow_model(self.documents.settings(), &tier_id)?;
            match &resolved {
                None => resolved = Some(candidate),
                Some(previous)
                    if previous.provider.id == candidate.provider.id
                        && previous.model.id == candidate.model.id =>
                {
                    resolved = Some(candidate);
                }
                Some(previous) => {
                    return Err(format!(
                        "workflow '{workflow_id}' references model tiers resolving to different provider/model bindings ('{}' vs '{}'); v1 graph execution requires one resolved binding",
                        previous.model.name, candidate.model.name
                    ));
                }
            }
        }
        let resolved = resolved.expect("at least one model tier is resolved");
        let workflow_name = workflow
            .document
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(&workflow_id)
            .to_owned();
        let agent = freeze_graph_bindings(
            &workflow.document,
            self.documents.settings(),
            project.is_some(),
            &mcp_definitions,
        )?;
        validate_model_capabilities(&resolved.provider, &resolved.model, !agent.tools.is_empty())?;
        validate_workflow_model_parameters(
            &workflow.document,
            &resolved.provider,
            &resolved.model,
        )?;
        let credential = resolved
            .credential
            .as_ref()
            .map(|metadata| FrozenCredentialBindingV1 {
                credential_ref: metadata.credential.0.clone(),
                field_names: metadata.field_names.clone(),
                revision: metadata.revision,
            });
        let context = FrozenChatExecutionContextV1 {
            approval_mode: Some(self.approvals.mode(
                identity.chat_id.as_str(),
                self.documents.settings().approvals.default_mode,
            )?),
            schema_version: 1,
            identity,
            history_base_head,
            start_command_id: StableId::parse(command_id.to_owned())
                .map_err(|error| error.to_string())?,
            start_command_hash: command_hash.to_owned(),
            pending_start_command: Some(command.clone()),
            settings_version: self.settings_v2_snapshot().version,
            project,
            workflow_id,
            workflow_name,
            workflow_version: workflow.version,
            workflow_snapshot_hash: canonical_hash(&workflow.document)?,
            workflow_snapshot: workflow.document,
            legacy_agent_maximum_turns: None,
            legacy_maximum_tool_calls: None,
            run_deadline_millis: agent.run_deadline_millis,
            tools: agent.tools,
            model_tier_id: resolved.tier.id.clone(),
            model_tier_hash: canonical_hash(&resolved.tier)?,
            model_tier_snapshot: resolved.tier.clone(),
            provider_id: resolved.provider.id.clone(),
            provider_name: resolved.provider.name.clone(),
            provider_kind: resolved.provider.kind.clone(),
            provider_base_url: resolved.provider.base_url.clone(),
            provider_hash: canonical_hash(&resolved.provider)?,
            provider_snapshot: resolved.provider.clone(),
            model_id: resolved.model.id.clone(),
            model_name: resolved.model.name.clone(),
            remote_model_id: resolved.model.remote_id.clone(),
            model_hash: canonical_hash(&resolved.model)?,
            model_snapshot: resolved.model.clone(),
            credential,
            mcp_manifests,
        };
        Ok(FrozenChatExecutionRecordV1 {
            context_hash: canonical_hash(&context)?,
            context,
        })
    }

    #[cfg(test)]
    fn freeze_workflow_context(
        &mut self,
        command: &UiCommandInput,
        command_id: &str,
        command_hash: &str,
        history_base_head: u64,
        selected_project_id: Option<&str>,
    ) -> Result<FrozenChatExecutionRecordV1, String> {
        let prepared = self.prepare_workflow_context(
            command,
            command_id,
            command_hash,
            history_base_head,
            selected_project_id,
        )?;
        let persisted = self.history.freeze_context(prepared.context.clone())?;
        if persisted == prepared {
            Ok(persisted)
        } else {
            Err("the prepared Chat context changed before its durable freeze".into())
        }
    }

    fn active_frozen_credential_ref(&self) -> Result<Option<String>, String> {
        Ok(self
            .history
            .current_frozen_context()?
            .and_then(|record| record.context.credential)
            .map(|credential| credential.credential_ref.to_string()))
    }

    /// Reconciles every interrupted cross-store operation from authoritative
    /// current Settings and the active immutable Chat binding. Cleanup failure
    /// never finalizes the intent, so a later profile open retries it.
    fn reconcile_pending_credential_operations(&mut self) -> Vec<String> {
        let operations = self.credential_journal.pending().to_vec();
        if operations.is_empty() {
            return Vec::new();
        }
        let configured = self
            .documents
            .settings()
            .credentials
            .iter()
            .map(|credential| credential.credential_ref.as_str())
            .collect::<BTreeSet<_>>();
        let active = match self.active_frozen_credential_ref() {
            Ok(active) => active,
            Err(error) => {
                return vec![format!(
                    "Credential cleanup remains pending because the active Chat binding could not be verified: {error}"
                )];
            }
        };
        let mut warnings = Vec::new();
        for operation in operations {
            let mut remains_pending = false;
            for reference in &operation.credential_refs {
                if configured.contains(reference.as_str()) {
                    continue;
                }
                if active.as_deref() == Some(reference.as_str()) {
                    remains_pending = true;
                    continue;
                }
                if let Err(error) = self.credentials.clear(Some(reference)) {
                    remains_pending = true;
                    warnings.push(format!(
                        "Credential cleanup remains pending for opaque reference '{reference}': {error}"
                    ));
                }
            }
            if !remains_pending
                && let Err(error) = self.credential_journal.finalize(&operation.operation_id)
            {
                warnings.push(format!(
                    "Credential cleanup completed, but its journal could not be finalized and will be checked again: {error}"
                ));
            }
        }
        warnings
    }

    fn record_provider_health(
        &mut self,
        exact_provider: &ProviderConfigurationV2,
        health: ProviderHealth,
    ) -> Result<bool, String> {
        let saved_providers = self.documents.settings().providers.clone();
        self.provider_health
            .set_exact(&saved_providers, exact_provider, health)
    }

    fn record_legacy_provider_health(&mut self, health: ProviderHealth) -> Result<bool, String> {
        let exact_provider =
            legacy_provider_id(self.documents.settings()).and_then(|provider_id| {
                self.documents
                    .settings()
                    .providers
                    .iter()
                    .find(|provider| provider.id == provider_id)
                    .cloned()
            });
        exact_provider.map_or(Ok(false), |provider| {
            self.record_provider_health(&provider, health)
        })
    }

    fn reconcile_provider_health(&mut self) -> Option<String> {
        self.provider_health
            .reconcile(&self.documents.settings().providers)
            .err()
            .map(|error| {
                format!("Settings were saved, but provider health could not be persisted: {error}")
            })
    }

    fn project_credential_warnings(&mut self, warnings: &[String]) {
        let unique = warnings
            .iter()
            .filter(|warning| {
                self.legacy_provider_warning
                    .as_deref()
                    .is_none_or(|detail| !detail.contains(warning.as_str()))
            })
            .cloned()
            .collect::<BTreeSet<_>>();
        if unique.is_empty() {
            return;
        }
        let warning = unique.into_iter().collect::<Vec<_>>().join(" ");
        self.legacy_provider_warning = Some(match self.legacy_provider_warning.take() {
            Some(detail) if !detail.is_empty() => format!("{detail} {warning}"),
            _ => warning,
        });
    }

    fn reconcile_credential_operation(&mut self) -> Option<String> {
        let warnings = self.reconcile_pending_credential_operations();
        self.project_credential_warnings(&warnings);
        (!warnings.is_empty()).then(|| warnings.join(" "))
    }

    fn credential_operation_error(&mut self, error: String) -> String {
        self.reconcile_credential_operation()
            .map_or(error.clone(), |warning| format!("{error}; {warning}"))
    }

    #[cfg(test)]
    fn arm_credential_crash_point(&mut self, point: CredentialCrashPointV1) {
        self.credential_journal.arm_crash_point(point);
    }

    fn ensure_current_chat_target(&self, target_id: Option<&str>) -> Result<(), String> {
        let (Some(target_id), Some(identity)) = (target_id, self.history.current_chat_identity()?)
        else {
            return Ok(());
        };
        if target_id == identity.chat_id.as_str() {
            Ok(())
        } else {
            Err(format!(
                "Chat target '{target_id}' is stale; the selected Chat is '{}'",
                identity.chat_id
            ))
        }
    }

    #[must_use]
    pub fn settings_snapshot(&self) -> SettingsSnapshot {
        let mut health = legacy_provider_id(self.documents.settings())
            .and_then(|provider_id| {
                self.documents
                    .settings()
                    .providers
                    .iter()
                    .find(|provider| provider.id == provider_id)
            })
            .map_or_else(ProviderHealth::legacy_unconfigured, |provider| {
                self.provider_health.health(provider)
            });
        if let Some(warning) = self.legacy_provider_warning.as_deref() {
            health = ProviderHealth::error(match health.detail {
                Some(detail) if !detail.is_empty() => format!("{detail} {warning}"),
                _ => warning.to_owned(),
            });
        }
        self.documents.settings_snapshot(&health)
    }

    /// Returns the complete secret-free Settings v2 projection.
    #[must_use]
    pub fn settings_v2_snapshot(&self) -> SettingsV2Snapshot {
        let provider_health = self
            .documents
            .settings()
            .providers
            .iter()
            .map(|provider| {
                let health = self.provider_health.health(provider);
                ProviderHealthSnapshotV2 {
                    provider_id: provider.id.clone(),
                    state: health.state,
                    detail: health.detail,
                }
            })
            .collect();
        SettingsV2Snapshot {
            version: self.settings_snapshot().version,
            schema_version: SETTINGS_SCHEMA_VERSION_V2,
            settings: self.documents.settings().clone(),
            provider_health,
        }
    }

    pub fn settings_test_provider(&mut self, input: ProviderTestInput) -> ProviderTestResult {
        let base_url = input.base_url.trim().to_owned();
        let model = input.model.trim().to_owned();
        let use_stored_credential = input.use_stored_credential;
        let direct_key = input
            .api_key
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.to_string());
        if direct_key.is_some() && use_stored_credential {
            return ProviderTestResult {
                ok: false,
                message: "Choose either the entered API key or the stored credential, not both."
                    .into(),
                model: None,
            };
        }
        let saved_provider = self.documents.legacy_provider();
        if use_stored_credential && saved_provider.credential_ref.is_some() {
            if base_url != saved_provider.base_url {
                return ProviderTestResult {
                    ok: false,
                    message: "The stored API key is bound to the saved provider endpoint. Replace or clear it before testing a different endpoint.".into(),
                    model: None,
                };
            }
            if let Err(error) = require_stored_credential_binding(&saved_provider) {
                let _ = self.record_legacy_provider_health(ProviderHealth::error(error.clone()));
                return ProviderTestResult {
                    ok: false,
                    message: error,
                    model: None,
                };
            }
        }
        let tests_saved_credentials = use_stored_credential
            || (saved_provider.credential_ref.is_none() && direct_key.is_none());
        let api_key = if use_stored_credential {
            match self
                .credentials
                .resolve(saved_provider.credential_ref.as_deref())
            {
                Ok(value) => value,
                Err(error) => {
                    let _ =
                        self.record_legacy_provider_health(ProviderHealth::error(error.clone()));
                    return ProviderTestResult {
                        ok: false,
                        message: error,
                        model: None,
                    };
                }
            }
        } else {
            direct_key
        };
        let mut result = self.provider.test_connection(
            "openai_compatible",
            &base_url,
            &model,
            api_key,
            Duration::from_secs(DEFAULT_PROVIDER_REQUEST_TIMEOUT_SECONDS_V1),
        );
        let tests_saved_binding = base_url == saved_provider.base_url
            && model == saved_provider.model
            && tests_saved_credentials;
        if tests_saved_binding {
            let health = if result.ok {
                ProviderHealth::ready(format!(
                    "Last native connection test succeeded for model '{model}'."
                ))
            } else {
                ProviderHealth::error(
                    "Last native connection test failed. Run Test connection again for current details.",
                )
            };
            if let Err(error) = self.record_legacy_provider_health(health) {
                result.message = format!(
                    "{} Provider health could not be persisted: {error}",
                    result.message
                );
            }
        }
        result
    }

    pub fn settings_commit(
        &mut self,
        mut input: SettingsCommitInput,
    ) -> Result<UiCommandReceipt, String> {
        validate_command_id(&input.command_id)?;
        let fingerprint = command_fingerprint(&input)?;
        if let Some(processed) = self.processed.get(&input.command_id) {
            return replay_processed(processed, &fingerprint);
        }
        let actual_version = self.settings_snapshot().version;
        if input.expected_version != actual_version {
            return Err(format!(
                "settings version conflict: expected {}, actual {actual_version}",
                input.expected_version
            ));
        }
        if !matches!(input.appearance.as_str(), "system" | "light" | "dark") {
            return Err("settings appearance must be system, light, or dark".into());
        }
        input.provider.base_url = input.provider.base_url.trim().to_owned();
        input.provider.model = input.provider.model.trim().to_owned();
        let previous = self.documents.settings().clone();
        let previous_provider = self.documents.legacy_provider();
        let frozen_credential_ref = self.active_frozen_credential_ref()?;
        let configured = !input.provider.base_url.is_empty() || !input.provider.model.is_empty();
        if configured {
            require_provider_fields(&input.provider.base_url, &input.provider.model)?;
            self.provider.validate(
                "openai_compatible",
                &input.provider.base_url,
                &input.provider.model,
                Duration::from_secs(DEFAULT_PROVIDER_REQUEST_TIMEOUT_SECONDS_V1),
            )?;
        } else if input.provider.api_key.is_some() {
            return Err("configure a provider endpoint and model before storing an API key".into());
        }
        let old_credential_ref = previous_provider.credential_ref.clone();
        let mut created_credential_metadata = None;
        let target_provider_id = legacy_provider_id(&previous)
            .unwrap_or("provider.primary")
            .to_owned();
        let provider = match input.provider.credential_action.as_str() {
            "keep" => {
                if input.provider.api_key.is_some() {
                    return Err("Keep credential must not include an API key".into());
                }
                if old_credential_ref.is_some()
                    && input.provider.base_url != previous_provider.base_url
                {
                    return Err(
                        "The stored API key is bound to the saved provider endpoint. Replace or clear it before changing the endpoint."
                            .into(),
                    );
                }
                ProviderDocument {
                    base_url: input.provider.base_url.clone(),
                    model: input.provider.model.clone(),
                    credential_ref: old_credential_ref.clone(),
                    credential_endpoint: previous_provider.credential_endpoint.clone(),
                    credential_fields: previous_provider.credential_fields.clone(),
                    credential_revision: previous_provider.credential_revision,
                }
            }
            "replace" => {
                let api_key = input
                    .provider
                    .api_key
                    .take()
                    .ok_or_else(|| "Replace credential requires an API key".to_owned())?;
                let reference = random_credential_ref()?;
                let mut affected = vec![reference.clone()];
                if let Some(old_reference) = old_credential_ref.as_ref() {
                    affected.push(old_reference.clone());
                }
                self.credential_journal.begin(
                    if old_credential_ref.is_some() {
                        CredentialOperationKindV1::Replace
                    } else {
                        CredentialOperationKindV1::Create
                    },
                    affected,
                )?;
                self.credential_journal
                    .fail_if(CredentialCrashPointV1::BeforePut)?;
                let metadata = match self
                    .credentials
                    .put_api_key_at(&reference, api_key.as_str())
                {
                    Ok(metadata) => metadata,
                    Err(error) => return Err(self.credential_operation_error(error)),
                };
                self.credential_journal
                    .fail_if(CredentialCrashPointV1::AfterPutBeforeSettings)?;
                created_credential_metadata = Some(CredentialMetadataConfigurationV2 {
                    credential_ref: reference.clone(),
                    label: format!("{} API key", input.provider.model),
                    kind: "api_key".into(),
                    field_names: metadata.field_names.iter().cloned().collect(),
                    revision: metadata.revision,
                    bound_provider_id: Some(target_provider_id.clone()),
                    bound_endpoint: Some(input.provider.base_url.clone()),
                });
                ProviderDocument {
                    base_url: input.provider.base_url.clone(),
                    model: input.provider.model.clone(),
                    credential_ref: Some(reference),
                    credential_endpoint: Some(input.provider.base_url.clone()),
                    credential_fields: metadata.field_names.into_iter().collect(),
                    credential_revision: Some(metadata.revision),
                }
            }
            "clear" => {
                if input.provider.api_key.is_some() {
                    return Err("Clear credential must not include an API key".into());
                }
                if let Some(reference) = old_credential_ref.as_ref() {
                    self.credential_journal
                        .begin(CredentialOperationKindV1::Delete, vec![reference.clone()])?;
                }
                ProviderDocument {
                    base_url: input.provider.base_url.clone(),
                    model: input.provider.model.clone(),
                    credential_ref: None,
                    credential_endpoint: None,
                    credential_fields: Vec::new(),
                    credential_revision: None,
                }
            }
            other => {
                return Err(format!(
                    "unsupported credential action '{other}'; use keep, replace, or clear"
                ));
            }
        };
        let mut next_settings = previous;
        next_settings.appearance.mode = parse_appearance_mode(&input.appearance)?;
        next_settings.data.portable_history_enabled = input.portable_history_enabled;
        let preserves_frozen_credential = old_credential_ref.is_some()
            && old_credential_ref.as_deref() != provider.credential_ref.as_deref()
            && old_credential_ref.as_deref() == frozen_credential_ref.as_deref();
        if old_credential_ref.as_deref() != provider.credential_ref.as_deref()
            && !preserves_frozen_credential
        {
            next_settings.credentials.retain(|credential| {
                Some(credential.credential_ref.as_str()) != old_credential_ref.as_deref()
            });
        }
        if preserves_frozen_credential {
            for credential in &mut next_settings.credentials {
                if Some(credential.credential_ref.as_str()) == old_credential_ref.as_deref() {
                    // The active Chat carries the immutable provider/endpoint
                    // binding. Canonical Settings retain only the opaque record
                    // until that Chat is released, so future configuration may
                    // remove or repurpose the old provider safely.
                    credential.bound_provider_id = None;
                    credential.bound_endpoint = None;
                }
            }
        }
        if let Some(metadata) = created_credential_metadata {
            next_settings.credentials.push(metadata);
        }
        apply_legacy_provider(&mut next_settings, &target_provider_id, &provider)?;
        if let Err(error) = next_settings.validate() {
            return Err(self.credential_operation_error(error));
        }
        if let Err(error) = next_settings.validate_installed_runtime_consumers() {
            return Err(self.credential_operation_error(error));
        }
        let next_version = match self
            .documents
            .save_settings(input.expected_version, next_settings)
        {
            Ok(version) => version,
            Err(commit_error) => {
                return Err(format!(
                    "{commit_error}; credential reconciliation remains durably pending until the profile is reopened"
                ));
            }
        };
        if input.provider.credential_action == "replace" {
            self.credential_journal
                .fail_if(CredentialCrashPointV1::AfterReplacementSettingsBeforeObsoleteDelete)?;
        } else if input.provider.credential_action == "clear" && old_credential_ref.is_some() {
            self.credential_journal
                .fail_if(CredentialCrashPointV1::AfterDeleteMetadataBeforeStoreDelete)?;
        }
        let provider_health_warning = self.reconcile_provider_health();
        let retention_warning = if preserves_frozen_credential {
            Some(
                "The previous credential remains stored because the active Chat is frozen to it; start a New Chat before deleting that now-unreferenced credential."
                    .to_owned(),
            )
        } else {
            None
        };
        let cleanup_warning = self.reconcile_credential_operation();
        let receipt_warning = [retention_warning, cleanup_warning, provider_health_warning]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        let receipt_warning = (!receipt_warning.is_empty()).then_some(receipt_warning);
        if let Some(warning) = receipt_warning.as_ref() {
            self.project_credential_warnings(std::slice::from_ref(warning));
        }
        let receipt = UiCommandReceipt {
            command_id: input.command_id.clone(),
            accepted: true,
            current_version: next_version,
            reason: receipt_warning,
            credential_mutation: None,
        };
        self.processed.insert(
            input.command_id,
            ProcessedCommand {
                fingerprint,
                receipt: receipt.clone(),
            },
        );
        Ok(receipt)
    }

    /// Validates and atomically saves the complete Settings v2 document.
    pub fn settings_v2_commit(
        &mut self,
        input: SettingsV2CommitInput,
    ) -> Result<UiCommandReceipt, String> {
        validate_command_id(&input.command_id)?;
        let fingerprint = command_fingerprint(&input)?;
        if let Some(processed) = self.processed.get(&input.command_id) {
            return replay_processed(processed, &fingerprint);
        }
        let actual_version = self.settings_v2_snapshot().version;
        if input.expected_version != actual_version {
            return Err(format!(
                "settings version conflict: expected {}, actual {actual_version}",
                input.expected_version
            ));
        }
        input.settings.validate()?;
        input.settings.validate_installed_runtime_consumers()?;
        let previous = self.documents.settings().clone();
        validate_credential_metadata_update(&previous, &input.settings)?;
        validate_extension_lifecycle_update(&previous, &input.settings)?;
        validate_unavailable_executor_enablement_update(&previous, &input.settings)?;
        for extension in input
            .settings
            .extensions
            .iter()
            .filter(|extension| extension.enabled)
        {
            verify_registered_extension_v2(extension).map_err(|error| {
                format!(
                    "extension '{}' has enabled legacy metadata whose verified identity is unavailable: {error}",
                    extension.id
                )
            })?;
        }
        let next_version = self
            .documents
            .save_settings(input.expected_version, input.settings)?;
        let provider_health_warning = self.reconcile_provider_health();
        let receipt = UiCommandReceipt {
            command_id: input.command_id.clone(),
            accepted: true,
            current_version: next_version,
            reason: provider_health_warning,
            credential_mutation: None,
        };
        self.processed.insert(
            input.command_id,
            ProcessedCommand {
                fingerprint,
                receipt: receipt.clone(),
            },
        );
        Ok(receipt)
    }

    /// Creates or replaces one write-only OS credential and atomically updates
    /// every opaque configuration reference. The old secret is deleted only
    /// after the new canonical document commits.
    pub fn settings_v2_store_credential(
        &mut self,
        mut input: CredentialStoreInputV2,
    ) -> Result<UiCommandReceipt, String> {
        validate_command_id(&input.command_id)?;
        let fingerprint = command_fingerprint(&input)?;
        if let Some(processed) = self.processed.get(&input.command_id) {
            return replay_processed(processed, &fingerprint);
        }
        let actual_version = self.settings_v2_snapshot().version;
        if input.expected_version != actual_version {
            return Err(format!(
                "settings version conflict: expected {}, actual {actual_version}",
                input.expected_version
            ));
        }
        input.label = input.label.trim().to_owned();
        input.kind = input.kind.trim().to_owned();
        let previous = self.documents.settings().clone();
        let frozen_credential_ref = self.active_frozen_credential_ref()?;
        if let Some(reference) = input.replace_credential_ref.as_deref()
            && previous.credential(reference).is_none()
        {
            return Err(format!("credential '{reference}' is not configured"));
        }

        let new_reference = random_credential_ref()?;
        let mut affected = vec![new_reference.clone()];
        if let Some(old_reference) = input.replace_credential_ref.as_ref() {
            affected.push(old_reference.clone());
        }
        self.credential_journal.begin(
            if input.replace_credential_ref.is_some() {
                CredentialOperationKindV1::Replace
            } else {
                CredentialOperationKindV1::Create
            },
            affected,
        )?;
        self.credential_journal
            .fail_if(CredentialCrashPointV1::BeforePut)?;
        let metadata = match self
            .credentials
            .put_fields_at(&new_reference, std::mem::take(&mut input.fields))
        {
            Ok(metadata) => metadata,
            Err(error) => return Err(self.credential_operation_error(error)),
        };
        self.credential_journal
            .fail_if(CredentialCrashPointV1::AfterPutBeforeSettings)?;
        let new_metadata = CredentialMetadataConfigurationV2 {
            credential_ref: new_reference.clone(),
            label: input.label,
            kind: input.kind,
            field_names: metadata.field_names.into_iter().collect(),
            revision: metadata.revision,
            bound_provider_id: input.bound_provider_id,
            bound_endpoint: input.bound_endpoint,
        };
        let mut next = previous;
        if let Some(old_reference) = input.replace_credential_ref.as_deref() {
            replace_credential_references(&mut next, old_reference, &new_reference);
            if frozen_credential_ref.as_deref() != Some(old_reference) {
                next.credentials
                    .retain(|credential| credential.credential_ref != old_reference);
            }
        }
        next.credentials.push(new_metadata);
        if let Err(error) = next.validate() {
            return Err(self.credential_operation_error(error));
        }
        if let Err(error) = next.validate_installed_runtime_consumers() {
            return Err(self.credential_operation_error(error));
        }
        let next_version = match self.documents.save_settings(input.expected_version, next) {
            Ok(version) => version,
            Err(error) => {
                return Err(format!(
                    "{error}; credential reconciliation remains durably pending until the profile is reopened"
                ));
            }
        };
        if input.replace_credential_ref.is_some() {
            self.credential_journal
                .fail_if(CredentialCrashPointV1::AfterReplacementSettingsBeforeObsoleteDelete)?;
        }
        let provider_health_warning = self.reconcile_provider_health();
        let retention_warning = match input.replace_credential_ref.as_deref() {
            Some(reference) if frozen_credential_ref.as_deref() == Some(reference) => Some(
                "The previous credential remains stored because the active Chat is frozen to it; start a New Chat before deleting that now-unreferenced credential."
                    .to_owned(),
            ),
            None => None,
            Some(_) => None,
        };
        let cleanup_warning = self.reconcile_credential_operation();
        let receipt_warning = [retention_warning, cleanup_warning, provider_health_warning]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        let receipt_warning = (!receipt_warning.is_empty()).then_some(receipt_warning);
        if let Some(warning) = receipt_warning.as_ref() {
            self.project_credential_warnings(std::slice::from_ref(warning));
        }
        let receipt = UiCommandReceipt {
            command_id: input.command_id.clone(),
            accepted: true,
            current_version: next_version,
            reason: receipt_warning,
            credential_mutation: Some(CredentialMutationOutcomeV2 {
                operation: if input.replace_credential_ref.is_some() {
                    CredentialMutationOperationV2::Replace
                } else {
                    CredentialMutationOperationV2::Create
                },
                previous_credential_ref: input.replace_credential_ref.clone(),
                fresh_credential_ref: new_reference,
            }),
        };
        self.processed.insert(
            input.command_id,
            ProcessedCommand {
                fingerprint,
                receipt: receipt.clone(),
            },
        );
        Ok(receipt)
    }

    /// Removes an unreferenced credential from canonical metadata first, then
    /// deletes the operating-system record. Bound credentials fail closed.
    pub fn settings_v2_delete_credential(
        &mut self,
        input: CredentialDeleteInputV2,
    ) -> Result<UiCommandReceipt, String> {
        validate_command_id(&input.command_id)?;
        let fingerprint = command_fingerprint(&input)?;
        if let Some(processed) = self.processed.get(&input.command_id) {
            return replay_processed(processed, &fingerprint);
        }
        let actual_version = self.settings_v2_snapshot().version;
        if input.expected_version != actual_version {
            return Err(format!(
                "settings version conflict: expected {}, actual {actual_version}",
                input.expected_version
            ));
        }
        if self.active_frozen_credential_ref()?.as_deref() == Some(&input.credential_ref) {
            return Err(
                "credential is retained by the active Chat's frozen execution context; start a New Chat before deleting it"
                    .into(),
            );
        }
        let mut next = self.documents.settings().clone();
        if next.credential(&input.credential_ref).is_none() {
            return Err(format!(
                "credential '{}' is not configured",
                input.credential_ref
            ));
        }
        if credential_is_referenced(&next, &input.credential_ref) {
            return Err(
                "credential is still referenced by a provider, tool, MCP server, or external agent; remove every binding and Save Configuration before deleting it"
                    .into(),
            );
        }
        next.credentials
            .retain(|credential| credential.credential_ref != input.credential_ref);
        next.validate()?;
        next.validate_installed_runtime_consumers()?;
        self.credential_journal.begin(
            CredentialOperationKindV1::Delete,
            vec![input.credential_ref.clone()],
        )?;
        let next_version = self
            .documents
            .save_settings(input.expected_version, next)
            .map_err(|error| {
                format!(
                    "{error}; credential reconciliation remains durably pending until the profile is reopened"
                )
            })?;
        self.credential_journal
            .fail_if(CredentialCrashPointV1::AfterDeleteMetadataBeforeStoreDelete)?;
        let provider_health_warning = self.reconcile_provider_health();
        let cleanup_warning = self.reconcile_credential_operation();
        let receipt_warning = [cleanup_warning, provider_health_warning]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        let receipt_warning = (!receipt_warning.is_empty()).then_some(receipt_warning);
        let receipt = UiCommandReceipt {
            command_id: input.command_id.clone(),
            accepted: true,
            current_version: next_version,
            reason: receipt_warning,
            credential_mutation: None,
        };
        self.processed.insert(
            input.command_id,
            ProcessedCommand {
                fingerprint,
                receipt: receipt.clone(),
            },
        );
        Ok(receipt)
    }

    /// Tests one exact concrete model through the configured native adapter.
    /// The result echoes the draft fingerprint so stale async results cannot be
    /// mistaken for the current UI draft.
    pub fn settings_v2_test_provider(
        &mut self,
        mut request: ProviderProbeRequestV2,
    ) -> Result<ProviderProbeResultV2, String> {
        validate_provider_operation_draft(&request.provider)?;
        if request.draft_fingerprint.trim().is_empty() {
            return Err("provider probe requires a non-empty draft fingerprint".into());
        }
        let model = request
            .provider
            .models
            .iter()
            .find(|model| model.id == request.model_id)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "provider '{}' has no model '{}'",
                    request.provider.id, request.model_id
                )
            })?;
        let runtime_limits = request.provider.runtime_limits()?;
        self.provider.validate(
            &request.provider.kind,
            &request.provider.base_url,
            &model.remote_id,
            Duration::from_secs(runtime_limits.request_timeout_seconds),
        )?;
        let tests_saved_credential_binding = request.replacement_credential.is_none()
            && if request.provider.credential_ref.is_some() {
                request.use_stored_credential
            } else {
                !request.use_stored_credential
            };
        let started = Instant::now();
        let credential = match self.provider_operation_credential(
            &request.provider,
            request.replacement_credential.take(),
            request.use_stored_credential,
        ) {
            Ok(credential) => credential,
            Err(error) => {
                if tests_saved_credential_binding
                    && saved_provider_matches(self.documents.settings(), &request.provider)
                {
                    self.record_provider_health(
                        &request.provider,
                        ProviderHealth::error(
                            "Last native connection test could not redeem the saved credential. Run Test connection again after repairing the operating-system credential.",
                        ),
                    )?;
                }
                return Ok(ProviderProbeResultV2 {
                    ok: false,
                    message: error,
                    provider_id: request.provider.id,
                    model_id: None,
                    remote_model_id: None,
                    latency_millis: u64::try_from(started.elapsed().as_millis())
                        .unwrap_or(u64::MAX),
                    draft_fingerprint: request.draft_fingerprint,
                });
            }
        };
        let result = self.provider.test_connection(
            &request.provider.kind,
            &request.provider.base_url,
            &model.remote_id,
            credential,
            Duration::from_secs(runtime_limits.request_timeout_seconds),
        );
        let latency_millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        if tests_saved_credential_binding
            && saved_provider_matches(self.documents.settings(), &request.provider)
        {
            let health = if result.ok {
                ProviderHealth::ready(format!(
                    "Last native connection test succeeded for model '{}'.",
                    model.remote_id
                ))
            } else {
                ProviderHealth::error(
                    "Last native connection test failed. Run Test connection again for current details.",
                )
            };
            self.record_provider_health(&request.provider, health)?;
        }
        Ok(ProviderProbeResultV2 {
            ok: result.ok,
            message: result.message,
            provider_id: request.provider.id,
            model_id: result.ok.then_some(request.model_id),
            remote_model_id: result.ok.then_some(model.remote_id.clone()),
            latency_millis,
            draft_fingerprint: request.draft_fingerprint,
        })
    }

    /// Fetches the bounded remote catalog through the provider adapter. It
    /// never saves, enables, or selects discovered models automatically.
    pub fn settings_v2_discover_models(
        &mut self,
        mut request: ModelDiscoveryRequestV2,
    ) -> Result<ModelDiscoveryResultV2, String> {
        validate_provider_operation_draft(&request.provider)?;
        if request.draft_fingerprint.trim().is_empty() {
            return Err("model discovery requires a non-empty draft fingerprint".into());
        }
        let credential = self.provider_operation_credential(
            &request.provider,
            request.replacement_credential.take(),
            request.use_stored_credential,
        )?;
        let runtime_limits = request.provider.runtime_limits()?;
        let models = self
            .provider
            .discover_models(
                &request.provider.kind,
                &request.provider.base_url,
                credential,
                Duration::from_secs(runtime_limits.request_timeout_seconds),
            )?
            .into_iter()
            .map(|model| DiscoveredModelV2 {
                remote_id: model.remote_id,
                name: model.name,
                context_window: model.context_window,
                max_output_tokens: model.max_output_tokens,
                capabilities: model.capabilities,
            })
            .collect::<Vec<_>>();
        Ok(ModelDiscoveryResultV2 {
            provider_id: request.provider.id,
            draft_fingerprint: request.draft_fingerprint,
            message: format!("Discovered {} model(s).", models.len()),
            models,
        })
    }

    /// Opens, discovers, and closes one exact unsaved MCP server draft through
    /// the production transport. This operation does not save the draft or
    /// change its configured `enabled`/`autoConnect` state.
    pub fn settings_v2_probe_mcp(
        &mut self,
        request: McpProbeRequestV2,
    ) -> Result<McpProbeResultV2, String> {
        probe_mcp_server(
            &mut self.credentials,
            &self.documents.settings().credentials,
            request,
        )
    }

    /// Initializes and interrogates one exact unsaved external-agent draft,
    /// then closes its complete process group without starting an agent task.
    pub fn settings_v2_probe_external_agent(
        &mut self,
        request: ExternalAgentProbeRequestV2,
    ) -> Result<ExternalAgentProbeResultV2, String> {
        probe_external_agent(
            &mut self.credentials,
            &self.documents.settings().credentials,
            request,
        )
    }

    /// Resolves one exact unsaved project draft without persisting it or
    /// granting the workspace to a workflow.
    pub fn settings_v2_probe_project(
        &self,
        request: ProjectProbeRequestV2,
    ) -> ProjectProbeResultV2 {
        probe_project(request)
    }

    /// Exercises the installed built-in adapter using bounded, side-effect-free
    /// health behavior against the exact unsaved tool/project draft.
    pub fn settings_v2_probe_tool(&mut self, request: ToolProbeRequestV2) -> ToolProbeResultV2 {
        let api_key = request
            .tool
            .credential_bindings
            .first()
            .map(|binding| {
                self.credentials
                    .resolve_fields(
                        &binding.credential_ref,
                        BTreeSet::from([binding.field.clone()]),
                    )
                    .and_then(|mut fields| {
                        let bytes = fields.remove(&binding.field).ok_or_else(|| {
                            "credential store did not return the bound tool field".to_owned()
                        })?;
                        String::from_utf8(bytes.as_slice().to_vec())
                            .map(zeroize::Zeroizing::new)
                            .map_err(|_| "bound tool credential is not valid UTF-8".to_owned())
                    })
            })
            .transpose();
        match api_key {
            Ok(api_key) => probe_tool_with_api_key(request, api_key.as_deref().map(String::as_str)),
            Err(message) => ToolProbeResultV2 {
                ok: false,
                tool_id: request.tool.id.clone(),
                adapter: "unavailable".into(),
                message,
                draft_fingerprint: request.draft_fingerprint,
            },
        }
    }

    /// Reads and validates an inert extension manifest. No entry point is
    /// resolved or executed, and the result remains disabled and untrusted.
    pub fn settings_v2_inspect_extension(
        &self,
        manifest_path: &Path,
    ) -> Result<ExtensionConfigurationV2, String> {
        super::extension_inspection::inspect_extension_manifest_v2(manifest_path)
            .map_err(|error| error.to_string())
    }

    /// Registers one saved discovery after re-inspecting its exact manifest,
    /// compatibility, and entry-point bytes. Registration never executes,
    /// trusts, or enables the extension.
    pub fn settings_v2_register_extension(
        &mut self,
        input: ExtensionRegisterInputV2,
    ) -> Result<UiCommandReceipt, String> {
        validate_command_id(&input.command_id)?;
        StableId::parse(&input.extension_id).map_err(|_| {
            "extension registration requires a valid stable extension ID".to_owned()
        })?;
        let fingerprint = command_fingerprint(&input)?;
        if let Some(processed) = self.processed.get(&input.command_id) {
            return replay_processed(processed, &fingerprint);
        }
        let actual_version = self.settings_v2_snapshot().version;
        if input.expected_version != actual_version {
            return Err(format!(
                "settings version conflict: expected {}, actual {actual_version}",
                input.expected_version
            ));
        }
        let mut next = self.documents.settings().clone();
        let index = next
            .extensions
            .iter()
            .position(|extension| extension.id == input.extension_id)
            .ok_or_else(|| {
                format!(
                    "extension '{}' is not a saved discovered extension",
                    input.extension_id
                )
            })?;
        let registered = register_extension_installation_v2(&next.extensions[index])
            .map_err(|error| error.to_string())?;
        next.extensions[index] = registered;
        next.validate()?;
        next.validate_installed_runtime_consumers()?;
        let next_version = self.documents.save_settings(input.expected_version, next)?;
        let receipt = UiCommandReceipt {
            command_id: input.command_id.clone(),
            accepted: true,
            current_version: next_version,
            reason: Some(
                "The local extension package was verified and registered as a disabled, untrusted metadata record. This build can record explicit trust metadata but cannot enable, load, or execute the extension."
                    .into(),
            ),
            credential_mutation: None,
        };
        self.processed.insert(
            input.command_id,
            ProcessedCommand {
                fingerprint,
                receipt: receipt.clone(),
            },
        );
        Ok(receipt)
    }

    fn provider_operation_credential(
        &mut self,
        provider: &ProviderConfigurationV2,
        replacement: Option<zeroize::Zeroizing<String>>,
        use_stored: bool,
    ) -> Result<Option<String>, String> {
        if replacement.is_some() && use_stored {
            return Err(
                "Choose either the entered replacement credential or the stored credential, not both."
                    .into(),
            );
        }
        if let Some(replacement) = replacement {
            if replacement.is_empty() {
                return Err("replacement credential cannot be empty".into());
            }
            return Ok(Some(replacement.to_string()));
        }
        if !use_stored {
            return Ok(None);
        }
        let reference = provider
            .credential_ref
            .as_deref()
            .ok_or_else(|| "this provider draft has no stored credential reference".to_owned())?;
        let metadata = self
            .documents
            .settings()
            .credential(reference)
            .ok_or_else(|| "provider references unknown credential metadata".to_owned())?;
        if let Some(bound_provider_id) = metadata.bound_provider_id.as_deref()
            && (bound_provider_id != provider.id
                || metadata.bound_endpoint.as_deref() != Some(provider.base_url.as_str()))
        {
            return Err(
                "the stored credential is bound to another provider identity or endpoint".into(),
            );
        }
        self.credentials.resolve(Some(reference))
    }

    #[must_use]
    pub fn workflow_snapshot(&self) -> WorkflowSnapshot {
        self.documents.workflow_snapshot()
    }

    #[must_use]
    pub fn workflow_snapshot_for(&self, workflow_id: String) -> WorkflowSnapshot {
        self.documents.workflow_snapshot_for(&workflow_id)
    }

    #[must_use]
    pub fn workflow_library(&self) -> WorkflowLibrarySnapshot {
        self.documents.workflow_library()
    }

    pub fn workflow_commit(
        &mut self,
        input: WorkflowCommitInput,
    ) -> Result<UiCommandReceipt, String> {
        validate_command_id(&input.command_id)?;
        let fingerprint = command_fingerprint(&input)?;
        if let Some(processed) = self.processed.get(&input.command_id) {
            return replay_processed(processed, &fingerprint);
        }
        let next_version = match input.workflow_id.as_deref() {
            Some(workflow_id) => self.documents.save_workflow_document(
                workflow_id,
                input.expected_version,
                input.document,
            )?,
            None => self
                .documents
                .save_workflow(input.expected_version, input.document)?,
        };
        let receipt = UiCommandReceipt {
            command_id: input.command_id.clone(),
            accepted: true,
            current_version: next_version,
            reason: None,
            credential_mutation: None,
        };
        self.processed.insert(
            input.command_id,
            ProcessedCommand {
                fingerprint,
                receipt: receipt.clone(),
            },
        );
        Ok(receipt)
    }

    pub fn workflow_create(
        &mut self,
        input: WorkflowCreateInput,
    ) -> Result<WorkflowCreateReceipt, String> {
        validate_command_id(&input.command_id)?;
        let fingerprint = command_fingerprint(&input)?;
        if let Some(processed) = self.processed.get(&input.command_id) {
            return replay_create_receipt(processed, &fingerprint);
        }
        let (workflow_id, version) = self
            .documents
            .create_workflow(&input.name, input.template.as_deref())?;
        let receipt = WorkflowCreateReceipt {
            command_id: input.command_id.clone(),
            accepted: true,
            current_version: version,
            workflow_id,
        };
        self.processed.insert(
            input.command_id,
            ProcessedCommand {
                fingerprint,
                receipt: create_receipt_into_ui(&receipt),
            },
        );
        Ok(receipt)
    }

    pub fn workflow_duplicate(
        &mut self,
        input: WorkflowDuplicateInput,
    ) -> Result<WorkflowCreateReceipt, String> {
        validate_command_id(&input.command_id)?;
        let fingerprint = command_fingerprint(&input)?;
        if let Some(processed) = self.processed.get(&input.command_id) {
            return replay_create_receipt(processed, &fingerprint);
        }
        let (workflow_id, version) = self
            .documents
            .duplicate_workflow(&input.workflow_id, &input.name)?;
        let receipt = WorkflowCreateReceipt {
            command_id: input.command_id.clone(),
            accepted: true,
            current_version: version,
            workflow_id,
        };
        self.processed.insert(
            input.command_id,
            ProcessedCommand {
                fingerprint,
                receipt: create_receipt_into_ui(&receipt),
            },
        );
        Ok(receipt)
    }

    pub fn workflow_delete(
        &mut self,
        input: WorkflowTargetInput,
    ) -> Result<UiCommandReceipt, String> {
        validate_command_id(&input.command_id)?;
        let fingerprint = command_fingerprint(&input)?;
        if let Some(processed) = self.processed.get(&input.command_id) {
            return replay_processed(processed, &fingerprint);
        }
        self.documents.delete_workflow(&input.workflow_id)?;
        let receipt = UiCommandReceipt {
            command_id: input.command_id.clone(),
            accepted: true,
            current_version: self.documents.workflow_library().version,
            reason: None,
            credential_mutation: None,
        };
        self.processed.insert(
            input.command_id,
            ProcessedCommand {
                fingerprint,
                receipt: receipt.clone(),
            },
        );
        Ok(receipt)
    }

    pub fn workflow_rename(
        &mut self,
        input: WorkflowRenameInput,
    ) -> Result<UiCommandReceipt, String> {
        validate_command_id(&input.command_id)?;
        let fingerprint = command_fingerprint(&input)?;
        if let Some(processed) = self.processed.get(&input.command_id) {
            return replay_processed(processed, &fingerprint);
        }
        let next_version = self
            .documents
            .rename_workflow(&input.workflow_id, &input.name)?;
        let receipt = UiCommandReceipt {
            command_id: input.command_id.clone(),
            accepted: true,
            current_version: next_version,
            reason: None,
            credential_mutation: None,
        };
        self.processed.insert(
            input.command_id,
            ProcessedCommand {
                fingerprint,
                receipt: receipt.clone(),
            },
        );
        Ok(receipt)
    }

    pub fn workflow_set_default(
        &mut self,
        input: WorkflowTargetInput,
    ) -> Result<UiCommandReceipt, String> {
        validate_command_id(&input.command_id)?;
        let fingerprint = command_fingerprint(&input)?;
        if let Some(processed) = self.processed.get(&input.command_id) {
            return replay_processed(processed, &fingerprint);
        }
        let library_version = self.documents.set_default_workflow(&input.workflow_id)?;
        let receipt = UiCommandReceipt {
            command_id: input.command_id.clone(),
            accepted: true,
            current_version: library_version,
            reason: None,
            credential_mutation: None,
        };
        self.processed.insert(
            input.command_id,
            ProcessedCommand {
                fingerprint,
                receipt: receipt.clone(),
            },
        );
        Ok(receipt)
    }

    pub fn management_repair_snapshot(
        &self,
        after_sequence: u64,
    ) -> Result<ManagementRepairProjectionDto, String> {
        self.management_repair.snapshot(after_sequence)
    }

    pub fn management_repair_command(
        &mut self,
        command: ManagementRepairCommandInput,
        expected_version: u64,
    ) -> Result<ManagementRepairReceipt, String> {
        self.management_repair.command(command, expected_version)
    }
}

fn prepare_root(root: &Path) -> Result<std::path::PathBuf, String> {
    fs::create_dir_all(root)
        .map_err(|error| format!("cannot create desktop data directory: {error}"))?;
    fs::canonicalize(root)
        .map_err(|error| format!("cannot resolve desktop data directory: {error}"))
}

fn require_stored_credential_binding(provider: &ProviderDocument) -> Result<(), String> {
    if provider.credential_ref.is_none() {
        return Ok(());
    }
    if provider.credential_endpoint.as_deref() == Some(provider.base_url.as_str()) {
        Ok(())
    } else {
        Err(
            "The stored API key is not bound to the saved provider endpoint. Replace or clear it in Settings before using this provider."
                .into(),
        )
    }
}

fn parse_appearance_mode(value: &str) -> Result<AppearanceModeV2, String> {
    match value {
        "system" => Ok(AppearanceModeV2::System),
        "light" => Ok(AppearanceModeV2::Light),
        "dark" => Ok(AppearanceModeV2::Dark),
        _ => Err("settings appearance must be system, light, or dark".into()),
    }
}

fn legacy_provider_id(settings: &SettingsDocument) -> Option<&str> {
    settings
        .providers
        .iter()
        .find(|provider| provider.id == "provider.primary" && provider.enabled)
        .or_else(|| settings.providers.iter().find(|provider| provider.enabled))
        .map(|provider| provider.id.as_str())
}

fn apply_legacy_provider(
    settings: &mut SettingsDocument,
    provider_id: &str,
    legacy: &ProviderDocument,
) -> Result<(), String> {
    let configured = !legacy.base_url.is_empty() || !legacy.model.is_empty();
    if !configured {
        settings
            .providers
            .retain(|provider| provider.id != provider_id);
        remove_provider_from_tiers(settings, provider_id);
        return Ok(());
    }
    require_provider_fields(&legacy.base_url, &legacy.model)?;
    let mut target_model_id = "model.primary".to_owned();
    if let Some(provider) = settings
        .providers
        .iter_mut()
        .find(|provider| provider.id == provider_id)
    {
        provider.base_url = legacy.base_url.clone();
        provider.enabled = true;
        provider.credential_ref.clone_from(&legacy.credential_ref);
        let selected_model = provider
            .models
            .iter()
            .position(|model| model.id == "model.primary")
            .or_else(|| provider.models.iter().position(|model| model.enabled));
        if let Some(index) = selected_model {
            let model = &mut provider.models[index];
            model.name = legacy.model.clone();
            model.remote_id = legacy.model.clone();
            model.enabled = true;
            target_model_id.clone_from(&model.id);
        } else {
            provider.models.push(legacy_model(legacy));
        }
    } else {
        settings.providers.push(ProviderConfigurationV2 {
            id: provider_id.into(),
            name: "Primary provider".into(),
            kind: "openai_compatible".into(),
            base_url: legacy.base_url.clone(),
            enabled: true,
            credential_ref: legacy.credential_ref.clone(),
            models: vec![legacy_model(legacy)],
            configuration: BTreeMap::new(),
        });
    }
    let target = ModelTargetV2 {
        provider_id: provider_id.into(),
        model_id: target_model_id,
    };
    for tier in &mut settings.model_tiers {
        if matches!(tier.resolution, ModelTierResolutionV2::Unconfigured) {
            tier.resolution = ModelTierResolutionV2::Exact {
                target: target.clone(),
            };
        }
    }
    Ok(())
}

fn legacy_model(legacy: &ProviderDocument) -> ModelConfigurationV2 {
    ModelConfigurationV2 {
        id: "model.primary".into(),
        name: legacy.model.clone(),
        remote_id: legacy.model.clone(),
        enabled: true,
        context_window: None,
        max_output_tokens: None,
        capabilities: vec!["text".into(), "tools".into()],
        parameters: BTreeMap::new(),
    }
}

fn remove_provider_from_tiers(settings: &mut SettingsDocument, provider_id: &str) {
    for tier in &mut settings.model_tiers {
        tier.resolution = match tier.resolution.clone() {
            ModelTierResolutionV2::Exact { target } if target.provider_id == provider_id => {
                ModelTierResolutionV2::Unconfigured
            }
            ModelTierResolutionV2::Fallback { mut targets } => {
                targets.retain(|target| target.provider_id != provider_id);
                match targets.len() {
                    0 => ModelTierResolutionV2::Unconfigured,
                    1 => ModelTierResolutionV2::Exact {
                        target: targets.remove(0),
                    },
                    _ => ModelTierResolutionV2::Fallback { targets },
                }
            }
            ModelTierResolutionV2::Policy {
                mut candidates,
                preference,
            } => {
                candidates.retain(|target| target.provider_id != provider_id);
                if candidates.is_empty() {
                    ModelTierResolutionV2::Unconfigured
                } else {
                    ModelTierResolutionV2::Policy {
                        candidates,
                        preference,
                    }
                }
            }
            resolution => resolution,
        };
    }
}

fn validate_credential_metadata_update(
    previous: &SettingsDocument,
    next: &SettingsDocument,
) -> Result<(), String> {
    let previous = previous
        .credentials
        .iter()
        .map(|credential| (credential.credential_ref.as_str(), credential))
        .collect::<BTreeMap<_, _>>();
    let next = next
        .credentials
        .iter()
        .map(|credential| (credential.credential_ref.as_str(), credential))
        .collect::<BTreeMap<_, _>>();
    if previous != next {
        return Err(
            "credential metadata cannot be added, changed, or removed through Save Configuration; use the dedicated credential command"
                .into(),
        );
    }
    Ok(())
}

struct FrozenWorkflowAgentV1 {
    run_deadline_millis: u64,
    tools: Vec<FrozenToolBindingV1>,
}

/// Freezes the tool subset for a catalog-valid graph workflow. The retained
/// run-deadline value is legacy frozen-context metadata and is not enforced.
/// the union of every agent/tool node's bindings, resolved only against saved
/// enabled Settings records. Only tools this build can execute survive the
/// pipeline freeze; a node binding anything else fails closed.
/// The distinct `mcp://<server>/<tool>` capability ids referenced by the
/// graph's agent and tool nodes, in document order.
fn graph_mcp_tool_ids(workflow: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    let mut seen = BTreeSet::new();
    if let Some(nodes) = workflow.get("nodes").and_then(Value::as_array) {
        for node in nodes {
            let node_type = node.get("type").and_then(Value::as_str).unwrap_or_default();
            let configuration = node.get("configuration").and_then(Value::as_object);
            let candidate_ids: Vec<String> = match node_type {
                "agent" => configuration
                    .and_then(|config| config.get("toolIds"))
                    .and_then(Value::as_array)
                    .map(|ids| {
                        ids.iter()
                            .filter_map(|id| id.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default(),
                "tool" => configuration
                    .and_then(|config| config.get("toolId"))
                    .and_then(Value::as_str)
                    .map(|tool_id| vec![tool_id.to_owned()])
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            for id in candidate_ids {
                if id.starts_with(MCP_CAPABILITY_PREFIX) && seen.insert(id.clone()) {
                    ids.push(id);
                }
            }
        }
    }
    ids
}

fn freeze_graph_bindings(
    workflow: &Value,
    settings: &SettingsConfigurationV2,
    has_project: bool,
    mcp_definitions: &BTreeMap<String, ModelToolDefinitionV1>,
) -> Result<FrozenWorkflowAgentV1, String> {
    let nodes = workflow
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| "workflow nodes are missing".to_owned())?;
    let mut seen = BTreeSet::new();
    let mut tools = Vec::new();
    for node in nodes {
        let node_id = node
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "workflow node id is missing".to_owned())?;
        let node_type = node.get("type").and_then(Value::as_str).unwrap_or_default();
        let configuration = node.get("configuration").and_then(Value::as_object);
        let tool_ids: Vec<String> = match node_type {
            "agent" => configuration
                .and_then(|config| config.get("toolIds"))
                .and_then(Value::as_array)
                .map(|ids| {
                    ids.iter()
                        .map(|id| id.as_str().map(str::to_owned))
                        .collect::<Option<Vec<_>>>()
                })
                .ok_or_else(|| format!("workflow node '{node_id}' toolIds must be an array"))?
                .unwrap_or_default(),
            "tool" => configuration
                .and_then(|config| config.get("toolId"))
                .and_then(Value::as_str)
                .map(|tool_id| vec![tool_id.to_owned()])
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        for tool_id in tool_ids {
            if !seen.insert(tool_id.clone()) {
                continue;
            }
            if tool_id.starts_with(MCP_CAPABILITY_PREFIX) {
                let definition = mcp_definitions.get(&tool_id).cloned().ok_or_else(|| {
                    format!("workflow MCP tool '{tool_id}' has no frozen definition")
                })?;
                let (server_id, tool) = split_mcp_capability(&tool_id).map_err(|error| error)?;
                let snapshot = BuiltInToolConfigurationV2 {
                    id: tool_id.clone(),
                    name: mcp_provider_name(server_id, tool),
                    enabled: true,
                    requires_project: false,
                    credential_bindings: Vec::new(),
                    configuration: BTreeMap::from([
                        ("serverId".to_owned(), Value::String(server_id.to_owned())),
                        ("tool".to_owned(), Value::String(tool.to_owned())),
                    ]),
                };
                let tool_hash = canonical_hash(&snapshot)?;
                tools.push(FrozenToolBindingV1 {
                    tool_id,
                    tool_hash,
                    tool_snapshot: snapshot,
                    credentials: Vec::new(),
                    definition: Some(definition),
                });
                continue;
            }
            let configured = settings
                .tools
                .iter()
                .find(|tool| tool.id == tool_id)
                .ok_or_else(|| format!("workflow tool '{tool_id}' is not installed"))?;
            if !configured.enabled {
                return Err(format!(
                    "workflow tool '{tool_id}' is disabled in saved Settings"
                ));
            }
            if configured.requires_project && !has_project {
                return Err(format!(
                    "workflow tool '{tool_id}' requires selecting a saved project before the first input"
                ));
            }
            tools.push(FrozenToolBindingV1 {
                tool_id,
                tool_hash: canonical_hash(configured)?,
                tool_snapshot: configured.clone(),
                credentials: freeze_tool_credentials(configured, settings)?,
                definition: None,
            });
        }
    }
    // `tool.subagent` owns a fresh child loop whose declared contract includes
    // the enabled read-only child subset. Freeze those bindings with the Run
    // even though they are not exposed to the parent agent node. The generic
    // graph compiler still exposes only the toolIds written in that node's JSON.
    if seen.contains(SUBAGENT_CAPABILITY_ID) {
        for child_id in SUBAGENT_CHILD_TOOL_IDS {
            if seen.contains(child_id) {
                continue;
            }
            let configured = settings
                .tools
                .iter()
                .find(|tool| tool.id == child_id)
                .ok_or_else(|| format!("subagent child tool '{child_id}' is not installed"))?;
            if !configured.enabled || (configured.requires_project && !has_project) {
                continue;
            }
            seen.insert(child_id.to_owned());
            tools.push(FrozenToolBindingV1 {
                tool_id: child_id.to_owned(),
                tool_hash: canonical_hash(configured)?,
                tool_snapshot: configured.clone(),
                credentials: freeze_tool_credentials(configured, settings)?,
                definition: None,
            });
        }
    }
    Ok(FrozenWorkflowAgentV1 {
        // Preserve the durable field for old Chat records without deriving
        // execution behavior from the removed Agent timeoutSeconds setting.
        run_deadline_millis: DEFAULT_MODEL_CALL_TIMEOUT_SECONDS.saturating_mul(1_000),
        tools,
    })
}

fn freeze_tool_credentials(
    tool: &BuiltInToolConfigurationV2,
    settings: &SettingsConfigurationV2,
) -> Result<Vec<FrozenCredentialBindingV1>, String> {
    let mut references = BTreeSet::new();
    let mut frozen = Vec::new();
    for binding in &tool.credential_bindings {
        if !references.insert(binding.credential_ref.as_str()) {
            continue;
        }
        let metadata = settings
            .credential(&binding.credential_ref)
            .ok_or_else(|| {
                format!(
                    "tool '{}' references missing credential '{}'",
                    tool.id, binding.credential_ref
                )
            })?;
        frozen.push(FrozenCredentialBindingV1 {
            credential_ref: StableId::parse(metadata.credential_ref.clone())
                .map_err(|error| error.to_string())?,
            field_names: metadata.field_names.iter().cloned().collect(),
            revision: metadata.revision,
        });
    }
    Ok(frozen)
}

/// The distinct model tiers referenced by the graph's model-consuming nodes.
fn graph_model_tier_ids(workflow: &Value) -> Vec<String> {
    let mut tiers = BTreeSet::new();
    if let Some(nodes) = workflow.get("nodes").and_then(Value::as_array) {
        for node in nodes {
            if !matches!(
                node.get("type").and_then(Value::as_str),
                Some("agent" | "model_call")
            ) {
                continue;
            }
            if let Some(tier) = node
                .get("configuration")
                .and_then(|config| config.get("modelTierId"))
                .and_then(Value::as_str)
            {
                tiers.insert(tier.to_owned());
            }
        }
    }
    tiers.into_iter().collect()
}

fn validate_model_capabilities(
    provider: &ProviderConfigurationV2,
    model: &ModelConfigurationV2,
    requires_tools: bool,
) -> Result<(), String> {
    if !model
        .capabilities
        .iter()
        .any(|capability| capability == "text")
    {
        return Err(format!(
            "model '{}' does not declare the text capability required by the workflow",
            model.name
        ));
    }
    if requires_tools
        && !model
            .capabilities
            .iter()
            .any(|capability| capability == "tools")
        && !provider_supports_tool_calls(&provider.kind)
    {
        return Err(format!(
            "provider protocol '{}' cannot transport the tool calls required by model '{}' and the selected workflow",
            provider.kind, model.name
        ));
    }
    Ok(())
}

fn validate_workflow_model_parameters(
    workflow: &Value,
    provider: &ProviderConfigurationV2,
    model: &ModelConfigurationV2,
) -> Result<(), String> {
    let advertised_efforts = model
        .capabilities
        .iter()
        .filter_map(|capability| capability.strip_prefix("reasoning_effort:"))
        .collect::<BTreeSet<_>>();
    let Some(nodes) = workflow.get("nodes").and_then(Value::as_array) else {
        return Ok(());
    };
    for node in nodes {
        if !matches!(
            node.get("type").and_then(Value::as_str),
            Some("agent" | "model_call")
        ) {
            continue;
        }
        let Some(configuration) = node.get("configuration").and_then(Value::as_object) else {
            continue;
        };
        let has_override = ["reasoningEffort", "enableThinking"]
            .into_iter()
            .any(|key| configuration.get(key).is_some_and(|value| !value.is_null()));
        if !has_override {
            continue;
        }
        let node_id = node.get("id").and_then(Value::as_str).unwrap_or("unknown");
        if provider.kind != "openai_compatible" {
            return Err(format!(
                "workflow node '{node_id}' sets OpenAI-compatible reasoning controls, but tier resolution selected the '{}' provider adapter",
                provider.kind
            ));
        }
        if let Some(effort) = configuration.get("reasoningEffort").and_then(Value::as_str)
            && !advertised_efforts.is_empty()
            && !advertised_efforts.contains(effort)
        {
            return Err(format!(
                "workflow node '{node_id}' selects reasoningEffort '{effort}', but model '{}' advertised only: {}",
                model.name,
                advertised_efforts
                    .iter()
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    Ok(())
}

struct ResolvedWorkflowModel {
    tier: ModelTierConfigurationV2,
    provider: ProviderConfigurationV2,
    model: ModelConfigurationV2,
    credential: Option<CredentialMetadataV1>,
}

fn resolve_workflow_model(
    settings: &SettingsDocument,
    tier_id: &str,
) -> Result<ResolvedWorkflowModel, String> {
    let tier = settings
        .model_tiers
        .iter()
        .find(|tier| tier.id == tier_id)
        .ok_or_else(|| format!("model tier '{tier_id}' is missing"))?;
    let target = match &tier.resolution {
        ModelTierResolutionV2::Unconfigured => {
            return Err(format!(
                "model tier '{tier_id}' is Unconfigured; map it in Settings before running the workflow"
            ));
        }
        ModelTierResolutionV2::Exact { target } => target,
        ModelTierResolutionV2::Fallback { .. } => {
            return Err(format!(
                "model tier '{tier_id}' uses Ordered fallback, but the current workflow runtime executes only an Exact provider/model mapping; change this tier to Exact in Settings"
            ));
        }
        ModelTierResolutionV2::Policy { .. } => {
            return Err(format!(
                "model tier '{tier_id}' uses a Selection policy, but the current workflow runtime executes only an Exact provider/model mapping; change this tier to Exact in Settings"
            ));
        }
    };
    let provider = settings
        .providers
        .iter()
        .find(|provider| provider.id == target.provider_id && provider.enabled)
        .ok_or_else(|| {
            format!(
                "model tier '{tier_id}' Exact provider '{}' is missing or disabled; repair its mapping in Settings",
                target.provider_id
            )
        })?;
    if !matches!(
        provider.kind.as_str(),
        "openai_compatible" | "anthropic" | "gemini"
    ) {
        return Err(format!(
            "provider '{}' uses protocol '{}', but no native workflow adapter is installed",
            provider.id, provider.kind
        ));
    }
    let model = provider
        .models
        .iter()
        .find(|model| model.id == target.model_id && model.enabled)
        .ok_or_else(|| {
            format!(
                "model tier '{tier_id}' Exact model '{}:{}' is missing or disabled; repair its mapping in Settings",
                target.provider_id, target.model_id
            )
        })?;
    require_consumed_provider_options(provider, Some(model))?;
    resolved_model(settings, tier, provider, model)
}

fn resolved_model(
    settings: &SettingsDocument,
    tier: &ModelTierConfigurationV2,
    provider: &ProviderConfigurationV2,
    model: &ModelConfigurationV2,
) -> Result<ResolvedWorkflowModel, String> {
    let credential = provider
        .credential_ref
        .as_deref()
        .map(|reference| -> Result<CredentialMetadataV1, String> {
            let metadata = settings.credential(reference).ok_or_else(|| {
                format!(
                    "provider '{}' references an unknown credential",
                    provider.id
                )
            })?;
            if !metadata.field_names.iter().any(|field| field == "api_key") {
                return Err(format!(
                    "provider '{}' credential '{}' has no api_key field required by the installed adapter",
                    provider.id, reference
                ));
            }
            Ok(CredentialMetadataV1 {
                credential: CredentialRef(
                    StableId::parse(reference.to_owned()).map_err(|error| error.to_string())?,
                ),
                field_names: metadata.field_names.iter().cloned().collect(),
                revision: metadata.revision,
            })
        })
        .transpose()?;
    Ok(ResolvedWorkflowModel {
        tier: tier.clone(),
        provider: provider.clone(),
        model: model.clone(),
        credential,
    })
}

fn run_terminal_fact(
    request_id: &StableId,
    run_id: &StableId,
    status: &str,
    body: &str,
    output: Option<Value>,
    created_at: &str,
) -> Value {
    json!({
        "schemaVersion": 1,
        "requestId": request_id,
        "runId": run_id,
        "spanId": format!("span.run.{run_id}.{request_id}"),
        "status": status,
        "body": body,
        "createdAt": created_at,
        "hasOutput": output.is_some(),
        "output": output,
    })
}

fn run_waiting_fact(request_id: &StableId, run_id: &StableId, created_at: &str) -> Value {
    json!({
        "schemaVersion": 1,
        "requestId": request_id,
        "runId": run_id,
        "spanId": format!("span.run.{run_id}.{request_id}"),
        "status": "waiting",
        "body": "Run is waiting for approval.",
        "createdAt": created_at,
    })
}

const fn execution_status_name(status: WorkflowExecutionStatusV1) -> &'static str {
    match status {
        WorkflowExecutionStatusV1::Succeeded => "succeeded",
        WorkflowExecutionStatusV1::FailedDefinitelyNotStarted => "failed_definitely_not_started",
        WorkflowExecutionStatusV1::FailedKnownStarted => "failed_known_started",
        WorkflowExecutionStatusV1::OutcomeUncertain => "outcome_uncertain",
        WorkflowExecutionStatusV1::AwaitingApproval => "awaiting_approval",
    }
}

fn current_epoch_millis() -> Result<u64, String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_owned())?
        .as_millis();
    u64::try_from(millis).map_err(|_| "system clock value is out of range".to_owned())
}

fn replace_credential_references(
    settings: &mut SettingsDocument,
    previous: &str,
    replacement: &str,
) {
    for provider in &mut settings.providers {
        if provider.credential_ref.as_deref() == Some(previous) {
            provider.credential_ref = Some(replacement.to_owned());
        }
    }
    for tool in &mut settings.tools {
        replace_bindings(&mut tool.credential_bindings, previous, replacement);
    }
    for server in &mut settings.mcp_servers {
        replace_transport_bindings(&mut server.transport, previous, replacement);
    }
    for agent in &mut settings.external_agents {
        let invalidates_capabilities = agent
            .credential_bindings
            .iter()
            .chain(transport_bindings(&agent.connection).iter())
            .any(|binding| binding.credential_ref == previous);
        replace_bindings(&mut agent.credential_bindings, previous, replacement);
        replace_transport_bindings(&mut agent.connection, previous, replacement);
        if invalidates_capabilities {
            agent.capabilities = Default::default();
        }
    }
}

fn transport_bindings(
    transport: &IntegrationTransportV2,
) -> &[super::settings_v2::NamedCredentialBindingV2] {
    match transport {
        IntegrationTransportV2::Http { headers, .. } => headers,
        IntegrationTransportV2::Stdio { env, .. } => env,
    }
}

fn replace_transport_bindings(
    transport: &mut IntegrationTransportV2,
    previous: &str,
    replacement: &str,
) {
    let bindings = match transport {
        IntegrationTransportV2::Http { headers, .. } => headers,
        IntegrationTransportV2::Stdio { env, .. } => env,
    };
    replace_bindings(bindings, previous, replacement);
}

fn replace_bindings(
    bindings: &mut [super::settings_v2::NamedCredentialBindingV2],
    previous: &str,
    replacement: &str,
) {
    for binding in bindings {
        if binding.credential_ref == previous {
            binding.credential_ref = replacement.to_owned();
        }
    }
}

fn credential_is_referenced(settings: &SettingsDocument, reference: &str) -> bool {
    settings
        .providers
        .iter()
        .any(|provider| provider.credential_ref.as_deref() == Some(reference))
        || settings.tools.iter().any(|tool| {
            tool.credential_bindings
                .iter()
                .any(|binding| binding.credential_ref == reference)
        })
        || settings
            .mcp_servers
            .iter()
            .any(|server| transport_references_credential(&server.transport, reference))
        || settings.external_agents.iter().any(|agent| {
            agent
                .credential_bindings
                .iter()
                .any(|binding| binding.credential_ref == reference)
                || transport_references_credential(&agent.connection, reference)
        })
}

fn validate_provider_operation_draft(provider: &ProviderConfigurationV2) -> Result<(), String> {
    validate_command_id(&provider.id)
        .map_err(|_| format!("provider id '{}' is invalid", provider.id))?;
    if provider.name.trim().is_empty() {
        return Err("provider name cannot be empty".into());
    }
    if !matches!(
        provider.kind.as_str(),
        "openai_compatible" | "anthropic" | "gemini"
    ) {
        return Err(format!(
            "provider protocol '{}' is not recognized",
            provider.kind
        ));
    }
    validate_http_url("provider base URL", &provider.base_url)?;
    require_consumed_provider_options(provider, None)?;
    Ok(())
}

fn require_consumed_provider_options(
    provider: &ProviderConfigurationV2,
    selected_model: Option<&ModelConfigurationV2>,
) -> Result<(), String> {
    provider.runtime_limits()?;
    let models = selected_model
        .map(std::slice::from_ref)
        .unwrap_or(provider.models.as_slice());
    for model in models {
        validate_consumed_model_parameters(provider, model)?;
    }
    Ok(())
}

fn validate_consumed_model_parameters(
    provider: &ProviderConfigurationV2,
    model: &ModelConfigurationV2,
) -> Result<(), String> {
    if model.parameters.is_empty() {
        return Ok(());
    }
    if provider.kind != "openai_compatible" {
        return Err(format!(
            "model '{}:{}' has parameters, but the '{}' adapter has no consumer for those fields",
            provider.id, model.id, provider.kind
        ));
    }
    for (key, value) in &model.parameters {
        let valid = match key.as_str() {
            "reasoningEffort" => value.as_str().is_some_and(|value| {
                matches!(
                    value,
                    "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
                )
            }),
            "enableThinking" | "preserveThinking" => value.is_boolean(),
            _ => false,
        };
        if !valid {
            return Err(format!(
                "model '{}:{}' parameter '{}' is unsupported or invalid; OpenAI-compatible models accept reasoningEffort (none, minimal, low, medium, high, xhigh, or max), enableThinking (boolean), and preserveThinking (boolean)",
                provider.id, model.id, key
            ));
        }
    }
    Ok(())
}

fn saved_provider_matches(settings: &SettingsDocument, draft: &ProviderConfigurationV2) -> bool {
    settings
        .providers
        .iter()
        .any(|configured| configured == draft)
}

fn transport_references_credential(transport: &IntegrationTransportV2, reference: &str) -> bool {
    let bindings = match transport {
        IntegrationTransportV2::Http { headers, .. } => headers,
        IntegrationTransportV2::Stdio { env, .. } => env,
    };
    bindings
        .iter()
        .any(|binding| binding.credential_ref == reference)
}

fn require_provider_fields(base_url: &str, model: &str) -> Result<(), String> {
    if base_url.is_empty() || model.is_empty() {
        Err("provider base URL and model are both required".into())
    } else {
        Ok(())
    }
}

fn string_field(payload: &Value, name: &str) -> Result<String, String> {
    payload
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("command payload requires non-empty {name}"))
}

fn required_chat_target(input: &UiCommandInput) -> Result<String, String> {
    input
        .target_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("{} requires a Chat target", input.action))
}

fn forked_message_payload(
    event: &aworkit_local_store::Event,
    parent_chat_id: &StableId,
    child_run_id: &StableId,
) -> Value {
    let mut object = event.payload.as_object().cloned().unwrap_or_default();
    for field in [
        "commandId",
        "commandHash",
        "resultHead",
        "requestId",
        "settlesCommandId",
    ] {
        object.remove(field);
    }
    object.insert("schemaVersion".into(), Value::from(1));
    object.insert("runId".into(), Value::String(child_run_id.to_string()));
    object.insert(
        "copiedFromEventId".into(),
        Value::String(event.event_id.clone()),
    );
    object.insert(
        "parentChatId".into(),
        Value::String(parent_chat_id.to_string()),
    );
    Value::Object(object)
}

fn optional_project_id(payload: &Value) -> Result<Option<String>, String> {
    match payload.get("projectId") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.clone())),
        Some(_) => Err("projectId must be null or a non-empty saved project ID".into()),
    }
}

/// Run-local task-list fact: when the pass settled a completed todo call,
/// the newest durable snapshot becomes a canonical semantic event for the UI reducer.
fn todo_state_fact(
    pipeline: &dyn WorkflowPipelinePort,
    result: &WorkflowExecutionResultV1,
    run_id: &StableId,
    created_at: &str,
) -> Result<Vec<(&'static str, Value)>, String> {
    if !result
        .tool_activity
        .iter()
        .any(|activity| activity.capability_id == "tool.todo" && activity.status == "completed")
    {
        return Ok(Vec::new());
    }
    let Some(todos) = pipeline.run_todo_state(run_id)? else {
        return Ok(Vec::new());
    };
    Ok(vec![(
        "tool.todo",
        json!({
            "todos": todos,
            "runId": run_id,
            "createdAt": created_at,
        }),
    )])
}

fn validate_command_id(value: &str) -> Result<(), String> {
    StableId::parse(value.to_owned())
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn command_fingerprint(command: &impl Serialize) -> Result<String, String> {
    let mut writer = DigestWriter(Sha256::new());
    serde_json::to_writer(&mut writer, command)
        .map_err(|error| format!("invalid desktop command: {error}"))?;
    Ok(format!("sha256:{:x}", writer.0.finalize()))
}

/// Streams command serialization into the digest so secret-bearing settings
/// commands never create a second plaintext JSON allocation.
struct DigestWriter(Sha256);

impl Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn replay_processed(
    processed: &ProcessedCommand,
    fingerprint: &str,
) -> Result<UiCommandReceipt, String> {
    if processed.fingerprint == fingerprint {
        Ok(processed.receipt.clone())
    } else {
        Err("desktop command ID was reused with different content".into())
    }
}

fn create_receipt_into_ui(receipt: &WorkflowCreateReceipt) -> UiCommandReceipt {
    UiCommandReceipt {
        command_id: receipt.command_id.clone(),
        accepted: receipt.accepted,
        current_version: receipt.current_version,
        reason: Some(receipt.workflow_id.clone()),
        credential_mutation: None,
    }
}

fn replay_create_receipt(
    processed: &ProcessedCommand,
    fingerprint: &str,
) -> Result<WorkflowCreateReceipt, String> {
    if processed.fingerprint != fingerprint {
        return Err("desktop command ID was reused with different content".into());
    }
    let receipt = &processed.receipt;
    Ok(WorkflowCreateReceipt {
        command_id: receipt.command_id.clone(),
        accepted: receipt.accepted,
        current_version: receipt.current_version,
        workflow_id: receipt
            .reason
            .clone()
            .ok_or_else(|| "stored workflow-create replay has no workflow id".to_owned())?,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::Path,
        sync::{
            Arc, Condvar, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        thread,
        time::Duration,
    };

    use aworkit_capability_host::CancellationToken;
    use aworkit_local_store::{DocumentKind, DocumentRepository, JsonDocument, RepositoryRoot};
    use aworkit_trusted_core::{
        CredentialReadAuthorizationV1, CredentialRef, CredentialSecretV1, MemoryCredentialStore,
        PlatformCredentialStorePort, SecretError,
    };
    use tempfile::TempDir;

    use super::super::provider::ProviderCompletion;
    use super::*;
    use crate::runtime::{
        CredentialMetadataConfigurationV2, ExtensionConfigurationV2, ExtensionStatusV2,
        ExternalAgentCapabilitiesV2, ExternalAgentConfigurationV2, IntegrationTransportV2,
        McpServerConfigurationV2, ModelTierConfigurationV2, ModelTierKindV2, ModelTierResolutionV2,
        ProjectConfigurationV2, ProviderCommitInput, ProviderConfigurationV2,
        ProviderSettingsSnapshot, WorkspaceConfigurationV2, WorkspaceKindV2,
    };

    mod credentialed_web_search;
    mod image_chat;

    struct FixtureProvider {
        calls: AtomicUsize,
        connection_tests: AtomicUsize,
        conversations: Mutex<Vec<Vec<ConversationMessage>>>,
        execution_requests: Mutex<Vec<WorkflowExecutionRequestV1>>,
    }

    impl FixtureProvider {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                connection_tests: AtomicUsize::new(0),
                conversations: Mutex::new(Vec::new()),
                execution_requests: Mutex::new(Vec::new()),
            }
        }
    }

    impl ProviderPort for FixtureProvider {
        fn validate(
            &self,
            _kind: &str,
            _base_url: &str,
            _model: &str,
            _request_timeout: Duration,
        ) -> Result<(), String> {
            Ok(())
        }

        fn test_connection(
            &self,
            _kind: &str,
            _base_url: &str,
            model: &str,
            _api_key: Option<String>,
            _request_timeout: Duration,
        ) -> ProviderTestResult {
            self.connection_tests.fetch_add(1, Ordering::SeqCst);
            ProviderTestResult {
                ok: true,
                message: "fixture ready".into(),
                model: Some(model.into()),
            }
        }

        fn complete(
            &self,
            _base_url: &str,
            model: &str,
            _api_key: Option<String>,
            messages: &[ConversationMessage],
        ) -> Result<ProviderCompletion, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.conversations.lock().unwrap().push(messages.to_vec());
            let prompt = &messages.last().expect("user input").content;
            Ok(ProviderCompletion {
                text: format!("fixture: {prompt}"),
                input_units: 3,
                output_units: 4,
                model: model.into(),
            })
        }

        fn discover_models(
            &self,
            kind: &str,
            _base_url: &str,
            _api_key: Option<String>,
            _request_timeout: Duration,
        ) -> Result<Vec<super::super::provider::DiscoveredProviderModel>, String> {
            if kind != "openai_compatible" {
                return Err("unsupported fixture protocol".into());
            }
            Ok(vec![super::super::provider::DiscoveredProviderModel {
                remote_id: "fixture-model".into(),
                name: "Fixture model".into(),
                context_window: Some(32_768),
                max_output_tokens: Some(4_096),
                capabilities: vec!["text".into(), "tools".into()],
            }])
        }
    }

    struct FixtureWorkflowPipeline {
        provider: Arc<FixtureProvider>,
    }

    impl WorkflowPipelinePort for FixtureWorkflowPipeline {
        fn execute(
            &self,
            request: WorkflowExecutionRequestV1,
        ) -> Result<WorkflowExecutionResultV1, String> {
            self.provider
                .execution_requests
                .lock()
                .unwrap()
                .push(request.clone());
            let messages = request
                .messages
                .iter()
                .map(|message| ConversationMessage {
                    images: message.images.clone(),
                    role: message.role.clone(),
                    content: message.content.clone(),
                })
                .collect::<Vec<_>>();
            let completion = self.provider.complete(
                &request.provider.base_url,
                &request.provider.model,
                None,
                &messages,
            )?;
            Ok(WorkflowExecutionResultV1 {
                request_id: request.request_id,
                chat_id: request.chat_id,
                run_id: request.run_id,
                snapshot_id: fixture_id("snapshot.fixture")?,
                snapshot_hash: format!("sha256:{}", "1".repeat(64)),
                authority_manifest_id: fixture_id("manifest.fixture")?,
                worker_invocation_id: fixture_id("invocation.worker.fixture")?,
                broker_invocation_id: fixture_id("invocation.broker.fixture")?,
                outcome_hash: format!("sha256:{}", "2".repeat(64)),
                status: WorkflowExecutionStatusV1::Succeeded,
                assistant_text: Some(completion.text),
                reasoning: None,
                error: None,
                model: completion.model,
                input_units: completion.input_units,
                output_units: completion.output_units,
                model_turns: 1,
                tool_calls: 0,
                tool_activity: Vec::new(),
                node_activity: Vec::new(),
                approval: None,
                replayed: false,
            })
        }
    }

    struct StopAwareWorkflowPipeline {
        cancellation_controller: WorkflowCancellationController,
        started: Arc<(Mutex<bool>, Condvar)>,
    }

    impl WorkflowPipelinePort for StopAwareWorkflowPipeline {
        fn execute(
            &self,
            request: WorkflowExecutionRequestV1,
        ) -> Result<WorkflowExecutionResultV1, String> {
            let cancellation = CancellationToken::default();
            let _active = self.cancellation_controller.register(
                request.chat_id.as_str(),
                request.run_id.as_str(),
                cancellation.clone(),
            )?;
            let (started, ready) = &*self.started;
            *started.lock().unwrap() = true;
            ready.notify_all();
            while !cancellation.is_cancelled() {
                thread::sleep(Duration::from_millis(5));
            }
            Ok(WorkflowExecutionResultV1 {
                request_id: request.request_id,
                chat_id: request.chat_id,
                run_id: request.run_id,
                snapshot_id: fixture_id("snapshot.stopped")?,
                snapshot_hash: format!("sha256:{}", "8".repeat(64)),
                authority_manifest_id: fixture_id("manifest.stopped")?,
                worker_invocation_id: fixture_id("invocation.worker.stopped")?,
                broker_invocation_id: fixture_id("invocation.broker.stopped")?,
                outcome_hash: format!("sha256:{}", "9".repeat(64)),
                status: WorkflowExecutionStatusV1::OutcomeUncertain,
                assistant_text: None,
                reasoning: None,
                error: Some("provider execution was cancelled".into()),
                model: request.provider.model,
                input_units: 0,
                output_units: 0,
                model_turns: 0,
                tool_calls: 0,
                tool_activity: Vec::new(),
                node_activity: Vec::new(),
                approval: None,
                replayed: false,
            })
        }
    }

    struct DurableRecoveryPipeline {
        provider: Arc<FixtureProvider>,
        settled: Mutex<Option<WorkflowExecutionResultV1>>,
        fail_after_first_settlement: AtomicBool,
    }

    impl DurableRecoveryPipeline {
        fn new(provider: Arc<FixtureProvider>) -> Self {
            Self {
                provider,
                settled: Mutex::new(None),
                fail_after_first_settlement: AtomicBool::new(true),
            }
        }
    }

    impl WorkflowPipelinePort for DurableRecoveryPipeline {
        fn execute(
            &self,
            request: WorkflowExecutionRequestV1,
        ) -> Result<WorkflowExecutionResultV1, String> {
            self.provider
                .execution_requests
                .lock()
                .unwrap()
                .push(request.clone());
            if let Some(mut settled) = self.settled.lock().unwrap().clone() {
                settled.replayed = true;
                return Ok(settled);
            }
            let messages = request
                .messages
                .iter()
                .map(|message| ConversationMessage {
                    images: message.images.clone(),
                    role: message.role.clone(),
                    content: message.content.clone(),
                })
                .collect::<Vec<_>>();
            let completion = self.provider.complete(
                &request.provider.base_url,
                &request.provider.model,
                None,
                &messages,
            )?;
            let result = WorkflowExecutionResultV1 {
                request_id: request.request_id,
                chat_id: request.chat_id,
                run_id: request.run_id,
                snapshot_id: fixture_id("snapshot.durable-recovery")?,
                snapshot_hash: format!("sha256:{}", "6".repeat(64)),
                authority_manifest_id: fixture_id("manifest.durable-recovery")?,
                worker_invocation_id: fixture_id("invocation.worker.durable-recovery")?,
                broker_invocation_id: fixture_id("invocation.broker.durable-recovery")?,
                outcome_hash: format!("sha256:{}", "7".repeat(64)),
                status: WorkflowExecutionStatusV1::Succeeded,
                assistant_text: Some(completion.text),
                reasoning: None,
                error: None,
                model: completion.model,
                input_units: completion.input_units,
                output_units: completion.output_units,
                model_turns: 1,
                tool_calls: 0,
                tool_activity: Vec::new(),
                node_activity: Vec::new(),
                approval: None,
                replayed: false,
            };
            *self.settled.lock().unwrap() = Some(result.clone());
            if self
                .fail_after_first_settlement
                .swap(false, Ordering::SeqCst)
            {
                return Err("simulated crash after durable provider settlement".into());
            }
            Ok(result)
        }
    }

    struct PreflightRejectingPipelineV1;

    impl WorkflowPipelinePort for PreflightRejectingPipelineV1 {
        fn preflight(&self, _request: &WorkflowExecutionRequestV1) -> Result<(), String> {
            Err("deterministic pipeline preflight rejection".into())
        }

        fn execute(
            &self,
            _request: WorkflowExecutionRequestV1,
        ) -> Result<WorkflowExecutionResultV1, String> {
            panic!("execute must not run after a deterministic preflight rejection")
        }
    }

    fn fixture_id(value: &str) -> Result<StableId, String> {
        StableId::parse(value.to_owned()).map_err(|error| error.to_string())
    }

    #[derive(Default)]
    struct RecordingCredentialStore {
        inner: MemoryCredentialStore,
        puts: Mutex<Vec<String>>,
        deletes: Mutex<Vec<String>>,
        live: Mutex<BTreeSet<String>>,
        fail_delete: AtomicBool,
    }

    impl PlatformCredentialStorePort for RecordingCredentialStore {
        fn put(
            &self,
            credential: &CredentialRef,
            secret: CredentialSecretV1,
        ) -> Result<(), SecretError> {
            let reference = credential.0.to_string();
            self.puts.lock().unwrap().push(reference.clone());
            self.inner.put(credential, secret)?;
            self.live.lock().unwrap().insert(reference);
            Ok(())
        }

        fn retrieve_for_lease(
            &self,
            credential: &CredentialRef,
            authorization: &CredentialReadAuthorizationV1,
        ) -> Result<CredentialSecretV1, SecretError> {
            self.inner.retrieve_for_lease(credential, authorization)
        }

        fn delete(&self, credential: &CredentialRef) -> Result<(), SecretError> {
            self.deletes.lock().unwrap().push(credential.0.to_string());
            if self.fail_delete.load(Ordering::SeqCst) {
                Err(SecretError::StoreUnavailable)
            } else {
                self.inner.delete(credential)?;
                self.live.lock().unwrap().remove(credential.0.as_str());
                Ok(())
            }
        }
    }

    fn runtime(root: &TempDir, provider: Arc<FixtureProvider>) -> DesktopRuntime {
        let store = Arc::new(MemoryCredentialStore::default());
        runtime_with_store(root, provider, store)
    }

    fn runtime_with_store(
        root: &TempDir,
        provider: Arc<FixtureProvider>,
        store: Arc<dyn PlatformCredentialStorePort>,
    ) -> DesktopRuntime {
        let mut runtime = DesktopRuntime::open_with_credential_store(root.path(), store).unwrap();
        runtime.provider = provider.clone();
        runtime.pipeline = Arc::new(FixtureWorkflowPipeline { provider });
        runtime
    }

    fn assert_profile_excludes(root: &Path, forbidden: &str) {
        let mut pending = vec![root.to_path_buf()];
        while let Some(path) = pending.pop() {
            for entry in fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    let bytes = fs::read(path).unwrap();
                    assert!(
                        !bytes
                            .windows(forbidden.len())
                            .any(|window| window == forbidden.as_bytes()),
                        "profile file persisted write-only credential material"
                    );
                }
            }
        }
    }

    fn configure(runtime: &mut DesktopRuntime) {
        runtime
            .settings_commit(SettingsCommitInput {
                command_id: "settings.configure".into(),
                expected_version: 1,
                appearance: "system".into(),
                portable_history_enabled: false,
                provider: ProviderCommitInput {
                    base_url: "http://127.0.0.1:9999/v1".into(),
                    model: "fixture-model".into(),
                    credential_action: "keep".into(),
                    api_key: None,
                },
            })
            .unwrap();
    }

    fn commit_provider(
        runtime: &mut DesktopRuntime,
        command_id: &str,
        expected_version: u64,
        base_url: &str,
        model: &str,
        credential_action: &str,
        api_key: Option<&str>,
    ) -> Result<UiCommandReceipt, String> {
        runtime.settings_commit(SettingsCommitInput {
            command_id: command_id.into(),
            expected_version,
            appearance: runtime.settings_snapshot().appearance,
            portable_history_enabled: runtime.settings_snapshot().portable_history_enabled,
            provider: ProviderCommitInput {
                base_url: base_url.into(),
                model: model.into(),
                credential_action: credential_action.into(),
                api_key: api_key.map(|value| value.to_owned().into()),
            },
        })
    }

    fn send(command_id: &str, expected_version: u64, text: &str) -> UiCommandInput {
        let payload = if expected_version == 0 {
            json!({
                "workflowId":"workflow.simple-chat",
                "input":text,
                "attachments":[],
            })
        } else {
            json!({"input":text})
        };
        UiCommandInput {
            schema_version: 1,
            command_id: command_id.into(),
            expected_version,
            action: if expected_version == 0 {
                "start"
            } else {
                "enqueue"
            }
            .into(),
            target_id: None,
            payload,
        }
    }

    fn saved_project(
        id: &str,
        name: &str,
        root: &Path,
        kind: WorkspaceKindV2,
    ) -> ProjectConfigurationV2 {
        ProjectConfigurationV2 {
            id: id.into(),
            name: name.into(),
            workspace: WorkspaceConfigurationV2 {
                kind,
                location: root.to_string_lossy().into_owned(),
            },
            default_workflow_id: Some("workflow.simple-chat".into()),
            portable_history_enabled: false,
        }
    }

    fn saved_provider_configuration(id: &str, base_url: &str) -> ProviderConfigurationV2 {
        ProviderConfigurationV2 {
            id: id.into(),
            name: format!("Provider {id}"),
            kind: "openai_compatible".into(),
            base_url: base_url.into(),
            enabled: true,
            credential_ref: None,
            models: vec![ModelConfigurationV2 {
                id: "model.fixture".into(),
                name: "Fixture model".into(),
                remote_id: "fixture-model".into(),
                enabled: true,
                context_window: None,
                max_output_tokens: None,
                capabilities: vec!["text".into()],
                parameters: BTreeMap::new(),
            }],
            configuration: BTreeMap::new(),
        }
    }

    fn configure_project_read_workflow(
        runtime: &mut DesktopRuntime,
        workspace: Option<&Path>,
        enabled: bool,
    ) {
        configure(runtime);
        let mut settings = runtime.settings_v2_snapshot().settings;
        let model = settings
            .providers
            .iter_mut()
            .flat_map(|provider| provider.models.iter_mut())
            .find(|model| model.remote_id == "fixture-model")
            .expect("fixture model");
        if !model
            .capabilities
            .iter()
            .any(|capability| capability == "tools")
        {
            model.capabilities.push("tools".into());
        }
        settings
            .tools
            .iter_mut()
            .find(|tool| tool.id == "tool.files.read")
            .expect("project read tool")
            .enabled = enabled;
        if let Some(workspace) = workspace {
            settings.projects.push(saved_project(
                "project.tool-test",
                "Tool Test",
                workspace,
                WorkspaceKindV2::LocalDirectory,
            ));
        }
        runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.tool-test".into(),
                expected_version: runtime.settings_v2_snapshot().version,
                settings,
            })
            .expect("tool Settings");

        let mut workflow = runtime.workflow_snapshot_for("workflow.simple-chat".into());
        workflow.document["nodes"][1]["configuration"]["toolIds"] = json!(["tool.files.read"]);
        runtime
            .workflow_commit(WorkflowCommitInput {
                command_id: "workflow.tool-test".into(),
                expected_version: workflow.version,
                document: workflow.document,
                workflow_id: Some("workflow.simple-chat".into()),
            })
            .expect("tool workflow");
    }

    fn project_tool_start(command_id: &str, text: &str) -> UiCommandInput {
        let mut command = send(command_id, 0, text);
        command.payload["projectId"] = Value::String("project.tool-test".into());
        command
    }

    #[test]
    fn standard_agent_start_stages_and_exposes_the_list_files_binding() {
        let root = TempDir::new().unwrap();
        let workspace = root.path().join("standard-agent-project");
        fs::create_dir(&workspace).unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider.clone());
        configure(&mut runtime);

        let mut settings = runtime.settings_v2_snapshot().settings;
        let model = settings
            .providers
            .iter_mut()
            .flat_map(|provider| provider.models.iter_mut())
            .find(|model| model.remote_id == "fixture-model")
            .expect("fixture model");
        model.capabilities = vec!["text".into()];
        model
            .parameters
            .insert("enableThinking".into(), Value::Bool(true));
        assert_eq!(model.capabilities, vec!["text"]);
        for tool in &mut settings.tools {
            if matches!(
                tool.id.as_str(),
                "tool.files.read"
                    | "tool.files.search"
                    | "tool.files.list"
                    | "tool.files.grep"
                    | "tool.todo"
                    | "tool.web_search"
                    | "tool.web_fetch"
                    | "tool.web_extract"
            ) {
                tool.enabled = true;
            }
        }
        settings.projects.push(ProjectConfigurationV2 {
            id: "project.standard-agent".into(),
            name: "Standard Agent Project".into(),
            workspace: WorkspaceConfigurationV2 {
                kind: WorkspaceKindV2::LocalDirectory,
                location: workspace.to_string_lossy().into_owned(),
            },
            default_workflow_id: Some("workflow.standard-agent".into()),
            portable_history_enabled: false,
        });
        runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.standard-agent".into(),
                expected_version: runtime.settings_v2_snapshot().version,
                settings,
            })
            .unwrap();

        let receipt = runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.standard-agent-start".into(),
                expected_version: 0,
                action: "start".into(),
                target_id: None,
                payload: json!({
                    "workflowId": "workflow.standard-agent",
                    "projectId": "project.standard-agent",
                    "input": "Can you see my project?",
                    "attachments": [],
                }),
            })
            .expect("Standard Agent start must pass durable command staging");

        assert_eq!(receipt.current_version, 6);
        assert!(!runtime.snapshot(0).unwrap().chat.recovery_pending);
        let requests = provider.execution_requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].deadline_epoch_millis,
            crate::runtime::pipeline::NO_AGGREGATE_RUN_DEADLINE_EPOCH_MILLIS
        );
        assert_eq!(
            requests[0].budget.deadline_ms,
            crate::runtime::pipeline::NO_AGGREGATE_RUN_DEADLINE_EPOCH_MILLIS
        );
        assert_eq!(
            requests[0].model_parameters.get("enableThinking"),
            Some(&Value::Bool(true))
        );
        assert!(
            requests[0]
                .tools
                .iter()
                .any(|tool| tool.capability_id == "tool.files.list")
        );
    }

    #[test]
    fn subagent_freeze_includes_enabled_child_tools_without_adding_parent_tool_ids() {
        let mut settings = SettingsConfigurationV2::default();
        for tool_id in [SUBAGENT_CAPABILITY_ID, "tool.files.read", "tool.todo"] {
            settings
                .tools
                .iter_mut()
                .find(|tool| tool.id == tool_id)
                .expect("built-in tool")
                .enabled = true;
        }
        let mut workflow = crate::runtime::documents::bundled_workflow_template("simple-chat")
            .expect("bundled workflow");
        workflow["nodes"][1]["configuration"]["toolIds"] = json!([SUBAGENT_CAPABILITY_ID]);

        let frozen = freeze_graph_bindings(&workflow, &settings, true, &BTreeMap::new())
            .expect("subagent freeze");
        assert_eq!(
            frozen
                .tools
                .iter()
                .map(|tool| tool.tool_id.as_str())
                .collect::<Vec<_>>(),
            vec![SUBAGENT_CAPABILITY_ID, "tool.files.read", "tool.todo"]
        );
        assert_eq!(
            workflow["nodes"][1]["configuration"]["toolIds"],
            json!([SUBAGENT_CAPABILITY_ID]),
            "child bindings belong to subagent semantics, not the parent JSON tool list"
        );
    }

    #[test]
    fn simple_chat_commits_real_assistant_output_and_replays_without_second_effect() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider.clone());
        configure(&mut runtime);
        let command = send("chat.first", 0, "hello");
        let first = runtime.command(command.clone()).unwrap();
        let replay = runtime.command(command).unwrap();
        assert_eq!(first.current_version, 6);
        assert_eq!(replay.current_version, 6);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        let snapshot = runtime.snapshot(0).unwrap();
        assert_eq!(snapshot.chat.phase, "waiting_input");
        let run_start = snapshot
            .events
            .iter()
            .find(|event| {
                event.kind == "span.started"
                    && event.payload.get("spanKind").and_then(Value::as_str) == Some("run")
            })
            .expect("run start");
        assert!(
            snapshot.events.iter().any(|event| {
                event.kind == "span.completed" && event.span_id == run_start.span_id
            })
        );
        assert_eq!(
            snapshot
                .events
                .iter()
                .rev()
                .find(|event| event.kind == "message.assistant")
                .and_then(|event| event.payload.get("body"))
                .and_then(Value::as_str),
            Some("fixture: hello")
        );
    }

    #[test]
    fn oversized_legacy_workflow_is_rejected_before_freeze_stage_or_provider_effect() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let mut initial = runtime(&root, provider.clone());
        configure(&mut initial);
        drop(initial);

        let repository = RepositoryRoot::open(root.path().join("documents")).unwrap();
        let mut oversized =
            super::super::documents::bundled_workflow_template("simple-chat").unwrap();
        oversized["preservedMetadata"] =
            Value::String("x".repeat(super::super::pipeline::MAXIMUM_WORKFLOW_SNAPSHOT_BYTES));
        let encoded = JsonDocument::parse(serde_json::to_vec(&oversized).unwrap()).unwrap();
        repository
            .save(
                DocumentKind::Workflow,
                "workflow.simple-chat",
                Some(1),
                &encoded,
            )
            .unwrap();

        let mut reopened = runtime(&root, provider.clone());
        let error = reopened
            .command(send("chat.oversized-workflow", 0, "must not stage"))
            .unwrap_err();
        assert!(error.contains("executable 128 KiB persistence bound"));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert!(provider.execution_requests.lock().unwrap().is_empty());
        assert_eq!(reopened.history.head().unwrap(), 0);
        assert!(
            reopened
                .history
                .pending_context_at_head(0)
                .unwrap()
                .is_none()
        );
        assert!(
            reopened
                .history
                .pending_effect_command_at_head(0)
                .unwrap()
                .is_none()
        );
        let snapshot = reopened.snapshot(0).unwrap();
        assert_eq!(snapshot.chat.phase, "draft");
        assert!(!snapshot.chat.recovery_pending);
    }

    #[test]
    fn deterministic_pipeline_rejection_occurs_before_start_freeze_or_effect_stage() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider.clone());
        configure(&mut runtime);
        runtime.pipeline = Arc::new(PreflightRejectingPipelineV1);

        let error = runtime
            .command(send("chat.preflight-rejected", 0, "must remain a draft"))
            .unwrap_err();
        assert_eq!(error, "deterministic pipeline preflight rejection");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(runtime.history.head().unwrap(), 0);
        assert!(
            runtime
                .history
                .pending_context_at_head(0)
                .unwrap()
                .is_none()
        );
        assert!(
            runtime
                .history
                .pending_effect_command_at_head(0)
                .unwrap()
                .is_none()
        );
        let snapshot = runtime.snapshot(0).unwrap();
        assert_eq!(snapshot.chat.phase, "draft");
        assert!(!snapshot.chat.locked_workflow);
        assert!(!snapshot.chat.recovery_pending);
    }

    #[test]
    fn project_file_tools_fail_before_provider_when_disabled_or_unscoped() {
        let unscoped_root = TempDir::new().unwrap();
        let unscoped_provider = Arc::new(FixtureProvider::new());
        let mut unscoped = runtime(&unscoped_root, unscoped_provider.clone());
        configure_project_read_workflow(&mut unscoped, None, true);
        let error = unscoped
            .command(send("chat.tool.unscoped", 0, "read notes.txt"))
            .unwrap_err();
        assert!(
            error.contains("requires selecting a saved project"),
            "{error}"
        );
        assert_eq!(unscoped_provider.calls.load(Ordering::SeqCst), 0);
        assert!(
            unscoped_provider
                .execution_requests
                .lock()
                .unwrap()
                .is_empty()
        );
        assert_eq!(unscoped.snapshot(0).unwrap().version, 0);

        let disabled_root = TempDir::new().unwrap();
        let workspace = TempDir::new().unwrap();
        let disabled_provider = Arc::new(FixtureProvider::new());
        let mut disabled = runtime(&disabled_root, disabled_provider.clone());
        configure_project_read_workflow(&mut disabled, Some(workspace.path()), false);
        let error = disabled
            .command(project_tool_start("chat.tool.disabled", "read notes.txt"))
            .unwrap_err();
        assert!(error.contains("disabled in saved Settings"), "{error}");
        assert_eq!(disabled_provider.calls.load(Ordering::SeqCst), 0);
        assert!(
            disabled_provider
                .execution_requests
                .lock()
                .unwrap()
                .is_empty()
        );
        assert_eq!(disabled.snapshot(0).unwrap().version, 0);
    }

    #[test]
    fn project_file_tool_settings_and_workspace_are_frozen_for_follow_ups() {
        let root = TempDir::new().unwrap();
        let workspace = TempDir::new().unwrap();
        fs::write(workspace.path().join("notes.txt"), "initial").unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider.clone());
        configure_project_read_workflow(&mut runtime, Some(workspace.path()), true);

        runtime
            .command(project_tool_start(
                "chat.tool.freeze-start",
                "read notes.txt",
            ))
            .unwrap();
        let frozen = runtime.history.current_frozen_context().unwrap().unwrap();
        assert_eq!(frozen.context.legacy_agent_maximum_turns, None);
        assert_eq!(frozen.context.legacy_maximum_tool_calls, None);
        assert_eq!(frozen.context.run_deadline_millis, 120_000);
        assert_eq!(frozen.context.tools.len(), 1);
        assert_eq!(frozen.context.tools[0].tool_id, "tool.files.read");
        assert!(frozen.context.tools[0].tool_hash.starts_with("sha256:"));

        let mut changed = runtime.settings_v2_snapshot().settings;
        let read = changed
            .tools
            .iter_mut()
            .find(|tool| tool.id == "tool.files.read")
            .unwrap();
        read.enabled = false;
        read.configuration
            .insert("maximumBytes".into(), Value::from(64_u64));
        runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.tool-test.future".into(),
                expected_version: runtime.settings_v2_snapshot().version,
                settings: changed,
            })
            .unwrap();
        runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.tool.freeze-follow-up".into(),
                expected_version: 6,
                action: "enqueue".into(),
                target_id: None,
                payload: json!({"input":"read it again"}),
            })
            .unwrap();

        let requests = provider.execution_requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].workspace, requests[1].workspace);
        assert_eq!(requests[0].tools, requests[1].tools);
        assert_eq!(requests[0].maximum_timeout_recoveries, 1);
        assert_eq!(requests[0].budget.turns, 1);
        assert_eq!(requests[0].provider.request_timeout_seconds, 300);
        assert_eq!(requests[0].provider.maximum_tool_output_bytes, 65_536);
        assert_eq!(requests[0].budget.tool_calls, 0);
        assert_eq!(
            requests[0].budget.deadline_ms,
            crate::runtime::pipeline::NO_AGGREGATE_RUN_DEADLINE_EPOCH_MILLIS
        );
        assert_eq!(
            requests[0].deadline_epoch_millis,
            crate::runtime::pipeline::NO_AGGREGATE_RUN_DEADLINE_EPOCH_MILLIS
        );
        assert_eq!(
            requests[1].tools[0].configuration["maximumBytes"],
            Value::from(crate::runtime::PROJECT_FILE_READ_MAXIMUM_BYTES_V1)
        );
        assert_eq!(
            requests[1].workspace.as_ref().unwrap().root,
            fs::canonicalize(workspace.path()).unwrap()
        );
        assert_eq!(
            requests[0].workflow_snapshot["nodes"][1]["configuration"]["toolIds"],
            json!(["tool.files.read"])
        );
    }

    #[test]
    fn frozen_start_projects_explicit_restart_recovery_and_changed_input_is_denied() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider.clone());
        configure(&mut runtime);
        let command = send("chat.pending-freeze", 0, "original input");
        let fingerprint = command_fingerprint(&command).unwrap();
        let frozen = runtime
            .freeze_workflow_context(&command, &command.command_id, &fingerprint, 0, None)
            .unwrap();
        let interrupted = runtime.snapshot(0).unwrap();
        assert_eq!(interrupted.version, 0);
        assert_eq!(interrupted.chat.phase, "paused");
        assert!(interrupted.chat.recovery_pending);
        commit_provider(
            &mut runtime,
            "settings.after-pending-freeze",
            2,
            "http://127.0.0.1:9999/v1",
            "future-model",
            "keep",
            None,
        )
        .unwrap();
        drop(runtime);

        let mut reopened = self::runtime(&root, provider.clone());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert!(reopened.snapshot(0).unwrap().chat.recovery_pending);
        let mut changed = command.clone();
        changed.payload["input"] = Value::String("changed input".into());
        let error = reopened.command(changed).unwrap_err();
        assert!(error.contains("interrupted effect-bearing"));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);

        let first = reopened
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.resume-pending-freeze".into(),
                expected_version: 0,
                action: "resume".into(),
                target_id: None,
                payload: json!({}),
            })
            .unwrap();
        let replay = reopened.command(command).unwrap();
        assert_eq!(first.current_version, replay.current_version);
        assert_eq!(first.command_id, "chat.resume-pending-freeze");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert!(!reopened.snapshot(0).unwrap().chat.recovery_pending);
        let requests = provider.execution_requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].provider.model, "fixture-model");
        assert_eq!(requests[0].frozen_context_hash, frozen.context_hash);
    }

    #[test]
    fn settled_provider_outcome_is_recovered_after_restart_without_a_second_effect() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let durable_pipeline = Arc::new(DurableRecoveryPipeline::new(provider.clone()));
        let mut runtime = runtime(&root, provider.clone());
        configure(&mut runtime);
        runtime.pipeline = durable_pipeline.clone();
        let error = runtime
            .command(send("chat.settled-before-history", 0, "recover me"))
            .unwrap_err();
        assert!(error.contains("simulated crash"));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert!(runtime.snapshot(0).unwrap().chat.recovery_pending);
        drop(runtime);

        let mut reopened = self::runtime(&root, provider.clone());
        reopened.pipeline = durable_pipeline;
        reopened
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.resume-settled-outcome".into(),
                expected_version: 4,
                action: "resume".into(),
                target_id: None,
                payload: json!({}),
            })
            .unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        let recovered = reopened.snapshot(0).unwrap();
        assert_eq!(recovered.chat.phase, "waiting_input");
        assert!(!recovered.chat.recovery_pending);
        let assistant = recovered
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "message.assistant")
            .expect("recovered assistant event");
        assert_eq!(assistant.payload["body"], "fixture: recover me");
        assert_eq!(assistant.payload["replayed"], true);
    }

    #[test]
    fn staged_follow_up_survives_restart_and_cannot_be_orphaned_by_a_new_chat() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider.clone());
        configure(&mut runtime);
        runtime
            .command(send("chat.recovery-start", 0, "first"))
            .unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

        let pending_command = send("chat.recovery-follow-up", 6, "second");
        let command_hash = command_fingerprint(&pending_command).unwrap();
        let frozen = runtime.history.current_frozen_context().unwrap().unwrap();
        runtime
            .history
            .stage_effect_command(PendingChatCommandV1 {
                schema_version: 1,
                frozen_context_hash: frozen.context_hash,
                command_hash,
                command: pending_command.clone(),
            })
            .unwrap();
        drop(runtime);

        let mut reopened = self::runtime(&root, provider.clone());
        let projection = reopened.snapshot(0).unwrap();
        assert_eq!(projection.chat.phase, "paused");
        assert!(projection.chat.recovery_pending);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        let new_chat_error = reopened
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.recovery-unsafe-new".into(),
                expected_version: 6,
                action: "new_chat".into(),
                target_id: None,
                payload: json!({}),
            })
            .unwrap_err();
        assert!(new_chat_error.contains("interrupted effect-bearing"));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

        reopened
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.resume-follow-up".into(),
                expected_version: 6,
                action: "resume".into(),
                target_id: None,
                payload: json!({}),
            })
            .unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        assert!(!reopened.snapshot(0).unwrap().chat.recovery_pending);
        let conversations = provider.conversations.lock().unwrap();
        let recovered = conversations.last().unwrap();
        assert_eq!(recovered.len(), 3);
        assert_eq!(recovered[2].role, "user");
        assert_eq!(recovered[2].content, "second");
    }

    #[test]
    fn pending_follow_up_can_be_abandoned_as_uncertain_without_replaying_an_effect() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider.clone());
        configure(&mut runtime);
        runtime
            .command(send("chat.abandon-start", 0, "first"))
            .unwrap();
        let pending_command = send("chat.abandon-follow-up", 6, "possibly sent");
        let frozen = runtime.history.current_frozen_context().unwrap().unwrap();
        runtime
            .history
            .stage_effect_command(PendingChatCommandV1 {
                schema_version: 1,
                frozen_context_hash: frozen.context_hash,
                command_hash: command_fingerprint(&pending_command).unwrap(),
                command: pending_command,
            })
            .unwrap();
        drop(runtime);

        let mut reopened = self::runtime(&root, provider.clone());
        reopened
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.abandon-recovery".into(),
                expected_version: 6,
                action: "abandon_recovery".into(),
                target_id: None,
                payload: json!({}),
            })
            .unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        let failed = reopened.snapshot(0).unwrap();
        assert_eq!(failed.chat.phase, "failed");
        assert!(!failed.chat.recovery_pending);
        let failure = failed
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "execution.failed")
            .expect("recovery failure event");
        assert_eq!(failure.payload["recoveryAbandoned"], true);

        reopened
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.after-abandon-new".into(),
                expected_version: 8,
                action: "new_chat".into(),
                target_id: None,
                payload: json!({}),
            })
            .unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(reopened.snapshot(0).unwrap().chat.phase, "draft");
    }

    #[test]
    fn accumulated_context_is_rejected_before_staging_or_provider_effect() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider.clone());
        configure(&mut runtime);
        let first = "a".repeat(90 * 1024);
        runtime
            .command(send("chat.context-bound-start", 0, &first))
            .unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

        let oversized_follow_up = "b".repeat(80 * 1024);
        let error = runtime
            .command(send(
                "chat.context-bound-follow-up",
                6,
                &oversized_follow_up,
            ))
            .unwrap_err();
        assert!(error.contains("accumulated Chat message context"));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert!(!runtime.snapshot(0).unwrap().chat.recovery_pending);
        drop(runtime);

        let mut reopened = self::runtime(&root, provider.clone());
        assert!(!reopened.snapshot(0).unwrap().chat.recovery_pending);
        reopened
            .command(send("chat.context-bound-small", 6, "small follow-up"))
            .unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn chat_lifecycle_rejects_draft_stop_and_allows_input_after_a_stopped_turn() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider.clone());
        configure(&mut runtime);
        let draft_cancel = runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.cancel-draft".into(),
                expected_version: 0,
                action: "cancel".into(),
                target_id: None,
                payload: json!({}),
            })
            .unwrap_err();
        assert!(draft_cancel.contains("draft Chat"));

        runtime
            .command(send("chat.lifecycle.start", 0, "hello"))
            .unwrap();
        let selected_chat = runtime.snapshot(0).unwrap().chat.chat_id;
        let stale = runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.lifecycle.stale".into(),
                expected_version: 6,
                action: "enqueue".into(),
                target_id: Some("chat.stale-target".into()),
                payload: json!({"input":"must not run"}),
            })
            .unwrap_err();
        assert!(stale.contains("stale"));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

        let run_id = runtime.snapshot(0).unwrap().chat.run_id;
        runtime
            .history
            .append(
                "chat.lifecycle.open-span",
                "fixture-open-span",
                6,
                vec![(
                    "span.started",
                    json!({
                        "schemaVersion":1,
                        "requestId":"chat.lifecycle.turn",
                        "runId":run_id,
                        "spanId":"span.run.lifecycle-turn",
                        "parentSpanId":Value::Null,
                        "spanKind":"run",
                        "semanticRole":"run",
                        "status":"running",
                        "createdAt":now_label()
                    }),
                )],
            )
            .unwrap();

        runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.lifecycle.cancel".into(),
                expected_version: 7,
                action: "cancel".into(),
                target_id: Some(selected_chat),
                payload: json!({}),
            })
            .unwrap();
        let repeated = runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.lifecycle.cancel-again".into(),
                expected_version: 9,
                action: "cancel".into(),
                target_id: None,
                payload: json!({}),
            })
            .unwrap_err();
        assert!(repeated.contains("no running turn"));
        runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.lifecycle.after-cancel".into(),
                expected_version: 9,
                action: "enqueue".into(),
                target_id: None,
                payload: json!({"input":"continue after stop"}),
            })
            .unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        assert_eq!(runtime.snapshot(0).unwrap().chat.phase, "waiting_input");
    }

    #[test]
    fn out_of_band_stop_interrupts_the_active_command_and_keeps_chat_open() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider.clone());
        configure(&mut runtime);
        let controller = runtime.cancellation_controller();
        let started = Arc::new((Mutex::new(false), Condvar::new()));
        runtime.pipeline = Arc::new(StopAwareWorkflowPipeline {
            cancellation_controller: controller.clone(),
            started: started.clone(),
        });
        let chat_id = runtime.snapshot(0).unwrap().chat.chat_id;
        let runtime = Arc::new(Mutex::new(runtime));
        let command_runtime = runtime.clone();
        let command = thread::spawn(move || {
            command_runtime.lock().unwrap().command(send(
                "chat.stop.active-turn",
                0,
                "keep working",
            ))
        });

        let (started_lock, ready) = &*started;
        let (started_guard, wait) = ready
            .wait_timeout_while(
                started_lock.lock().unwrap(),
                Duration::from_secs(2),
                |started| !*started,
            )
            .unwrap();
        assert!(
            *started_guard && !wait.timed_out(),
            "workflow did not start"
        );
        assert!(
            controller
                .request_stop(&chat_id, "chat.stop.request")
                .expect("request stop")
        );
        command
            .join()
            .expect("command thread")
            .expect("settled stop");

        let mut runtime = runtime.lock().unwrap();
        let stopped = runtime.snapshot(0).unwrap();
        assert_eq!(stopped.chat.phase, "waiting_input");
        assert!(
            stopped
                .events
                .iter()
                .any(|event| event.kind == "chat.turn_stopped")
        );
        assert!(
            stopped
                .events
                .iter()
                .all(|event| event.kind != "execution.failed")
        );
        runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.stop.request".into(),
                expected_version: 0,
                action: "cancel".into(),
                target_id: Some(chat_id),
                payload: json!({}),
            })
            .expect("idempotent stop acknowledgement");
        runtime.pipeline = Arc::new(FixtureWorkflowPipeline {
            provider: provider.clone(),
        });
        let version = runtime.snapshot(0).unwrap().version;
        runtime
            .command(send("chat.after-stop", version, "continue here"))
            .expect("follow-up after stop");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.snapshot(0).unwrap().chat.phase, "waiting_input");
    }

    #[test]
    fn settings_workflow_and_conversation_survive_runtime_reopen() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let mut first = runtime(&root, provider.clone());
        configure(&mut first);
        first.command(send("chat.first", 0, "hello")).unwrap();
        drop(first);

        let mut reopened = runtime(&root, provider.clone());
        let settings = reopened.settings_snapshot();
        assert_eq!(settings.version, 2);
        assert_eq!(settings.provider.model, "fixture-model");
        assert_eq!(
            reopened
                .snapshot(0)
                .unwrap()
                .events
                .iter()
                .filter(|event| matches!(event.kind.as_str(), "message.user" | "message.assistant"))
                .count(),
            2
        );
        reopened.command(send("chat.second", 6, "again")).unwrap();
        let conversations = provider.conversations.lock().unwrap();
        let continued = conversations.last().unwrap();
        assert!(
            continued.iter().any(|message| {
                message.role == "assistant" && message.content == "fixture: hello"
            })
        );
    }

    #[test]
    fn chat_history_selects_pins_forks_deletes_and_reopens_independent_streams() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider.clone());
        configure(&mut runtime);

        runtime
            .command(send("chat.history.first", 0, "first topic"))
            .unwrap();
        let first = runtime.snapshot(0).unwrap();
        let first_id = first.chat.chat_id.clone();
        assert_eq!(first.history.len(), 1);
        assert_eq!(first.history[0].title, "first topic");

        runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.history.new".into(),
                expected_version: first.version,
                action: "new_chat".into(),
                target_id: Some(first_id.clone()),
                payload: json!({}),
            })
            .unwrap();
        assert_eq!(runtime.snapshot(0).unwrap().version, 0);
        runtime
            .command(send("chat.history.second", 0, "second topic"))
            .unwrap();
        let second = runtime.snapshot(0).unwrap();
        let second_id = second.chat.chat_id.clone();
        assert_ne!(first_id, second_id);
        assert_eq!(second.history.len(), 2);
        assert!(second.events.iter().any(|event| {
            event.kind == "message.user"
                && event.payload.get("body").and_then(Value::as_str) == Some("second topic")
        }));
        assert!(!second.events.iter().any(|event| {
            event.payload.get("body").and_then(Value::as_str) == Some("first topic")
        }));

        runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.history.pin".into(),
                expected_version: second.version,
                action: "set_chat_pinned".into(),
                target_id: Some(first_id.clone()),
                payload: json!({"pinned": true}),
            })
            .unwrap();
        assert!(
            runtime
                .snapshot(0)
                .unwrap()
                .history
                .iter()
                .find(|entry| entry.chat_id == first_id)
                .unwrap()
                .pinned
        );

        runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.history.select".into(),
                expected_version: second.version,
                action: "select_chat".into(),
                target_id: Some(first_id.clone()),
                payload: json!({}),
            })
            .unwrap();
        let selected = runtime.snapshot(0).unwrap();
        assert_eq!(selected.chat.chat_id, first_id);
        assert!(selected.events.iter().any(|event| {
            event.payload.get("body").and_then(Value::as_str) == Some("first topic")
        }));

        runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.history.fork".into(),
                expected_version: selected.version,
                action: "fork".into(),
                target_id: Some(first_id.clone()),
                payload: json!({}),
            })
            .unwrap();
        let fork = runtime.snapshot(0).unwrap();
        let fork_id = fork.chat.chat_id.clone();
        assert_ne!(fork_id, first_id);
        assert_eq!(fork.history.len(), 3);
        assert_eq!(
            fork.history
                .iter()
                .find(|entry| entry.chat_id == fork_id)
                .unwrap()
                .parent_chat_id
                .as_deref(),
            Some(first_id.as_str())
        );
        assert!(fork.events.iter().any(|event| {
            event.kind == "message.user"
                && event.payload.get("body").and_then(Value::as_str) == Some("first topic")
                && event.payload.get("parentChatId").and_then(Value::as_str)
                    == Some(first_id.as_str())
        }));

        runtime
            .command(send(
                "chat.history.fork.continue",
                fork.version,
                "fork follow-up",
            ))
            .unwrap();
        let continued_fork = runtime.snapshot(0).unwrap();
        assert_eq!(continued_fork.chat.chat_id, fork_id);
        assert!(continued_fork.events.iter().any(|event| {
            event.kind == "message.user"
                && event.payload.get("body").and_then(Value::as_str) == Some("fork follow-up")
        }));
        let conversations = provider.conversations.lock().unwrap();
        let continued_messages = conversations.last().unwrap();
        assert!(continued_messages.iter().any(|message| {
            message.role == "assistant" && message.content == "fixture: first topic"
        }));
        assert!(
            continued_messages
                .iter()
                .any(|message| { message.role == "user" && message.content == "fork follow-up" })
        );
        drop(conversations);

        runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.history.delete".into(),
                expected_version: continued_fork.version,
                action: "delete_chat".into(),
                target_id: Some(second_id.clone()),
                payload: json!({}),
            })
            .unwrap();
        let deleted = runtime.snapshot(0).unwrap();
        assert_eq!(deleted.chat.chat_id, fork_id);
        assert!(
            !deleted
                .history
                .iter()
                .any(|entry| entry.chat_id == second_id)
        );
        drop(runtime);

        let reopened =
            runtime_with_store(&root, provider, Arc::new(MemoryCredentialStore::default()));
        let restored = reopened.snapshot(0).unwrap();
        assert_eq!(restored.chat.chat_id, fork_id);
        assert_eq!(restored.history.len(), 2);
        assert!(restored.history.iter().any(|entry| entry.pinned));
    }

    #[test]
    fn deleting_the_selected_or_last_chat_always_leaves_a_valid_selection() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider.clone());
        let initial = runtime.snapshot(0).unwrap();
        let initial_id = initial.chat.chat_id.clone();

        runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.delete-selected.new".into(),
                expected_version: initial.version,
                action: "new_chat".into(),
                target_id: Some(initial_id.clone()),
                payload: json!({}),
            })
            .unwrap();
        let second = runtime.snapshot(0).unwrap();
        let second_id = second.chat.chat_id.clone();
        runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.delete-selected.second".into(),
                expected_version: second.version,
                action: "delete_chat".into(),
                target_id: Some(second_id.clone()),
                payload: json!({}),
            })
            .unwrap();
        let fallback = runtime.snapshot(0).unwrap();
        assert_eq!(fallback.chat.chat_id, initial_id);
        assert!(
            !fallback
                .history
                .iter()
                .any(|entry| entry.chat_id == second_id)
        );

        runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.delete-selected.last".into(),
                expected_version: fallback.version,
                action: "delete_chat".into(),
                target_id: Some(initial_id.clone()),
                payload: json!({}),
            })
            .unwrap();
        let replacement = runtime.snapshot(0).unwrap();
        assert_ne!(replacement.chat.chat_id, initial_id);
        assert_eq!(replacement.chat.phase, "draft");
        assert_eq!(replacement.history.len(), 1);

        drop(runtime);
        let reopened =
            runtime_with_store(&root, provider, Arc::new(MemoryCredentialStore::default()));
        assert_eq!(
            reopened.snapshot(0).unwrap().chat.chat_id,
            replacement.chat.chat_id
        );
    }

    #[test]
    fn provider_snapshot_is_honest_before_configuration() {
        let root = TempDir::new().unwrap();
        let runtime = runtime(&root, Arc::new(FixtureProvider::new()));
        assert_eq!(
            runtime.settings_snapshot().provider,
            ProviderSettingsSnapshot {
                base_url: String::new(),
                model: String::new(),
                credential_configured: false,
                state: "unconfigured".into(),
                detail: Some("Enter an OpenAI-compatible base URL and model, then test it.".into()),
            }
        );
    }

    #[test]
    fn draft_projection_exposes_exact_native_simple_chat_readiness() {
        let root = TempDir::new().unwrap();
        let mut runtime = runtime(&root, Arc::new(FixtureProvider::new()));
        runtime
            .documents
            .set_default_workflow("workflow.simple-chat")
            .unwrap();
        let blocked = runtime.snapshot(0).unwrap();
        assert!(
            blocked
                .chat
                .disabled_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("Unconfigured"))
        );

        configure(&mut runtime);
        assert_eq!(runtime.snapshot(0).unwrap().chat.disabled_reason, None);

        let mut settings = runtime.settings_v2_snapshot().settings;
        let exact_target = match settings.model_tiers[2].resolution.clone() {
            ModelTierResolutionV2::Exact { target } => target,
            _ => panic!("configure maps the balanced tier exactly"),
        };
        settings.model_tiers[2].resolution = ModelTierResolutionV2::Policy {
            candidates: vec![exact_target],
            preference: super::super::settings_v2::ModelPolicyPreferenceV2::Quality,
        };
        runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.readiness.policy".into(),
                expected_version: 2,
                settings,
            })
            .unwrap();
        let blocked = runtime.snapshot(0).unwrap();
        assert!(
            blocked
                .chat
                .disabled_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("executes only an Exact"))
        );
    }

    #[test]
    fn simple_chat_preflight_requires_text_capability_and_adapter_credential_field() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider.clone());
        runtime
            .documents
            .set_default_workflow("workflow.simple-chat")
            .unwrap();
        configure(&mut runtime);
        let mut settings = runtime.settings_v2_snapshot().settings;
        settings.providers[0].models[0].capabilities.clear();
        runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.no-text-capability".into(),
                expected_version: 2,
                settings,
            })
            .unwrap();
        let reason = runtime.snapshot(0).unwrap().chat.disabled_reason.unwrap();
        assert!(reason.contains("text capability"), "{reason}");
        let error = runtime
            .command(send("chat.no-text-capability", 0, "must not run"))
            .unwrap_err();
        assert!(error.contains("text capability"), "{error}");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert!(
            runtime
                .history
                .pending_context_at_head(0)
                .unwrap()
                .is_none()
        );

        let mut incompatible = runtime.documents.settings().clone();
        incompatible.providers[0].models[0].capabilities = vec!["text".into()];
        incompatible.providers[0].credential_ref = Some("credential.wrong-field".into());
        incompatible
            .credentials
            .push(CredentialMetadataConfigurationV2 {
                credential_ref: "credential.wrong-field".into(),
                label: "Wrong field".into(),
                kind: "api_key".into(),
                field_names: vec!["token".into()],
                revision: 1,
                bound_provider_id: Some(incompatible.providers[0].id.clone()),
                bound_endpoint: Some(incompatible.providers[0].base_url.clone()),
            });
        let error = resolve_workflow_model(&incompatible, "tier:balanced")
            .err()
            .expect("incompatible credential must fail preflight");
        assert!(error.contains("has no api_key field"), "{error}");
    }

    #[test]
    fn saved_agent_instructions_reach_the_exact_provider_request() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider.clone());
        configure(&mut runtime);
        let mut workflow = runtime.workflow_snapshot_for("workflow.simple-chat".into());
        workflow.document["nodes"][1]["configuration"]["instructions"] =
            Value::String("Answer in exactly one short sentence.".into());
        runtime
            .workflow_commit(WorkflowCommitInput {
                command_id: "workflow.agent-instructions".into(),
                expected_version: workflow.version,
                document: workflow.document,
                workflow_id: Some("workflow.simple-chat".into()),
            })
            .unwrap();
        runtime
            .command(send("chat.agent-instructions", 0, "hello"))
            .unwrap();
        let requests = provider.execution_requests.lock().unwrap();
        assert_eq!(requests[0].messages[0].role, "user");
        assert_eq!(
            requests[0].workflow_snapshot["nodes"][1]["configuration"]["instructions"],
            "Answer in exactly one short sentence."
        );
    }

    #[test]
    fn unsupported_agent_configuration_is_rejected_before_context_freeze() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider.clone());
        configure(&mut runtime);
        let mut workflow = runtime.workflow_snapshot_for("workflow.simple-chat".into());
        workflow.document["nodes"][1]["configuration"]["silentIgnoredField"] = Value::Bool(true);
        runtime
            .workflow_commit(WorkflowCommitInput {
                command_id: "workflow.unsupported-agent-field".into(),
                expected_version: workflow.version,
                document: workflow.document,
                workflow_id: Some("workflow.simple-chat".into()),
            })
            .unwrap();
        let error = runtime
            .command(send("chat.unsupported-agent-field", 0, "must not run"))
            .unwrap_err();
        assert!(error.contains("accepts exactly"), "{error}");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert!(
            runtime
                .history
                .pending_context_at_head(0)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn settings_v2_full_document_save_persists_every_section_and_is_idempotent() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider.clone());
        let mut settings = runtime.settings_v2_snapshot().settings;
        settings.providers.push(ProviderConfigurationV2 {
            id: "provider.catalog".into(),
            name: "Catalog provider".into(),
            kind: "openai_compatible".into(),
            base_url: "https://catalog.example/v1".into(),
            enabled: false,
            credential_ref: None,
            models: Vec::new(),
            configuration: BTreeMap::from([
                ("requestTimeoutSeconds".into(), Value::from(180)),
                ("maximumToolOutputBytes".into(), Value::from(32_768)),
            ]),
        });
        settings.model_tiers.push(ModelTierConfigurationV2 {
            id: "tier:private".into(),
            name: "Private".into(),
            kind: ModelTierKindV2::Custom,
            resolution: ModelTierResolutionV2::Unconfigured,
        });
        settings.tools[0].enabled = true;
        settings.extensions.push(ExtensionConfigurationV2 {
            id: "extension.discovered".into(),
            name: "Discovered extension".into(),
            version: "1.0.0".into(),
            status: ExtensionStatusV2::Discovered,
            enabled: false,
            trust_accepted: false,
            manifest_path: "/opt/extension/manifest.json".into(),
            entry_point: None,
            content_hash: None,
            compatibility: Some("aworkit >= 0.1".into()),
            provenance: Some("local discovery".into()),
            configuration: BTreeMap::new(),
        });
        settings.mcp_servers.push(McpServerConfigurationV2 {
            id: "mcp.catalog".into(),
            name: "Catalog MCP".into(),
            enabled: false,
            auto_connect: false,
            transport: IntegrationTransportV2::Http {
                url: "https://mcp.example/rpc".into(),
                headers: Vec::new(),
            },
        });
        settings.external_agents.push(ExternalAgentConfigurationV2 {
            id: "agent.codex".into(),
            name: "Codex".into(),
            adapter: "codex_app_server".into(),
            enabled: false,
            connection: IntegrationTransportV2::Stdio {
                command: "codex".into(),
                args: vec!["app-server".into()],
                cwd: None,
                env: Vec::new(),
            },
            credential_bindings: Vec::new(),
            mcp_server_ids: Vec::new(),
            // Handshake capabilities are exact ephemeral probe output. The
            // canonical settings document persists no unattested booleans.
            capabilities: ExternalAgentCapabilitiesV2::default(),
            configuration: BTreeMap::new(),
        });
        settings.projects.push(ProjectConfigurationV2 {
            id: "project.atlas".into(),
            name: "Atlas".into(),
            workspace: WorkspaceConfigurationV2 {
                kind: WorkspaceKindV2::GitWorktree,
                location: "/workspace/atlas".into(),
            },
            default_workflow_id: Some("workflow.simple-chat".into()),
            portable_history_enabled: false,
        });
        settings.appearance.font_scale = 1.25;
        let command = SettingsV2CommitInput {
            command_id: "settings.v2.complete".into(),
            expected_version: 1,
            settings,
        };
        let first = runtime.settings_v2_commit(command.clone()).unwrap();
        let replay = runtime.settings_v2_commit(command).unwrap();
        assert_eq!(first.current_version, 2);
        assert_eq!(replay.current_version, 2);
        assert_eq!(runtime.settings_v2_snapshot().settings.providers.len(), 1);
        drop(runtime);

        let reopened = self::runtime(&root, provider);
        let settings = reopened.settings_v2_snapshot();
        assert_eq!(settings.version, 2);
        assert_eq!(settings.schema_version, SETTINGS_SCHEMA_VERSION_V2);
        assert_eq!(settings.settings.tools.len(), 13);
        assert!(
            settings
                .settings
                .tools
                .iter()
                .any(|tool| tool.id == "tool.files.read" && tool.enabled)
        );
        assert_eq!(settings.settings.extensions.len(), 1);
        assert_eq!(settings.settings.mcp_servers.len(), 1);
        assert_eq!(settings.settings.external_agents.len(), 1);
        assert_eq!(settings.settings.projects.len(), 1);
        assert_eq!(settings.settings.model_tiers.len(), 5);
        assert_eq!(settings.settings.appearance.font_scale, 1.25);
    }

    #[test]
    fn generic_settings_cannot_enable_unavailable_executors_but_can_clear_legacy_state() {
        let root = TempDir::new().unwrap();
        let mut runtime = runtime(&root, Arc::new(FixtureProvider::new()));

        // Every built-in tool now has an installed v1 executor, so generic
        // Settings may enable any of them.
        for tool_id in [
            "tool.files.read",
            "tool.files.search",
            "tool.files.list",
            "tool.files.grep",
            "tool.files.edit",
            "tool.files.write",
            "tool.shell.host",
            "tool.python.host",
            "tool.todo",
            "tool.web_search",
            "tool.web_fetch",
            "tool.web_extract",
            "tool.subagent",
        ] {
            let mut attempted = runtime.settings_v2_snapshot().settings;
            attempted
                .tools
                .iter_mut()
                .find(|tool| tool.id == tool_id)
                .expect("built-in tool")
                .enabled = true;
            runtime
                .settings_v2_commit(SettingsV2CommitInput {
                    command_id: format!("settings.available.enable.{tool_id}"),
                    expected_version: runtime.settings_v2_snapshot().version,
                    settings: attempted,
                })
                .unwrap_or_else(|error| panic!("enabling {tool_id} failed: {error}"));
        }

        let mut attempted_mcp = runtime.settings_v2_snapshot().settings;
        attempted_mcp.mcp_servers.push(McpServerConfigurationV2 {
            id: "mcp.unavailable".into(),
            name: "Unavailable MCP".into(),
            enabled: true,
            auto_connect: false,
            transport: IntegrationTransportV2::Http {
                url: "https://mcp.example/rpc".into(),
                headers: Vec::new(),
            },
        });
        let mcp_version = runtime.settings_v2_snapshot().version;
        let error = runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.unavailable.enable-mcp".into(),
                expected_version: mcp_version,
                settings: attempted_mcp,
            })
            .unwrap_err();
        assert!(error.contains("MCP server 'mcp.unavailable' cannot be enabled"));
        assert_eq!(runtime.settings_v2_snapshot().version, mcp_version);

        let mut attempted_agent = runtime.settings_v2_snapshot().settings;
        attempted_agent
            .external_agents
            .push(ExternalAgentConfigurationV2 {
                id: "agent.unavailable".into(),
                name: "Unavailable agent".into(),
                adapter: "codex_app_server".into(),
                enabled: true,
                connection: IntegrationTransportV2::Stdio {
                    command: "codex".into(),
                    args: vec!["app-server".into()],
                    cwd: None,
                    env: Vec::new(),
                },
                credential_bindings: Vec::new(),
                mcp_server_ids: Vec::new(),
                capabilities: ExternalAgentCapabilitiesV2::default(),
                configuration: BTreeMap::new(),
            });
        let agent_version = runtime.settings_v2_snapshot().version;
        let error = runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.unavailable.enable-agent".into(),
                expected_version: agent_version,
                settings: attempted_agent,
            })
            .unwrap_err();
        assert!(error.contains("external agent 'agent.unavailable' cannot be enabled"));
        assert_eq!(runtime.settings_v2_snapshot().version, agent_version);

        let mut supported = runtime.settings_v2_snapshot().settings;
        for tool_id in ["tool.files.read", "tool.files.search"] {
            supported
                .tools
                .iter_mut()
                .find(|tool| tool.id == tool_id)
                .expect("built-in tool")
                .enabled = true;
        }
        let supported_version = runtime.settings_v2_snapshot().version;
        let supported_receipt = runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.available.enable-read-only-tools".into(),
                expected_version: supported_version,
                settings: supported,
            })
            .expect("implemented read-only tools may be enabled");
        assert_eq!(supported_receipt.current_version, supported_version + 1);

        // Simulate a profile written by an older build. Generic Settings must
        // round-trip this truth without manufacturing another enable action,
        // and the user must retain a path to turn every unavailable executor
        // off.
        let mut legacy_enabled = runtime.settings_v2_snapshot().settings;
        legacy_enabled.mcp_servers.push(McpServerConfigurationV2 {
            id: "mcp.legacy".into(),
            name: "Legacy MCP".into(),
            enabled: true,
            auto_connect: false,
            transport: IntegrationTransportV2::Http {
                url: "https://legacy-mcp.example/rpc".into(),
                headers: Vec::new(),
            },
        });
        legacy_enabled
            .external_agents
            .push(ExternalAgentConfigurationV2 {
                id: "agent.legacy".into(),
                name: "Legacy agent".into(),
                adapter: "codex_app_server".into(),
                enabled: true,
                connection: IntegrationTransportV2::Stdio {
                    command: "codex".into(),
                    args: vec!["app-server".into()],
                    cwd: None,
                    env: Vec::new(),
                },
                credential_bindings: Vec::new(),
                mcp_server_ids: Vec::new(),
                capabilities: ExternalAgentCapabilitiesV2::default(),
                configuration: BTreeMap::new(),
            });
        for tool_id in ["tool.files.edit", "tool.shell.host", "tool.python.host"] {
            legacy_enabled
                .tools
                .iter_mut()
                .find(|tool| tool.id == tool_id)
                .expect("built-in tool")
                .enabled = true;
        }
        let legacy_version = runtime.settings_v2_snapshot().version;
        assert_eq!(
            runtime
                .documents
                .save_settings(legacy_version, legacy_enabled)
                .expect("seed preexisting legacy enabled metadata"),
            legacy_version + 1
        );

        let mut preserved = runtime.settings_v2_snapshot().settings;
        preserved.mcp_servers[0].name = "Renamed legacy MCP".into();
        preserved.external_agents[0].name = "Renamed legacy agent".into();
        let preserved_receipt = runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.unavailable.preserve-legacy".into(),
                expected_version: legacy_version + 1,
                settings: preserved,
            })
            .expect("preexisting enabled metadata remains lossless");
        assert_eq!(preserved_receipt.current_version, legacy_version + 2);

        let mut disabled = runtime.settings_v2_snapshot().settings;
        disabled.mcp_servers[0].enabled = false;
        disabled.external_agents[0].enabled = false;
        for tool_id in ["tool.files.edit", "tool.shell.host", "tool.python.host"] {
            disabled
                .tools
                .iter_mut()
                .find(|tool| tool.id == tool_id)
                .expect("built-in tool")
                .enabled = false;
        }
        let disabled_receipt = runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.unavailable.disable-legacy".into(),
                expected_version: legacy_version + 2,
                settings: disabled,
            })
            .expect("preexisting enabled metadata may be cleared");
        assert_eq!(disabled_receipt.current_version, legacy_version + 3);
        let saved = runtime.settings_v2_snapshot().settings;
        assert!(!saved.mcp_servers[0].enabled);
        assert!(!saved.external_agents[0].enabled);
        assert!(saved.tools.iter().all(|tool| {
            !matches!(
                tool.id.as_str(),
                "tool.files.edit" | "tool.shell.host" | "tool.python.host"
            ) || !tool.enabled
        }));
    }

    #[test]
    fn settings_v2_validation_and_version_conflicts_leave_canonical_state_untouched() {
        let root = TempDir::new().unwrap();
        let mut runtime = runtime(&root, Arc::new(FixtureProvider::new()));
        let mut invalid = runtime.settings_v2_snapshot().settings;
        invalid.tools[0]
            .configuration
            .insert("apiKey".into(), Value::from("plaintext"));
        let error = runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.v2.invalid".into(),
                expected_version: 1,
                settings: invalid,
            })
            .unwrap_err();
        assert!(error.contains("secret-like field"));
        assert_eq!(runtime.settings_v2_snapshot().version, 1);

        let mut invented = runtime.settings_v2_snapshot().settings;
        invented
            .credentials
            .push(CredentialMetadataConfigurationV2 {
                credential_ref: "credential.invented".into(),
                label: "Invented".into(),
                kind: "api_key".into(),
                field_names: vec!["api_key".into()],
                revision: 1,
                bound_provider_id: None,
                bound_endpoint: None,
            });
        let error = runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.v2.invented-credential".into(),
                expected_version: 1,
                settings: invented,
            })
            .unwrap_err();
        assert!(error.contains("dedicated credential command"));
        assert_eq!(runtime.settings_v2_snapshot().version, 1);

        let unchanged = runtime.settings_v2_snapshot().settings;
        let stale = runtime.settings_v2_commit(SettingsV2CommitInput {
            command_id: "settings.v2.stale".into(),
            expected_version: 0,
            settings: unchanged,
        });
        assert!(stale.unwrap_err().contains("version conflict"));
        assert_eq!(runtime.settings_v2_snapshot().version, 1);
    }

    #[test]
    fn settings_v2_generic_save_cannot_change_or_delete_stored_credential_metadata() {
        let root = TempDir::new().unwrap();
        let mut runtime = runtime(&root, Arc::new(FixtureProvider::new()));
        commit_provider(
            &mut runtime,
            "settings.credential.seed",
            1,
            "https://provider.example/v1",
            "fixture-model",
            "replace",
            Some("secret"),
        )
        .unwrap();
        let mut renamed = runtime.settings_v2_snapshot().settings;
        renamed.credentials[0].label = "Changed through generic save".into();
        let error = runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.v2.rename-credential".into(),
                expected_version: 2,
                settings: renamed,
            })
            .unwrap_err();
        assert!(error.contains("dedicated credential command"));

        let mut removed = runtime.settings_v2_snapshot().settings;
        removed.providers[0].credential_ref = None;
        removed.credentials.clear();
        let error = runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.v2.delete-credential".into(),
                expected_version: 2,
                settings: removed,
            })
            .unwrap_err();
        assert!(error.contains("dedicated credential command"));
        assert_eq!(runtime.settings_v2_snapshot().version, 2);
        assert_eq!(runtime.settings_v2_snapshot().settings.credentials.len(), 1);
    }

    #[test]
    fn settings_v2_write_only_credential_lifecycle_rewires_bindings_and_never_projects_secrets() {
        let root = TempDir::new().unwrap();
        let store = Arc::new(RecordingCredentialStore::default());
        let mut runtime =
            runtime_with_store(&root, Arc::new(FixtureProvider::new()), store.clone());
        let mut settings = runtime.settings_v2_snapshot().settings;
        settings.providers.push(ProviderConfigurationV2 {
            id: "provider.secure".into(),
            name: "Secure provider".into(),
            kind: "openai_compatible".into(),
            base_url: "https://secure.example/v1".into(),
            enabled: false,
            credential_ref: None,
            models: Vec::new(),
            configuration: BTreeMap::new(),
        });
        runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.v2.provider".into(),
                expected_version: 1,
                settings,
            })
            .unwrap();

        let create_receipt = runtime
            .settings_v2_store_credential(CredentialStoreInputV2 {
                command_id: "settings.v2.credential.create".into(),
                expected_version: 2,
                replace_credential_ref: None,
                label: "Secure API key".into(),
                kind: "api_key".into(),
                bound_provider_id: Some("provider.secure".into()),
                bound_endpoint: Some("https://secure.example/v1".into()),
                fields: BTreeMap::from([("api_key".into(), "first-secret".to_owned().into())]),
            })
            .unwrap();
        let first_reference = runtime.settings_v2_snapshot().settings.credentials[0]
            .credential_ref
            .clone();
        assert_eq!(
            create_receipt.credential_mutation,
            Some(CredentialMutationOutcomeV2 {
                operation: CredentialMutationOperationV2::Create,
                previous_credential_ref: None,
                fresh_credential_ref: first_reference.clone(),
            })
        );
        let mut settings = runtime.settings_v2_snapshot().settings;
        settings.providers[0].credential_ref = Some(first_reference.clone());
        runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.v2.credential.bind".into(),
                expected_version: 3,
                settings,
            })
            .unwrap();

        let replace_receipt = runtime
            .settings_v2_store_credential(CredentialStoreInputV2 {
                command_id: "settings.v2.credential.replace".into(),
                expected_version: 4,
                replace_credential_ref: Some(first_reference.clone()),
                label: "Secure API key".into(),
                kind: "api_key".into(),
                bound_provider_id: Some("provider.secure".into()),
                bound_endpoint: Some("https://secure.example/v1".into()),
                fields: BTreeMap::from([("api_key".into(), "second-secret".to_owned().into())]),
            })
            .unwrap();
        let snapshot = runtime.settings_v2_snapshot();
        let replacement = snapshot.settings.credentials[0].credential_ref.clone();
        let expected_outcome = CredentialMutationOutcomeV2 {
            operation: CredentialMutationOperationV2::Replace,
            previous_credential_ref: Some(first_reference.clone()),
            fresh_credential_ref: replacement.clone(),
        };
        assert_eq!(
            replace_receipt.credential_mutation,
            Some(expected_outcome.clone())
        );
        let replayed_receipt = runtime
            .settings_v2_store_credential(CredentialStoreInputV2 {
                command_id: "settings.v2.credential.replace".into(),
                expected_version: 4,
                replace_credential_ref: Some(first_reference.clone()),
                label: "Secure API key".into(),
                kind: "api_key".into(),
                bound_provider_id: Some("provider.secure".into()),
                bound_endpoint: Some("https://secure.example/v1".into()),
                fields: BTreeMap::from([("api_key".into(), "second-secret".to_owned().into())]),
            })
            .unwrap();
        assert_eq!(
            replayed_receipt.current_version,
            replace_receipt.current_version
        );
        assert_eq!(replayed_receipt.credential_mutation, Some(expected_outcome));
        assert_ne!(replacement, first_reference);
        assert_eq!(
            snapshot.settings.providers[0].credential_ref.as_deref(),
            Some(replacement.as_str())
        );
        let projection = serde_json::to_string(&snapshot).unwrap();
        assert!(!projection.contains("first-secret"));
        assert!(!projection.contains("second-secret"));
        assert_eq!(store.puts.lock().unwrap().len(), 2);
        assert_eq!(store.deletes.lock().unwrap().len(), 1);

        let error = runtime
            .settings_v2_delete_credential(CredentialDeleteInputV2 {
                command_id: "settings.v2.credential.delete.bound".into(),
                expected_version: 5,
                credential_ref: replacement.clone(),
            })
            .unwrap_err();
        assert!(error.contains("still referenced"));
        let mut settings = runtime.settings_v2_snapshot().settings;
        settings.providers[0].credential_ref = None;
        runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.v2.credential.unbind".into(),
                expected_version: 5,
                settings,
            })
            .unwrap();
        let deleted = runtime
            .settings_v2_delete_credential(CredentialDeleteInputV2 {
                command_id: "settings.v2.credential.delete".into(),
                expected_version: 6,
                credential_ref: replacement,
            })
            .unwrap();
        assert_eq!(deleted.current_version, 7);
        assert!(
            runtime
                .settings_v2_snapshot()
                .settings
                .credentials
                .is_empty()
        );
        assert_eq!(store.deletes.lock().unwrap().len(), 2);
    }

    #[test]
    fn dedicated_credential_replacement_cannot_bypass_installed_provider_contract() {
        let root = TempDir::new().unwrap();
        let store = Arc::new(RecordingCredentialStore::default());
        let mut runtime =
            runtime_with_store(&root, Arc::new(FixtureProvider::new()), store.clone());
        commit_provider(
            &mut runtime,
            "settings.credential.contract.seed",
            1,
            "https://provider.example/v1",
            "fixture-model",
            "replace",
            Some("valid-secret"),
        )
        .unwrap();
        let snapshot = runtime.settings_v2_snapshot();
        let original = snapshot.settings.credentials[0].credential_ref.clone();
        let provider = snapshot.settings.providers[0].clone();

        let error = runtime
            .settings_v2_store_credential(CredentialStoreInputV2 {
                command_id: "settings.credential.contract.invalid-replacement".into(),
                expected_version: snapshot.version,
                replace_credential_ref: Some(original.clone()),
                label: "Missing API key".into(),
                kind: "api_key".into(),
                bound_provider_id: Some(provider.id),
                bound_endpoint: Some(provider.base_url),
                fields: BTreeMap::from([("token".into(), "must-be-cleaned-up".to_owned().into())]),
            })
            .unwrap_err();

        assert!(error.contains("has no api_key field required by the installed adapters"));
        let after = runtime.settings_v2_snapshot();
        assert_eq!(after.version, snapshot.version);
        assert_eq!(after.settings.credentials[0].credential_ref, original);
        assert_eq!(after.settings.providers[0].credential_ref, Some(original));
        assert_eq!(store.live.lock().unwrap().len(), 1);
        assert_profile_excludes(root.path(), "must-be-cleaned-up");
    }

    #[test]
    fn credential_reference_rewrite_invalidates_external_agent_capabilities() {
        let root = TempDir::new().unwrap();
        let runtime = runtime(&root, Arc::new(FixtureProvider::new()));
        let mut settings = runtime.settings_v2_snapshot().settings;
        settings.external_agents.push(ExternalAgentConfigurationV2 {
            id: "agent.legacy-health".into(),
            name: "Legacy negotiated agent".into(),
            adapter: "codex_app_server".into(),
            enabled: false,
            connection: IntegrationTransportV2::Stdio {
                command: "codex".into(),
                args: vec!["app-server".into()],
                cwd: None,
                env: vec![super::super::settings_v2::NamedCredentialBindingV2 {
                    name: "OPENAI_API_KEY".into(),
                    credential_ref: "credential.old".into(),
                    field: "api_key".into(),
                }],
            },
            credential_bindings: Vec::new(),
            mcp_server_ids: Vec::new(),
            capabilities: ExternalAgentCapabilitiesV2 {
                progress: true,
                continuation: true,
                cancellation: true,
                approvals: true,
            },
            configuration: BTreeMap::new(),
        });

        replace_credential_references(&mut settings, "credential.old", "credential.new");

        assert_eq!(
            settings.external_agents[0].capabilities,
            ExternalAgentCapabilitiesV2::default()
        );
        let IntegrationTransportV2::Stdio { env, .. } = &settings.external_agents[0].connection
        else {
            panic!("fixture must remain STDIO");
        };
        assert_eq!(env[0].credential_ref, "credential.new");
    }

    #[test]
    fn settings_v2_provider_probe_and_discovery_use_the_unsaved_exact_draft() {
        let root = TempDir::new().unwrap();
        let provider_port = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider_port.clone());
        let provider = ProviderConfigurationV2 {
            id: "provider.draft".into(),
            name: "Draft provider".into(),
            kind: "openai_compatible".into(),
            base_url: "https://draft.example/v1".into(),
            enabled: true,
            credential_ref: None,
            models: vec![ModelConfigurationV2 {
                id: "model.draft".into(),
                name: "Draft model".into(),
                remote_id: "fixture-model".into(),
                enabled: true,
                context_window: None,
                max_output_tokens: None,
                capabilities: vec!["text".into()],
                parameters: BTreeMap::new(),
            }],
            configuration: BTreeMap::new(),
        };
        let discovery = runtime
            .settings_v2_discover_models(ModelDiscoveryRequestV2 {
                provider: provider.clone(),
                replacement_credential: None,
                use_stored_credential: false,
                draft_fingerprint: "draft.discovery".into(),
            })
            .unwrap();
        assert_eq!(discovery.provider_id, "provider.draft");
        assert_eq!(discovery.draft_fingerprint, "draft.discovery");
        assert_eq!(discovery.models[0].remote_id, "fixture-model");
        assert_eq!(discovery.models[0].context_window, Some(32_768));

        let probe = runtime
            .settings_v2_test_provider(ProviderProbeRequestV2 {
                provider: provider.clone(),
                model_id: "model.draft".into(),
                replacement_credential: None,
                use_stored_credential: false,
                draft_fingerprint: "draft.probe".into(),
            })
            .unwrap();
        assert!(probe.ok);
        assert_eq!(probe.model_id.as_deref(), Some("model.draft"));
        assert_eq!(probe.remote_model_id.as_deref(), Some("fixture-model"));
        assert_eq!(probe.draft_fingerprint, "draft.probe");
        assert_eq!(provider_port.connection_tests.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.settings_v2_snapshot().version, 1);

        let mut unsupported_provider_configuration = provider.clone();
        unsupported_provider_configuration
            .configuration
            .insert("apiStyle".into(), Value::from("responses"));
        let error = runtime
            .settings_v2_discover_models(ModelDiscoveryRequestV2 {
                provider: unsupported_provider_configuration,
                replacement_credential: None,
                use_stored_credential: false,
                draft_fingerprint: "draft.unsupported-provider-configuration".into(),
            })
            .unwrap_err();
        assert!(error.contains("unsupported"));

        let mut supported_model_parameters = provider.clone();
        supported_model_parameters.models[0]
            .parameters
            .insert("reasoningEffort".into(), Value::from("xhigh"));
        runtime
            .settings_v2_test_provider(ProviderProbeRequestV2 {
                provider: supported_model_parameters,
                model_id: "model.draft".into(),
                replacement_credential: None,
                use_stored_credential: false,
                draft_fingerprint: "draft.supported-model-parameters".into(),
            })
            .expect("supported OpenAI-compatible reasoning parameters");

        let mut unsupported_model_parameters = provider;
        unsupported_model_parameters.models[0]
            .parameters
            .insert("temperature".into(), Value::from(0.5));
        let error = runtime
            .settings_v2_test_provider(ProviderProbeRequestV2 {
                provider: unsupported_model_parameters,
                model_id: "model.draft".into(),
                replacement_credential: None,
                use_stored_credential: false,
                draft_fingerprint: "draft.unsupported-model-parameters".into(),
            })
            .unwrap_err();
        assert!(error.contains("unsupported or invalid"));
        assert_eq!(provider_port.connection_tests.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn workflow_node_reasoning_overrides_follow_resolved_provider_metadata() {
        let mut provider =
            saved_provider_configuration("provider.reasoning", "https://example.com/v1");
        provider.models[0].capabilities.extend([
            "reasoning".into(),
            "reasoning_effort:low".into(),
            "reasoning_effort:high".into(),
            "thinking_toggle".into(),
        ]);
        let model = provider.models[0].clone();
        let workflow = json!({
            "nodes":[{
                "id":"agent.1",
                "type":"agent",
                "configuration":{
                    "modelTierId":"tier:balanced",
                    "toolIds":[],
                    "reasoningEffort":"high",
                    "enableThinking":false
                }
            }]
        });
        validate_workflow_model_parameters(&workflow, &provider, &model).unwrap();

        let mut unsupported_effort = workflow.clone();
        unsupported_effort["nodes"][0]["configuration"]["reasoningEffort"] = json!("max");
        assert!(
            validate_workflow_model_parameters(&unsupported_effort, &provider, &model)
                .unwrap_err()
                .contains("advertised only")
        );

        provider.kind = "anthropic".into();
        assert!(
            validate_workflow_model_parameters(&workflow, &provider, &model)
                .unwrap_err()
                .contains("OpenAI-compatible")
        );
    }

    #[test]
    fn provider_b_health_never_badges_legacy_selected_provider_a() {
        let root = TempDir::new().unwrap();
        let provider_port = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider_port);
        let provider_a =
            saved_provider_configuration("provider.a", "https://provider-a.example/v1");
        let provider_b =
            saved_provider_configuration("provider.b", "https://provider-b.example/v1");
        let mut settings = runtime.settings_v2_snapshot().settings;
        settings.providers = vec![provider_a, provider_b.clone()];
        runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.health.two-providers".into(),
                expected_version: 1,
                settings,
            })
            .unwrap();

        runtime
            .settings_v2_test_provider(ProviderProbeRequestV2 {
                provider: provider_b,
                model_id: "model.fixture".into(),
                replacement_credential: None,
                use_stored_credential: false,
                draft_fingerprint: "health.provider-b".into(),
            })
            .unwrap();

        let snapshot = runtime.settings_v2_snapshot();
        let provider_a_health = snapshot
            .provider_health
            .iter()
            .find(|health| health.provider_id == "provider.a")
            .unwrap();
        let provider_b_health = snapshot
            .provider_health
            .iter()
            .find(|health| health.provider_id == "provider.b")
            .unwrap();
        assert_eq!(provider_a_health.state, "configured");
        assert_eq!(provider_b_health.state, "ready");
        assert_eq!(runtime.settings_snapshot().provider.state, "configured");
    }

    #[test]
    fn exact_provider_health_survives_profile_restart() {
        let root = TempDir::new().unwrap();
        let provider_port = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider_port.clone());
        let provider =
            saved_provider_configuration("provider.persisted", "https://persisted.example/v1");
        let mut settings = runtime.settings_v2_snapshot().settings;
        settings.providers = vec![provider.clone()];
        runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.health.persisted".into(),
                expected_version: 1,
                settings,
            })
            .unwrap();
        runtime
            .settings_v2_test_provider(ProviderProbeRequestV2 {
                provider,
                model_id: "model.fixture".into(),
                replacement_credential: None,
                use_stored_credential: false,
                draft_fingerprint: "health.persisted".into(),
            })
            .unwrap();
        drop(runtime);

        let reopened = self::runtime(&root, provider_port);
        let health = reopened
            .settings_v2_snapshot()
            .provider_health
            .into_iter()
            .find(|health| health.provider_id == "provider.persisted")
            .unwrap();
        assert_eq!(health.state, "ready");
        assert_eq!(
            health.detail.as_deref(),
            Some("Last native connection test succeeded for model 'fixture-model'.")
        );
        assert_eq!(reopened.settings_snapshot().provider.state, "ready");
    }

    #[test]
    fn saved_provider_edit_invalidates_persisted_health() {
        let root = TempDir::new().unwrap();
        let provider_port = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider_port.clone());
        let provider =
            saved_provider_configuration("provider.changed", "https://before.example/v1");
        let mut settings = runtime.settings_v2_snapshot().settings;
        settings.providers = vec![provider.clone()];
        runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.health.before-change".into(),
                expected_version: 1,
                settings,
            })
            .unwrap();
        runtime
            .settings_v2_test_provider(ProviderProbeRequestV2 {
                provider,
                model_id: "model.fixture".into(),
                replacement_credential: None,
                use_stored_credential: false,
                draft_fingerprint: "health.before-change".into(),
            })
            .unwrap();
        assert_eq!(
            runtime.settings_v2_snapshot().provider_health[0].state,
            "ready"
        );

        let mut changed = runtime.settings_v2_snapshot().settings;
        changed.providers[0].base_url = "https://after.example/v1".into();
        runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.health.after-change".into(),
                expected_version: 2,
                settings: changed,
            })
            .unwrap();
        assert_eq!(
            runtime.settings_v2_snapshot().provider_health[0].state,
            "configured"
        );
        drop(runtime);

        let reopened = self::runtime(&root, provider_port);
        assert_eq!(
            reopened.settings_v2_snapshot().provider_health[0].state,
            "configured"
        );
    }

    #[test]
    fn unsaved_provider_draft_probe_does_not_mutate_saved_health() {
        let root = TempDir::new().unwrap();
        let provider_port = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider_port.clone());
        let saved =
            saved_provider_configuration("provider.saved-draft", "https://saved-draft.example/v1");
        let mut settings = runtime.settings_v2_snapshot().settings;
        settings.providers = vec![saved.clone()];
        runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.health.saved-draft".into(),
                expected_version: 1,
                settings,
            })
            .unwrap();
        let repository = RepositoryRoot::open(root.path().join("documents")).unwrap();
        let health_before = repository
            .load(DocumentKind::Configuration, "provider-health.desktop")
            .unwrap()
            .unwrap();

        let mut unsaved = saved.clone();
        unsaved.base_url = "https://unsaved-draft.example/v1".into();
        let result = runtime
            .settings_v2_test_provider(ProviderProbeRequestV2 {
                provider: unsaved,
                model_id: "model.fixture".into(),
                replacement_credential: None,
                use_stored_credential: false,
                draft_fingerprint: "health.unsaved-draft".into(),
            })
            .unwrap();
        assert!(result.ok);
        let replacement_result = runtime
            .settings_v2_test_provider(ProviderProbeRequestV2 {
                provider: saved,
                model_id: "model.fixture".into(),
                replacement_credential: Some("unsaved-health-secret".to_owned().into()),
                use_stored_credential: false,
                draft_fingerprint: "health.unsaved-credential".into(),
            })
            .unwrap();
        assert!(replacement_result.ok);
        assert_eq!(
            runtime.settings_v2_snapshot().provider_health[0].state,
            "configured"
        );
        let health_after = repository
            .load(DocumentKind::Configuration, "provider-health.desktop")
            .unwrap()
            .unwrap();
        assert_eq!(health_after.version, health_before.version);
        assert_eq!(
            health_after.document.raw_json(),
            health_before.document.raw_json()
        );
        assert_profile_excludes(root.path(), "unsaved-health-secret");
        drop(runtime);

        let reopened = self::runtime(&root, provider_port);
        assert_eq!(
            reopened.settings_v2_snapshot().provider_health[0].state,
            "configured"
        );
    }

    #[test]
    fn failed_saved_credential_redemption_returns_a_probe_and_replaces_ready_health() {
        let root = TempDir::new().unwrap();
        let store = Arc::new(RecordingCredentialStore::default());
        let provider_port = Arc::new(FixtureProvider::new());
        let mut runtime = runtime_with_store(&root, provider_port.clone(), store.clone());
        commit_provider(
            &mut runtime,
            "settings.health.credential.seed",
            1,
            "https://provider.example/v1",
            "fixture-model",
            "replace",
            Some("credential-that-will-disappear"),
        )
        .unwrap();
        let snapshot = runtime.settings_v2_snapshot();
        let provider = snapshot.settings.providers[0].clone();
        let credential_ref = snapshot.settings.credentials[0].credential_ref.clone();
        let request = |fingerprint: &str| ProviderProbeRequestV2 {
            provider: provider.clone(),
            model_id: provider.models[0].id.clone(),
            replacement_credential: None,
            use_stored_credential: true,
            draft_fingerprint: fingerprint.into(),
        };

        let ready = runtime
            .settings_v2_test_provider(request("health.credential.ready"))
            .unwrap();
        assert!(ready.ok);
        assert_eq!(
            runtime.settings_v2_snapshot().provider_health[0].state,
            "ready"
        );
        assert_eq!(provider_port.connection_tests.load(Ordering::SeqCst), 1);

        store
            .delete(&CredentialRef(
                StableId::parse(credential_ref).expect("stored credential reference"),
            ))
            .unwrap();
        let failed = runtime
            .settings_v2_test_provider(request("health.credential.missing"))
            .unwrap();

        assert!(!failed.ok);
        assert_eq!(failed.model_id, None);
        assert_eq!(provider_port.connection_tests.load(Ordering::SeqCst), 1);
        let health = &runtime.settings_v2_snapshot().provider_health[0];
        assert_eq!(health.state, "error");
        assert!(
            health
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("could not redeem the saved credential"))
        );
        assert_eq!(runtime.settings_v2_snapshot().version, snapshot.version);
    }

    #[test]
    fn retained_credentials_cannot_cross_provider_endpoints() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider.clone());
        commit_provider(
            &mut runtime,
            "settings.key.first",
            1,
            "https://provider-a.example/v1",
            "fixture-model",
            "replace",
            Some("secret-a"),
        )
        .unwrap();

        let saved = runtime.documents.legacy_provider();
        assert_eq!(
            saved.credential_endpoint.as_deref(),
            Some("https://provider-a.example/v1")
        );
        assert!(
            saved
                .credential_ref
                .as_deref()
                .is_some_and(|reference| reference.starts_with("credential."))
        );
        let error = commit_provider(
            &mut runtime,
            "settings.endpoint.keep",
            2,
            "https://provider-b.example/v1",
            "fixture-model",
            "keep",
            None,
        )
        .unwrap_err();
        assert!(error.contains("Replace or clear"));
        assert_eq!(runtime.settings_snapshot().version, 2);

        let test = runtime.settings_test_provider(ProviderTestInput {
            base_url: "https://provider-b.example/v1".into(),
            model: "fixture-model".into(),
            api_key: None,
            use_stored_credential: true,
        });
        assert!(!test.ok);
        assert!(
            test.message
                .contains("bound to the saved provider endpoint")
        );
        assert_eq!(provider.connection_tests.load(Ordering::SeqCst), 0);

        let accepted = commit_provider(
            &mut runtime,
            "settings.endpoint.replace",
            2,
            "https://provider-b.example/v1",
            "fixture-model",
            "replace",
            Some("secret-b"),
        )
        .unwrap();
        assert!(accepted.accepted);
        assert_eq!(
            runtime
                .documents
                .legacy_provider()
                .credential_endpoint
                .as_deref(),
            Some("https://provider-b.example/v1")
        );
    }

    #[test]
    fn credential_replacement_uses_fresh_refs_and_cleans_obsolete_values() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let store = Arc::new(RecordingCredentialStore::default());
        let mut runtime = runtime_with_store(&root, provider, store.clone());
        commit_provider(
            &mut runtime,
            "settings.credential.first",
            1,
            "https://provider.example/v1",
            "fixture-model",
            "replace",
            Some("secret-one"),
        )
        .unwrap();
        let first_ref = runtime.documents.legacy_provider().credential_ref.unwrap();
        let projection = serde_json::to_string(&runtime.settings_v2_snapshot()).unwrap();
        assert!(!projection.contains("secret-one"));
        assert!(projection.contains(&first_ref));

        commit_provider(
            &mut runtime,
            "settings.credential.second",
            2,
            "https://provider.example/v1",
            "fixture-model",
            "replace",
            Some("secret-two"),
        )
        .unwrap();
        let second_ref = runtime.documents.legacy_provider().credential_ref.unwrap();
        assert_ne!(first_ref, second_ref);
        assert_ne!(first_ref, "credential.provider.primary");
        assert_eq!(
            store.puts.lock().unwrap().as_slice(),
            &[first_ref.clone(), second_ref.clone()]
        );
        assert_eq!(
            store.deletes.lock().unwrap().as_slice(),
            std::slice::from_ref(&first_ref)
        );
        assert!(runtime.credentials.resolve(Some(&first_ref)).is_err());
        assert_eq!(
            runtime.credentials.resolve(Some(&second_ref)).unwrap(),
            Some("secret-two".into())
        );

        commit_provider(
            &mut runtime,
            "settings.credential.clear",
            3,
            "https://provider.example/v1",
            "fixture-model",
            "clear",
            None,
        )
        .unwrap();
        assert!(runtime.documents.legacy_provider().credential_ref.is_none());
        assert!(
            runtime
                .documents
                .legacy_provider()
                .credential_endpoint
                .is_none()
        );
        assert_eq!(
            store.deletes.lock().unwrap().as_slice(),
            &[first_ref, second_ref.clone()]
        );
        assert!(runtime.credentials.resolve(Some(&second_ref)).is_err());
    }

    #[test]
    fn credential_cleanup_failure_does_not_roll_back_committed_settings() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let store = Arc::new(RecordingCredentialStore::default());
        let mut runtime = runtime_with_store(&root, provider, store.clone());
        commit_provider(
            &mut runtime,
            "settings.cleanup.first",
            1,
            "https://provider.example/v1",
            "fixture-model",
            "replace",
            Some("secret-one"),
        )
        .unwrap();
        let first_ref = runtime.documents.legacy_provider().credential_ref.unwrap();

        store.fail_delete.store(true, Ordering::SeqCst);
        let replace_receipt = commit_provider(
            &mut runtime,
            "settings.cleanup.replace",
            2,
            "https://provider.example/v1",
            "fixture-model",
            "replace",
            Some("secret-two"),
        )
        .unwrap();
        let second_ref = runtime.documents.legacy_provider().credential_ref.unwrap();
        assert!(replace_receipt.accepted);
        assert!(replace_receipt.reason.is_some());
        assert_eq!(replace_receipt.current_version, 3);
        assert_ne!(first_ref, second_ref);
        assert_eq!(
            runtime.credentials.resolve(Some(&first_ref)).unwrap(),
            Some("secret-one".into())
        );

        let clear_receipt = commit_provider(
            &mut runtime,
            "settings.cleanup.clear",
            3,
            "https://provider.example/v1",
            "fixture-model",
            "clear",
            None,
        )
        .unwrap();
        assert!(clear_receipt.accepted);
        assert!(clear_receipt.reason.is_some());
        assert_eq!(clear_receipt.current_version, 4);
        assert!(runtime.documents.legacy_provider().credential_ref.is_none());
        assert_eq!(
            runtime.credentials.resolve(Some(&second_ref)).unwrap(),
            Some("secret-two".into())
        );
    }

    #[test]
    fn credential_journal_recovers_a_crash_before_the_os_store_put() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let store = Arc::new(RecordingCredentialStore::default());
        let mut runtime = runtime_with_store(&root, provider.clone(), store.clone());
        runtime.arm_credential_crash_point(CredentialCrashPointV1::BeforePut);
        let error = commit_provider(
            &mut runtime,
            "settings.crash.before-put",
            1,
            "https://provider.example/v1",
            "fixture-model",
            "replace",
            Some("before-put-plaintext"),
        )
        .unwrap_err();
        assert!(error.contains("simulated process termination"));
        assert!(store.puts.lock().unwrap().is_empty());
        assert!(store.live.lock().unwrap().is_empty());
        assert_eq!(runtime.credential_journal.pending().len(), 1);
        let planned_ref = runtime.credential_journal.pending()[0].credential_refs[0].clone();
        assert_profile_excludes(root.path(), "before-put-plaintext");
        drop(runtime);

        let reopened = runtime_with_store(&root, provider, store.clone());
        assert_eq!(reopened.settings_v2_snapshot().version, 1);
        assert!(reopened.credential_journal.pending().is_empty());
        assert!(store.live.lock().unwrap().is_empty());
        assert_eq!(store.puts.lock().unwrap().as_slice(), &[] as &[String]);
        assert_eq!(
            store.deletes.lock().unwrap().as_slice(),
            std::slice::from_ref(&planned_ref)
        );
    }

    #[test]
    fn credential_journal_removes_a_put_that_crashed_before_settings() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let store = Arc::new(RecordingCredentialStore::default());
        let mut runtime = runtime_with_store(&root, provider.clone(), store.clone());
        runtime.arm_credential_crash_point(CredentialCrashPointV1::AfterPutBeforeSettings);
        let error = commit_provider(
            &mut runtime,
            "settings.crash.after-put",
            1,
            "https://provider.example/v1",
            "fixture-model",
            "replace",
            Some("after-put-plaintext"),
        )
        .unwrap_err();
        assert!(error.contains("simulated process termination"));
        let planned_ref = runtime.credential_journal.pending()[0].credential_refs[0].clone();
        assert_eq!(
            store.puts.lock().unwrap().as_slice(),
            std::slice::from_ref(&planned_ref)
        );
        let live = store.live.lock().unwrap();
        assert_eq!(live.len(), 1);
        assert!(live.contains(&planned_ref));
        drop(live);
        assert_eq!(runtime.settings_v2_snapshot().version, 1);
        assert_profile_excludes(root.path(), "after-put-plaintext");
        drop(runtime);

        let reopened = runtime_with_store(&root, provider, store.clone());
        assert!(reopened.credential_journal.pending().is_empty());
        assert!(store.live.lock().unwrap().is_empty());
        assert_eq!(
            store.deletes.lock().unwrap().as_slice(),
            std::slice::from_ref(&planned_ref)
        );
    }

    #[test]
    fn credential_journal_finishes_replacement_after_settings_commit() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let store = Arc::new(RecordingCredentialStore::default());
        let mut runtime = runtime_with_store(&root, provider.clone(), store.clone());
        commit_provider(
            &mut runtime,
            "settings.crash.replace.seed",
            1,
            "https://provider.example/v1",
            "fixture-model",
            "replace",
            Some("replacement-old-plaintext"),
        )
        .unwrap();
        let old_ref = runtime.documents.legacy_provider().credential_ref.unwrap();
        runtime.arm_credential_crash_point(
            CredentialCrashPointV1::AfterReplacementSettingsBeforeObsoleteDelete,
        );
        let error = commit_provider(
            &mut runtime,
            "settings.crash.replace.next",
            2,
            "https://provider.example/v1",
            "fixture-model",
            "replace",
            Some("replacement-new-plaintext"),
        )
        .unwrap_err();
        assert!(error.contains("simulated process termination"));
        let new_ref = runtime.documents.legacy_provider().credential_ref.unwrap();
        assert_ne!(old_ref, new_ref);
        assert_eq!(runtime.settings_v2_snapshot().version, 3);
        assert_eq!(runtime.credential_journal.pending().len(), 1);
        assert_eq!(
            *store.live.lock().unwrap(),
            BTreeSet::from([old_ref.clone(), new_ref.clone()])
        );
        assert!(store.deletes.lock().unwrap().is_empty());
        assert_profile_excludes(root.path(), "replacement-old-plaintext");
        assert_profile_excludes(root.path(), "replacement-new-plaintext");
        drop(runtime);

        let reopened = runtime_with_store(&root, provider, store.clone());
        assert!(reopened.credential_journal.pending().is_empty());
        assert_eq!(
            reopened
                .documents
                .legacy_provider()
                .credential_ref
                .as_deref(),
            Some(new_ref.as_str())
        );
        let live = store.live.lock().unwrap();
        assert_eq!(live.len(), 1);
        assert!(live.contains(&new_ref));
        drop(live);
        assert_eq!(
            store.deletes.lock().unwrap().as_slice(),
            std::slice::from_ref(&old_ref)
        );
    }

    #[test]
    fn credential_journal_finishes_delete_after_metadata_commit() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let store = Arc::new(RecordingCredentialStore::default());
        let mut runtime = runtime_with_store(&root, provider.clone(), store.clone());
        commit_provider(
            &mut runtime,
            "settings.crash.delete.seed",
            1,
            "https://provider.example/v1",
            "fixture-model",
            "replace",
            Some("delete-window-plaintext"),
        )
        .unwrap();
        let deleted_ref = runtime.documents.legacy_provider().credential_ref.unwrap();
        runtime.arm_credential_crash_point(
            CredentialCrashPointV1::AfterDeleteMetadataBeforeStoreDelete,
        );
        let error = commit_provider(
            &mut runtime,
            "settings.crash.delete.clear",
            2,
            "https://provider.example/v1",
            "fixture-model",
            "clear",
            None,
        )
        .unwrap_err();
        assert!(error.contains("simulated process termination"));
        assert!(runtime.documents.legacy_provider().credential_ref.is_none());
        assert_eq!(runtime.settings_v2_snapshot().version, 3);
        assert_eq!(runtime.credential_journal.pending().len(), 1);
        let live = store.live.lock().unwrap();
        assert_eq!(live.len(), 1);
        assert!(live.contains(&deleted_ref));
        drop(live);
        assert!(store.deletes.lock().unwrap().is_empty());
        assert_profile_excludes(root.path(), "delete-window-plaintext");
        drop(runtime);

        let reopened = runtime_with_store(&root, provider, store.clone());
        assert!(reopened.credential_journal.pending().is_empty());
        assert!(store.live.lock().unwrap().is_empty());
        assert_eq!(
            store.deletes.lock().unwrap().as_slice(),
            std::slice::from_ref(&deleted_ref)
        );
    }

    #[test]
    fn credential_cleanup_failure_is_visible_and_retried_on_later_opens() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let store = Arc::new(RecordingCredentialStore::default());
        let mut runtime = runtime_with_store(&root, provider.clone(), store.clone());
        commit_provider(
            &mut runtime,
            "settings.retry-cleanup.seed",
            1,
            "https://provider.example/v1",
            "fixture-model",
            "replace",
            Some("retry-old-plaintext"),
        )
        .unwrap();
        let old_ref = runtime.documents.legacy_provider().credential_ref.unwrap();
        store.fail_delete.store(true, Ordering::SeqCst);
        let receipt = commit_provider(
            &mut runtime,
            "settings.retry-cleanup.replace",
            2,
            "https://provider.example/v1",
            "fixture-model",
            "replace",
            Some("retry-new-plaintext"),
        )
        .unwrap();
        let new_ref = runtime.documents.legacy_provider().credential_ref.unwrap();
        assert!(
            receipt
                .reason
                .as_deref()
                .is_some_and(|reason| { reason.contains("cleanup remains pending") })
        );
        assert_eq!(runtime.credential_journal.pending().len(), 1);
        assert_eq!(
            store.deletes.lock().unwrap().as_slice(),
            std::slice::from_ref(&old_ref)
        );
        drop(runtime);

        let reopened = runtime_with_store(&root, provider.clone(), store.clone());
        assert_eq!(reopened.credential_journal.pending().len(), 1);
        assert_eq!(reopened.settings_snapshot().provider.state, "error");
        assert!(
            reopened
                .settings_snapshot()
                .provider
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("cleanup remains pending"))
        );
        assert_eq!(
            store.deletes.lock().unwrap().as_slice(),
            &[old_ref.clone(), old_ref.clone()]
        );
        assert_eq!(
            *store.live.lock().unwrap(),
            BTreeSet::from([old_ref.clone(), new_ref.clone()])
        );
        drop(reopened);

        store.fail_delete.store(false, Ordering::SeqCst);
        let repaired = runtime_with_store(&root, provider, store.clone());
        assert!(repaired.credential_journal.pending().is_empty());
        assert_eq!(
            store.deletes.lock().unwrap().as_slice(),
            &[old_ref.clone(), old_ref.clone(), old_ref]
        );
        let live = store.live.lock().unwrap();
        assert_eq!(live.len(), 1);
        assert!(live.contains(&new_ref));
        assert_profile_excludes(root.path(), "retry-old-plaintext");
        assert_profile_excludes(root.path(), "retry-new-plaintext");
    }

    #[test]
    fn credential_replacement_preserves_the_active_frozen_binding_until_new_chat() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let store = Arc::new(RecordingCredentialStore::default());
        let mut runtime = runtime_with_store(&root, provider.clone(), store.clone());
        commit_provider(
            &mut runtime,
            "settings.session-key.first",
            1,
            "https://provider.example/v1",
            "fixture-model",
            "replace",
            Some("secret-one"),
        )
        .unwrap();
        let first_ref = runtime.documents.legacy_provider().credential_ref.unwrap();
        runtime
            .command(send("chat.credential.start", 0, "hello"))
            .unwrap();

        let replacement = commit_provider(
            &mut runtime,
            "settings.session-key.replace",
            2,
            "https://provider.example/v1",
            "fixture-model",
            "replace",
            Some("secret-two"),
        )
        .unwrap();
        assert!(
            replacement
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("active Chat"))
        );
        let second_ref = runtime.documents.legacy_provider().credential_ref.unwrap();
        assert_ne!(first_ref, second_ref);
        assert!(store.deletes.lock().unwrap().is_empty());
        assert!(
            runtime
                .settings_v2_snapshot()
                .settings
                .credential(&first_ref)
                .is_some()
        );

        runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.credential.follow-up".into(),
                expected_version: 6,
                action: "enqueue".into(),
                target_id: None,
                payload: json!({"input":"again"}),
            })
            .unwrap();
        let requests = provider.execution_requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        for request in requests.iter() {
            assert_eq!(
                request
                    .provider
                    .credential
                    .as_ref()
                    .map(|credential| credential.credential.0.as_str()),
                Some(first_ref.as_str())
            );
        }
        drop(requests);

        let error = runtime
            .settings_v2_delete_credential(CredentialDeleteInputV2 {
                command_id: "settings.session-key.delete-active".into(),
                expected_version: 3,
                credential_ref: first_ref.clone(),
            })
            .unwrap_err();
        assert!(error.contains("active Chat's frozen execution context"));
        runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.credential.new".into(),
                expected_version: 11,
                action: "new_chat".into(),
                target_id: None,
                payload: json!({}),
            })
            .unwrap();
        runtime
            .settings_v2_delete_credential(CredentialDeleteInputV2 {
                command_id: "settings.session-key.delete-released".into(),
                expected_version: 3,
                credential_ref: first_ref.clone(),
            })
            .unwrap();
        assert_eq!(store.deletes.lock().unwrap().as_slice(), &[first_ref]);

        runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.credential.next-start".into(),
                expected_version: 0,
                action: "start".into(),
                target_id: None,
                payload: json!({
                    "workflowId":"workflow.simple-chat",
                    "input":"new credentials",
                    "attachments":[],
                }),
            })
            .unwrap();
        let requests = provider.execution_requests.lock().unwrap();
        assert_eq!(
            requests[2]
                .provider
                .credential
                .as_ref()
                .map(|credential| credential.credential.0.as_str()),
            Some(second_ref.as_str())
        );
        drop(requests);
        for entry in fs::read_dir(root.path().join("history")).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() {
                let bytes = fs::read(path).unwrap();
                assert!(
                    !bytes
                        .windows(b"secret-one".len())
                        .any(|window| window == b"secret-one")
                );
                assert!(
                    !bytes
                        .windows(b"secret-two".len())
                        .any(|window| window == b"secret-two")
                );
            }
        }
    }

    #[test]
    fn active_chat_stays_frozen_while_settings_and_workflow_edits_feed_the_next_chat() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider.clone());
        configure(&mut runtime);
        runtime.command(send("chat.frozen", 0, "hello")).unwrap();
        let first_projection = runtime.snapshot(0).unwrap().chat;
        assert_ne!(first_projection.chat_id, "chat.local");
        assert_ne!(first_projection.run_id, "run.local");

        commit_provider(
            &mut runtime,
            "settings.future.provider",
            2,
            "http://127.0.0.1:9999/v1",
            "future-model",
            "keep",
            None,
        )
        .unwrap();

        let mut workflow = runtime.workflow_snapshot_for("workflow.simple-chat".into());
        workflow.document["name"] = Value::String("Future Simple Chat".into());
        runtime
            .workflow_commit(WorkflowCommitInput {
                command_id: "workflow.future".into(),
                expected_version: workflow.version,
                document: workflow.document.clone(),
                workflow_id: Some("workflow.simple-chat".into()),
            })
            .unwrap();
        drop(runtime);
        let mut runtime = self::runtime(&root, provider.clone());

        runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.frozen.follow-up".into(),
                expected_version: 6,
                action: "enqueue".into(),
                target_id: Some(first_projection.chat_id.clone()),
                payload: json!({"input":"again"}),
            })
            .unwrap();
        let requests = provider.execution_requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].chat_id, requests[1].chat_id);
        assert_eq!(requests[0].run_id, requests[1].run_id);
        assert_eq!(
            requests[0].frozen_context_hash,
            requests[1].frozen_context_hash
        );
        assert_eq!(requests[0].provider.model, "fixture-model");
        assert_eq!(requests[1].provider.model, "fixture-model");
        let first_chat_id = requests[0].chat_id.clone();
        let first_run_id = requests[0].run_id.clone();
        let first_context_hash = requests[0].frozen_context_hash.clone();
        drop(requests);

        runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.future.new".into(),
                expected_version: 11,
                action: "new_chat".into(),
                target_id: Some(first_projection.chat_id),
                payload: json!({}),
            })
            .unwrap();
        let next_chat_id = runtime.snapshot(0).unwrap().chat.chat_id;
        runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.future.start".into(),
                expected_version: 0,
                action: "start".into(),
                target_id: Some(next_chat_id),
                payload: json!({
                    "workflowId":"workflow.simple-chat",
                    "input":"new chat",
                    "attachments":[],
                }),
            })
            .unwrap();
        let requests = provider.execution_requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        let next = &requests[2];
        assert_ne!(next.chat_id, first_chat_id);
        assert_ne!(next.run_id, first_run_id);
        assert_ne!(next.frozen_context_hash, first_context_hash);
        assert_eq!(next.provider.model, "future-model");
        assert_eq!(
            runtime.snapshot(0).unwrap().chat.workflow_name.as_deref(),
            Some("Future Simple Chat")
        );
    }

    #[test]
    fn selected_project_scope_is_native_frozen_and_later_edits_feed_only_new_chats() {
        let root = TempDir::new().unwrap();
        let first_workspace = TempDir::new().unwrap();
        let future_workspace = TempDir::new().unwrap();
        for (workspace, branch) in [
            (&first_workspace, "main"),
            (&future_workspace, "future/project-scope"),
        ] {
            fs::create_dir(workspace.path().join(".git")).unwrap();
            fs::write(
                workspace.path().join(".git/HEAD"),
                format!("ref: refs/heads/{branch}\n"),
            )
            .unwrap();
        }
        let provider = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider.clone());
        configure(&mut runtime);
        let mut settings = runtime.settings_v2_snapshot().settings;
        settings.projects.push(saved_project(
            "project.atlas",
            "Project Atlas",
            first_workspace.path(),
            WorkspaceKindV2::GitWorktree,
        ));
        runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.project.atlas".into(),
                expected_version: 2,
                settings,
            })
            .unwrap();
        assert_eq!(
            runtime.snapshot(0).unwrap().projects[0].name,
            "Project Atlas"
        );

        let mut start = send("chat.project.start", 0, "hello project");
        start.payload["projectId"] = Value::String("project.atlas".into());
        runtime.command(start).unwrap();
        let first_snapshot = runtime.snapshot(0).unwrap();
        let first_projection = first_snapshot.chat;
        assert_eq!(first_projection.scope, "Project Atlas");
        assert_eq!(
            first_projection.project_id.as_deref(),
            Some("project.atlas")
        );
        assert_eq!(first_projection.branch.as_deref(), Some("main"));
        let first_history_entry = first_snapshot
            .history
            .iter()
            .find(|entry| entry.chat_id == first_projection.chat_id)
            .unwrap();
        assert_eq!(
            first_history_entry.project_id.as_deref(),
            Some("project.atlas")
        );
        assert_eq!(
            first_history_entry.project_name.as_deref(),
            Some("Project Atlas")
        );
        let first_context = runtime.history.current_frozen_context().unwrap().unwrap();
        let first_project = first_context.context.project.as_ref().unwrap();
        assert_eq!(
            first_project.workspace_binding.root,
            fs::canonicalize(first_workspace.path()).unwrap()
        );
        assert!(first_project.workspace_identity_hash.starts_with("sha256:"));

        let mut changed = runtime.settings_v2_snapshot().settings;
        changed.projects[0] = saved_project(
            "project.atlas",
            "Future Atlas",
            future_workspace.path(),
            WorkspaceKindV2::GitWorktree,
        );
        runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.project.future".into(),
                expected_version: 3,
                settings: changed,
            })
            .unwrap();
        drop(runtime);

        let mut reopened = self::runtime(&root, provider.clone());
        let reopened_projection = reopened.snapshot(0).unwrap();
        assert_eq!(reopened_projection.chat.scope, "Project Atlas");
        assert_eq!(reopened_projection.chat.branch.as_deref(), Some("main"));
        assert_eq!(reopened_projection.projects[0].name, "Future Atlas");
        reopened
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.project.follow-up".into(),
                expected_version: 6,
                action: "enqueue".into(),
                target_id: Some(first_projection.chat_id.clone()),
                payload: json!({"input":"again"}),
            })
            .unwrap();
        let requests = provider.execution_requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].workspace, requests[1].workspace);
        assert_eq!(
            requests[0].frozen_context_hash,
            requests[1].frozen_context_hash
        );
        assert_eq!(
            requests[1].workspace.as_ref().unwrap().root,
            fs::canonicalize(first_workspace.path()).unwrap()
        );
        let first_hash = requests[0].frozen_context_hash.clone();
        drop(requests);

        reopened
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.project.new".into(),
                expected_version: 11,
                action: "new_chat".into(),
                target_id: Some(first_projection.chat_id),
                payload: json!({}),
            })
            .unwrap();
        let next_chat = reopened.snapshot(0).unwrap().chat.chat_id;
        reopened
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.project.future-start".into(),
                expected_version: 0,
                action: "start".into(),
                target_id: Some(next_chat),
                payload: json!({
                    "workflowId":"workflow.simple-chat",
                    "projectId":"project.atlas",
                    "input":"new project Chat",
                    "attachments":[],
                }),
            })
            .unwrap();
        let next_projection = reopened.snapshot(0).unwrap().chat;
        assert_eq!(next_projection.scope, "Future Atlas");
        assert_eq!(
            next_projection.branch.as_deref(),
            Some("future/project-scope")
        );
        let requests = provider.execution_requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_ne!(requests[2].frozen_context_hash, first_hash);
        assert_eq!(
            requests[2].workspace.as_ref().unwrap().root,
            fs::canonicalize(future_workspace.path()).unwrap()
        );
    }

    #[test]
    fn remote_missing_and_follow_up_project_injection_fail_before_provider_effect() {
        let root = TempDir::new().unwrap();
        let remote_root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider.clone());
        configure(&mut runtime);
        let mut settings = runtime.settings_v2_snapshot().settings;
        settings.projects = vec![
            saved_project(
                "project.remote",
                "Remote",
                remote_root.path(),
                WorkspaceKindV2::Remote,
            ),
            saved_project(
                "project.missing",
                "Missing",
                &root.path().join("does-not-exist"),
                WorkspaceKindV2::LocalDirectory,
            ),
        ];
        runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.project.invalid".into(),
                expected_version: 2,
                settings,
            })
            .unwrap();
        let choices = runtime.snapshot(0).unwrap().projects;
        assert!(
            choices
                .iter()
                .all(|project| project.project_id != "project.remote")
        );
        assert!(
            choices
                .iter()
                .any(|project| project.project_id == "project.missing")
        );

        for (command_id, project_id, expected) in [
            (
                "chat.project.remote",
                "project.remote",
                "remote-workspace adapter",
            ),
            ("chat.project.missing", "project.missing", "unavailable"),
        ] {
            let mut start = send(command_id, 0, "must not run");
            start.payload["projectId"] = Value::String(project_id.into());
            let error = runtime.command(start).unwrap_err();
            assert!(error.contains(expected), "unexpected error: {error}");
        }
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(runtime.snapshot(0).unwrap().version, 0);

        runtime
            .command(send("chat.no-project", 0, "still valid"))
            .unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.snapshot(0).unwrap().chat.scope, "No project");
        let injected = runtime
            .command(UiCommandInput {
                schema_version: 1,
                command_id: "chat.project.inject-follow-up".into(),
                expected_version: 6,
                action: "enqueue".into(),
                target_id: None,
                payload: json!({"input":"again","projectId":"project.missing"}),
            })
            .unwrap_err();
        assert!(injected.contains("only by the first Chat start"));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }

    #[cfg(unix)]
    fn make_test_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(not(unix))]
    fn make_test_executable(_path: &Path) {}

    fn write_extension_fixture(root: &TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
        let entry_point = root.path().join("extension-entry");
        fs::write(&entry_point, b"#!/bin/sh\nexit 0\n").unwrap();
        make_test_executable(&entry_point);
        let content_hash = format!(
            "sha256:{:x}",
            Sha256::digest(fs::read(&entry_point).unwrap())
        );
        let manifest = root.path().join("extension.json");
        fs::write(
            &manifest,
            serde_json::to_vec(&json!({
                "schemaVersion": 1,
                "extensionId": "extension.registered",
                "version": "1.0.0",
                "contentHash": content_hash,
                "aworkitVersionRequirement": ">=0.1.0,<0.2.0",
                "protocolVersion": 1,
                "entryPoint": {"program": "extension-entry", "arguments": ["--stdio"]},
                "contributions": [{
                    "contributionId": "tool.registered",
                    "kind": "tool",
                    "inputSchema": {"type": "object"},
                    "outputSchema": {"type": "object"}
                }],
                "dependencies": []
            }))
            .unwrap(),
        )
        .unwrap();
        (manifest, entry_point)
    }

    #[test]
    fn extension_registration_is_versioned_durable_and_generic_save_cannot_fabricate_it() {
        let root = TempDir::new().unwrap();
        let package = TempDir::new().unwrap();
        let (manifest, entry_point) = write_extension_fixture(&package);
        let provider = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider.clone());
        let discovered = runtime
            .settings_v2_inspect_extension(&manifest)
            .expect("inert discovery");
        let mut settings = runtime.settings_v2_snapshot().settings;
        settings.extensions.push(discovered);
        runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.extension.discovery".into(),
                expected_version: 1,
                settings,
            })
            .expect("save discovery");

        let command = ExtensionRegisterInputV2 {
            command_id: "settings.extension.register".into(),
            expected_version: 2,
            extension_id: "extension.registered".into(),
        };
        let registered = runtime
            .settings_v2_register_extension(command.clone())
            .expect("register extension");
        let replay = runtime
            .settings_v2_register_extension(command)
            .expect("idempotent replay");
        assert_eq!(registered.current_version, 3);
        assert_eq!(replay.current_version, 3);
        let extension = &runtime.settings_v2_snapshot().settings.extensions[0];
        assert_eq!(extension.status, ExtensionStatusV2::Installed);
        assert!(!extension.enabled);
        assert!(!extension.trust_accepted);

        let mut forbidden_enablement = runtime.settings_v2_snapshot().settings;
        forbidden_enablement.extensions[0].trust_accepted = true;
        forbidden_enablement.extensions[0].enabled = true;
        let error = runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.extension.enable-rejected".into(),
                expected_version: 3,
                settings: forbidden_enablement,
            })
            .unwrap_err();
        assert!(error.contains("cannot be enabled through generic Settings"));
        assert_eq!(runtime.settings_v2_snapshot().version, 3);

        let mut trusted_metadata = runtime.settings_v2_snapshot().settings;
        trusted_metadata.extensions[0].trust_accepted = true;
        let trusted = runtime
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.extension.trust-metadata".into(),
                expected_version: 3,
                settings: trusted_metadata,
            })
            .expect("record trust metadata without enabling extension code");
        assert_eq!(trusted.current_version, 4);

        // Simulate a profile written by a build that already persisted the
        // legacy flag. Opening and saving that truth must not fabricate a new
        // enable operation, while the user must remain able to turn it off.
        let mut legacy_enabled_metadata = runtime.settings_v2_snapshot().settings;
        legacy_enabled_metadata.extensions[0].enabled = true;
        assert_eq!(
            runtime
                .documents
                .save_settings(4, legacy_enabled_metadata)
                .expect("seed preexisting legacy enabled metadata"),
            5
        );
        drop(runtime);

        let mut reopened = self::runtime(&root, provider);
        assert_eq!(
            reopened.settings_v2_snapshot().settings.extensions[0].status,
            ExtensionStatusV2::Installed
        );
        assert!(reopened.settings_v2_snapshot().settings.extensions[0].enabled);
        let mut preserved_legacy = reopened.settings_v2_snapshot().settings;
        preserved_legacy.extensions[0].name = "Friendly registered extension".into();
        let preserved = reopened
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.extension.preserve-legacy-enabled".into(),
                expected_version: 5,
                settings: preserved_legacy,
            })
            .expect("preserve existing legacy enabled metadata");
        assert_eq!(preserved.current_version, 6);

        fs::write(&entry_point, b"#!/bin/sh\nexit 7\n").unwrap();
        make_test_executable(&entry_point);
        let mut changed = reopened.settings_v2_snapshot().settings;
        changed.appearance.font_scale = 1.1;
        let error = reopened
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.extension.drift".into(),
                expected_version: 6,
                settings: changed,
            })
            .unwrap_err();
        assert!(error.contains("verified identity is unavailable"));

        let mut disabled_legacy = reopened.settings_v2_snapshot().settings;
        disabled_legacy.extensions[0].enabled = false;
        disabled_legacy.appearance.font_scale = 1.1;
        let disabled = reopened
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.extension.disable-legacy-enabled".into(),
                expected_version: 6,
                settings: disabled_legacy,
            })
            .expect("disable existing legacy enabled metadata");
        assert_eq!(disabled.current_version, 7);
        assert!(!reopened.settings_v2_snapshot().settings.extensions[0].enabled);

        let other_root = TempDir::new().unwrap();
        let mut other = self::runtime(&other_root, Arc::new(FixtureProvider::new()));
        let mut fabricated = other.settings_v2_snapshot().settings;
        fabricated.extensions.push(ExtensionConfigurationV2 {
            id: "extension.fake".into(),
            name: "Fake".into(),
            version: "1.0.0".into(),
            status: ExtensionStatusV2::Installed,
            enabled: false,
            trust_accepted: false,
            manifest_path: "/tmp/fake/extension.json".into(),
            entry_point: Some("/tmp/fake/run".into()),
            content_hash: Some(format!("sha256:{}", "a".repeat(64))),
            compatibility: Some("fabricated".into()),
            provenance: Some("fabricated".into()),
            configuration: BTreeMap::new(),
        });
        let error = other
            .settings_v2_commit(SettingsV2CommitInput {
                command_id: "settings.extension.fabricated".into(),
                expected_version: 1,
                settings: fabricated,
            })
            .unwrap_err();
        assert!(error.contains("cannot fabricate installation"));
    }

    #[test]
    fn editable_advanced_workflow_commit_does_not_bypass_the_run_gate() {
        let root = TempDir::new().unwrap();
        let provider = Arc::new(FixtureProvider::new());
        let mut runtime = runtime(&root, provider);
        let snapshot = runtime.workflow_snapshot_for("workflow.simple-chat".into());
        let advanced = json!({
            "schemaVersion": 1,
            "id": "workflow.simple-chat",
            "name": "Advanced harness",
            "nodes": [
                {
                    "id": "input.custom",
                    "type": "input",
                    "configuration": {"future": {"retained": true}}
                },
                {"id": "approval.custom", "type": "approval"}
            ],
            "edges": [{
                "id": "approval-edge",
                "source": "input.custom",
                "target": "approval.custom",
                "futureEdge": {"retained": true}
            }]
        });
        let receipt = runtime
            .workflow_commit(WorkflowCommitInput {
                command_id: "workflow.advanced.save".into(),
                expected_version: snapshot.version,
                document: advanced.clone(),
                workflow_id: Some("workflow.simple-chat".into()),
            })
            .unwrap();
        assert!(receipt.accepted);
        assert_eq!(
            runtime
                .workflow_snapshot_for("workflow.simple-chat".into())
                .document,
            advanced
        );

        let error = runtime
            .command(send("chat.advanced", 0, "hello"))
            .unwrap_err();
        assert!(error.contains("not executable"), "{error}");
        assert_eq!(runtime.snapshot(0).unwrap().chat.phase, "draft");
    }
}
