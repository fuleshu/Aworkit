//! Frozen project-file authority used by the desktop Agent tool loop.
//!
//! Model output is treated only as an invocation proposal. This module binds
//! that proposal to the Run's immutable workspace and tool Settings, sends it
//! through the durable trusted-core broker, then executes it behind the
//! authenticated capability-host gateway. Read/search outcomes are durably
//! settled before they can be returned to the provider.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use aworkit_capability_host::{
    AdmissionReceipt, AdmittedInvocationDispatcherV1, ApprovedInvocationEnvelopeV1,
    CancellationToken, CapabilityDescriptor, CapabilityHost, CapabilityKind, FileAuthority,
    FileReadRequestV1, FileSearchRequestV1, ModelToolCallV1, ModelToolDefinitionV1,
    ModelToolExchangeV1, ModelToolResultV1, ProjectFiles, SideEffectClass,
};
use aworkit_local_store::{CommitBatch, Deduplication, Event, LocalHistoryStore, StoreError};
use aworkit_protocol::{ProcessGeneration, SchemaVersion, StableId};
use aworkit_trusted_core::{
    ApprovalRequirement, ApprovedDispatchV1, ApprovedHostDispatchPortV1, AuthorityManifest,
    AuthorityManifestV1, BrokerDecisionV1, BrokerError, CapabilityBinding, CapabilityBindingV1,
    CommittedWorkerResultPortV1, DeliveryAcceptanceV1, DurableInvocationBroker,
    InvocationLedgerEventV1, InvocationLedgerPortV1, ProjectCoordinator,
    WorkerInvocationProposalV1, WorkerResultOutboxV1, WorkspaceBindingV1,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{
    PROJECT_FILE_READ_MAXIMUM_BYTES_V1, PROJECT_FILE_SEARCH_MAXIMUM_RESULTS_V1,
    model_tool_loop::{ModelToolInvocationPortV1, SettledModelToolCallV1},
    pipeline::{CoreAuthenticationKey, LocalInvocationLedger, SimpleChatPipelineError},
    project_scope::revalidate_git_branch,
};

pub(crate) const FILE_TOOL_ADAPTER_VERSION: &str = "1.0.0";
pub(crate) const FILE_READ_CAPABILITY_ID: &str = "tool.files.read";
pub(crate) const FILE_SEARCH_CAPABILITY_ID: &str = "tool.files.search";
const FILE_READ_PROVIDER_NAME: &str = "aworkit_read_project_file";
const FILE_SEARCH_PROVIDER_NAME: &str = "aworkit_search_project_file";
const FILE_READ_ADAPTER_ID: &str = "adapter.project-files.read";
const FILE_SEARCH_ADAPTER_ID: &str = "adapter.project-files.search";
const FILE_READ_SCOPE: &str = "project.read";
const FILE_SEARCH_SCOPE: &str = "project.search";
const TOOL_RECORD_CHAT_ID: &str = "pipeline.tool-invocations";
const TOOL_BROKER_CHAT_ID: &str = "broker.tool-invocations";
const TOOL_HOST_DESTINATION: &str = "aworkit.capability-host.tools";
const TOOL_WORKER_DESTINATION: &str = "aworkit.workflow-worker.tools";
const STORE_BRANCH_ID: &str = "main";
const TOOL_NODE_TYPE: &str = "agent";
const TOOL_APPROVAL_TTL_MILLIS: u64 = 60_000;
const MAXIMUM_TOOL_PAYLOAD_BYTES: usize = 256 * 1024;
const MAXIMUM_TOOL_RESULT_BYTES: usize = 512 * 1024;
const MAXIMUM_FILE_SEARCH_QUERY_BYTES: usize = 16 * 1024;
const MAXIMUM_ACTIVITY_TEXT_BYTES: usize = 512;

/// Secret-free tool Settings frozen for one Chat/Run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SimpleChatToolBindingV1 {
    pub capability_id: String,
    pub configuration: Value,
}

/// Durable, UI-safe evidence for one authority-settled provider tool call.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SimpleChatToolActivityV1 {
    pub call_id: String,
    pub invocation_id: StableId,
    pub capability_id: String,
    pub path: String,
    pub status: String,
    pub summary: String,
    pub outcome_hash: String,
    pub replayed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StoredFileToolBindingV1 {
    pub capability_id: String,
    pub provider_name: String,
    pub description: String,
    pub input_schema: Value,
    pub configuration: Value,
    pub configuration_hash: String,
    pub limit: StoredFileToolLimitV1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum StoredFileToolLimitV1 {
    Read { maximum_bytes: usize },
    Search { maximum_results: usize },
}

impl StoredFileToolBindingV1 {
    pub(crate) fn definition(&self) -> ModelToolDefinitionV1 {
        ModelToolDefinitionV1 {
            capability_id: self.capability_id.clone(),
            name: self.provider_name.clone(),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
        }
    }
}

/// Builds the two attested built-in descriptors registered for this desktop
/// generation. No write/process capability is installed by this loop.
pub(crate) fn file_tool_descriptors()
-> Result<BTreeMap<String, CapabilityDescriptor>, SimpleChatPipelineError> {
    let mut descriptors = BTreeMap::new();
    for capability_id in [FILE_READ_CAPABILITY_ID, FILE_SEARCH_CAPABILITY_ID] {
        let (kind, scope, schema) = match capability_id {
            FILE_READ_CAPABILITY_ID => (
                CapabilityKind::FileRead,
                FILE_READ_SCOPE,
                file_read_schema(),
            ),
            FILE_SEARCH_CAPABILITY_ID => (
                CapabilityKind::FileSearch,
                FILE_SEARCH_SCOPE,
                file_search_schema(),
            ),
            _ => unreachable!("fixed capability list"),
        };
        let mut descriptor = CapabilityDescriptor::build(
            capability_id,
            FILE_TOOL_ADAPTER_VERSION,
            kind,
            SideEffectClass::ReadOnly,
        )
        .map_err(|error| SimpleChatPipelineError::Host(error.to_string()))?;
        descriptor.guarantees_same_id_deduplication = false;
        descriptor.supports_cancellation = true;
        descriptor.allowed_scopes = vec![scope.to_owned()];
        descriptor.requires_workspace = true;
        descriptor.maximum_concurrency = 8;
        descriptor.max_input_bytes = MAXIMUM_TOOL_PAYLOAD_BYTES;
        descriptor.max_output_bytes = MAXIMUM_TOOL_RESULT_BYTES;
        descriptor.input_schema_hash = Some(canonical_hash(&schema)?);
        descriptor
            .rehash()
            .map_err(|error| SimpleChatPipelineError::Host(error.to_string()))?;
        descriptors.insert(capability_id.to_owned(), descriptor);
    }
    Ok(descriptors)
}

/// Validates and freezes the exact authority-relevant subset of tool Settings.
pub(crate) fn freeze_file_tool_bindings(
    requested: &[SimpleChatToolBindingV1],
) -> Result<Vec<StoredFileToolBindingV1>, SimpleChatPipelineError> {
    let mut seen = BTreeSet::new();
    let mut bindings = Vec::with_capacity(requested.len());
    for requested in requested {
        if !seen.insert(requested.capability_id.as_str()) {
            return Err(invalid_tool("duplicate tool binding"));
        }
        let (provider_name, description, input_schema, limit) = match requested
            .capability_id
            .as_str()
        {
            FILE_READ_CAPABILITY_ID => (
                FILE_READ_PROVIDER_NAME,
                "Read one UTF-8 text file relative to the frozen project root.",
                file_read_schema(),
                StoredFileToolLimitV1::Read {
                    maximum_bytes: exact_unsigned_configuration(
                        &requested.configuration,
                        &[
                            ("authorityMode", Value::String("project_files".into())),
                            ("effect", Value::String("read".into())),
                        ],
                        "maximumBytes",
                        1,
                        PROJECT_FILE_READ_MAXIMUM_BYTES_V1,
                    )?,
                },
            ),
            FILE_SEARCH_CAPABILITY_ID => (
                FILE_SEARCH_PROVIDER_NAME,
                "Find exact UTF-8 text matches in one file relative to the frozen project root.",
                file_search_schema(),
                StoredFileToolLimitV1::Search {
                    maximum_results: exact_unsigned_configuration(
                        &requested.configuration,
                        &[
                            ("authorityMode", Value::String("project_files".into())),
                            ("effect", Value::String("search".into())),
                        ],
                        "maximumResults",
                        1,
                        PROJECT_FILE_SEARCH_MAXIMUM_RESULTS_V1,
                    )?,
                },
            ),
            "tool.files.edit" | "tool.shell.host" | "tool.python.host" => {
                return Err(invalid_tool(
                    "write, shell, and Python tools require a user approval path that Simple Chat does not yet expose",
                ));
            }
            _ => return Err(invalid_tool("tool binding has no installed native adapter")),
        };
        bindings.push(StoredFileToolBindingV1 {
            capability_id: requested.capability_id.clone(),
            provider_name: provider_name.to_owned(),
            description: description.to_owned(),
            input_schema,
            configuration_hash: canonical_hash(&requested.configuration)?,
            configuration: requested.configuration.clone(),
            limit,
        });
    }
    Ok(bindings)
}

pub(crate) fn file_tool_capability_binding(
    tool: &StoredFileToolBindingV1,
    descriptor: &CapabilityDescriptor,
) -> Result<CapabilityBindingV1, SimpleChatPipelineError> {
    if descriptor.capability_id != tool.capability_id {
        return Err(SimpleChatPipelineError::IncompleteEvidence);
    }
    Ok(CapabilityBindingV1 {
        capability_id: stable(&tool.capability_id)?,
        adapter_id: stable(match tool.capability_id.as_str() {
            FILE_READ_CAPABILITY_ID => FILE_READ_ADAPTER_ID,
            FILE_SEARCH_CAPABILITY_ID => FILE_SEARCH_ADAPTER_ID,
            _ => return Err(SimpleChatPipelineError::IncompleteEvidence),
        })?,
        adapter_version: descriptor.version.clone(),
        descriptor_hash: descriptor.version_hash.clone(),
        extension: None,
        required_isolation_profile: descriptor.required_isolation.clone(),
        enabled: true,
        compatible: true,
        approval: ApprovalRequirement::Never,
        allowed_node_types: vec![TOOL_NODE_TYPE.to_owned()],
    })
}

#[derive(Clone)]
pub(crate) struct FileToolAuthorityRuntimeV1 {
    projects: ProjectCoordinator,
    records: Arc<ToolRecordStore>,
    ledger: Arc<LocalInvocationLedger>,
    host: Arc<CapabilityHost>,
    descriptors: BTreeMap<String, CapabilityDescriptor>,
    generation: ProcessGeneration,
    core_key: Arc<CoreAuthenticationKey>,
}

impl FileToolAuthorityRuntimeV1 {
    pub(crate) fn open(
        database: &Path,
        projects: ProjectCoordinator,
        host: Arc<CapabilityHost>,
        descriptors: BTreeMap<String, CapabilityDescriptor>,
        generation: ProcessGeneration,
        core_key: Arc<CoreAuthenticationKey>,
    ) -> Result<Self, SimpleChatPipelineError> {
        Ok(Self {
            projects,
            records: Arc::new(ToolRecordStore::open(database)?),
            ledger: Arc::new(LocalInvocationLedger::open_scoped(
                database,
                TOOL_BROKER_CHAT_ID,
                TOOL_HOST_DESTINATION,
                TOOL_WORKER_DESTINATION,
            )?),
            host,
            descriptors,
            generation,
            core_key,
        })
    }

    pub(crate) fn bind(
        &self,
        context: FrozenFileToolAuthorityContextV1,
    ) -> BoundFileToolAuthorityV1 {
        BoundFileToolAuthorityV1 {
            runtime: self.clone(),
            context,
        }
    }
}

#[derive(Clone)]
pub(crate) struct FrozenFileToolAuthorityContextV1 {
    pub manifest: AuthorityManifestV1,
    pub run_id: StableId,
    pub node_id: StableId,
    pub workspace: WorkspaceBindingV1,
    pub project_branch: Option<String>,
    pub bindings: Vec<StoredFileToolBindingV1>,
    pub deadline_epoch_millis: u64,
}

pub(crate) struct BoundFileToolAuthorityV1 {
    runtime: FileToolAuthorityRuntimeV1,
    context: FrozenFileToolAuthorityContextV1,
}

impl ModelToolInvocationPortV1 for BoundFileToolAuthorityV1 {
    fn invoke(
        &self,
        outer_invocation_id: &StableId,
        turn: u32,
        call: &ModelToolCallV1,
        cancellation: &CancellationToken,
    ) -> Result<SettledModelToolCallV1, String> {
        self.invoke_v1(outer_invocation_id, turn, call, cancellation)
            .map_err(|error| error.to_string())
    }

    fn commit_exchange(
        &self,
        outer_invocation_id: &StableId,
        turn: u32,
        exchange: &ModelToolExchangeV1,
    ) -> Result<(), String> {
        self.runtime
            .records
            .record_exchange(outer_invocation_id, turn, exchange)
            .map_err(|error| error.to_string())
    }
}

impl BoundFileToolAuthorityV1 {
    fn invoke_v1(
        &self,
        outer_invocation_id: &StableId,
        turn: u32,
        call: &ModelToolCallV1,
        cancellation: &CancellationToken,
    ) -> Result<SettledModelToolCallV1, SimpleChatPipelineError> {
        if cancellation.is_cancelled() {
            return Err(SimpleChatPipelineError::Host(
                "Agent tool loop was cancelled".into(),
            ));
        }
        self.runtime
            .projects
            .revalidate_workspace_v1(&self.context.workspace)
            .map_err(|error| SimpleChatPipelineError::Authority(error.to_string()))?;
        revalidate_optional_branch(
            &self.context.workspace,
            self.context.project_branch.as_deref(),
        )?;
        let binding = self
            .context
            .bindings
            .iter()
            .find(|binding| {
                binding.capability_id == call.capability_id && binding.provider_name == call.name
            })
            .ok_or_else(|| invalid_tool("provider requested an unbound tool"))?;
        validate_call_arguments(binding, &call.arguments)?;
        let manifest_binding = self
            .context
            .manifest
            .capability_bindings
            .iter()
            .find(|candidate| candidate.capability_id.as_str() == binding.capability_id)
            .cloned()
            .ok_or(SimpleChatPipelineError::AuthorityDenied)?;
        if !manifest_binding.enabled || !manifest_binding.compatible {
            return Err(SimpleChatPipelineError::AuthorityDenied);
        }
        let record = self.prepare_invocation_record(
            outer_invocation_id,
            turn,
            call,
            binding,
            manifest_binding,
        )?;
        let proposal_id = record.proposal.proposal_id.clone();
        let proposal = record.proposal.clone();
        let replayed = self.runtime.records.record_invocation(&record)?;
        let broker =
            DurableInvocationBroker::new(self.runtime.ledger.clone(), TOOL_APPROVAL_TTL_MILLIS);
        let decision = broker
            .propose(
                &legacy_manifest(&self.context.manifest),
                proposal,
                current_epoch_millis(),
            )
            .map_err(broker_error)?;
        let invocation_id = match decision {
            BrokerDecisionV1::DispatchReady(dispatch) => dispatch.invocation_id,
            BrokerDecisionV1::AlreadySettled(_) => self
                .runtime
                .ledger
                .invocation_for_proposal(&proposal_id)?
                .ok_or(SimpleChatPipelineError::IncompleteEvidence)?,
            BrokerDecisionV1::Denied => return Err(SimpleChatPipelineError::AuthorityDenied),
            BrokerDecisionV1::AwaitingApproval(_) => {
                return Err(SimpleChatPipelineError::ApprovalRequired);
            }
        };
        self.reconcile_outcome(&broker, &invocation_id)?;
        if self.runtime.ledger.settlement(&invocation_id)?.is_none() {
            let host = FileToolHostPortV1 {
                runtime: self.runtime.clone(),
            };
            let delivery = broker.deliver_dispatches(&host);
            self.reconcile_outcome(&broker, &invocation_id)?;
            if self.runtime.ledger.settlement(&invocation_id)?.is_none() {
                return Err(broker_error(
                    delivery.err().unwrap_or(BrokerError::IncompleteState),
                ));
            }
        }
        let (outcome_hash, uncertain) = self
            .runtime
            .ledger
            .settlement(&invocation_id)?
            .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
        if uncertain {
            return Err(SimpleChatPipelineError::Host(
                "project-file tool outcome is uncertain; automatic replay is forbidden".into(),
            ));
        }
        let outcome = self
            .runtime
            .records
            .outcome(&invocation_id)?
            .filter(|outcome| canonical_hash(outcome).ok().as_deref() == Some(&outcome_hash))
            .ok_or(SimpleChatPipelineError::IncompleteEvidence)?;
        let _ = broker.deliver_worker_results(&CommittedToolResultAckV1);
        Ok(SettledModelToolCallV1 {
            result: ModelToolResultV1 {
                call_id: call.call_id.clone(),
                content: outcome.result.clone(),
                is_error: outcome.is_error,
            },
            activity: SimpleChatToolActivityV1 {
                call_id: call.call_id.clone(),
                invocation_id,
                capability_id: call.capability_id.clone(),
                path: bounded_activity_text(outcome.path),
                status: if outcome.is_error {
                    "failed"
                } else {
                    "completed"
                }
                .into(),
                summary: bounded_activity_text(outcome.summary),
                outcome_hash,
                replayed,
            },
        })
    }

    fn prepare_invocation_record(
        &self,
        outer_invocation_id: &StableId,
        turn: u32,
        call: &ModelToolCallV1,
        binding: &StoredFileToolBindingV1,
        manifest_binding: CapabilityBindingV1,
    ) -> Result<ToolInvocationRecordV1, SimpleChatPipelineError> {
        let payload = json!({
            "arguments": call.arguments,
            "configurationHash": binding.configuration_hash,
            "workspaceIdentity": self.context.workspace.identity,
            "projectBranch": self.context.project_branch,
        });
        let proposal_id = digest_id(
            "proposal.tool",
            &format!(
                "{}:{turn}:{}:{}",
                outer_invocation_id.as_str(),
                call.call_id,
                canonical_hash(&payload)?
            ),
        )?;
        Ok(ToolInvocationRecordV1 {
            schema_version: 1,
            outer_invocation_id: outer_invocation_id.clone(),
            turn,
            call: call.clone(),
            proposal: WorkerInvocationProposalV1 {
                proposal_id,
                run_id: self.context.run_id.clone(),
                node_id: self.context.node_id.clone(),
                attempt: 1,
                capability_id: stable(&binding.capability_id)?,
                payload_hash: canonical_hash(&payload)?,
            },
            payload,
            authority_manifest_id: self.context.manifest.manifest_id.clone(),
            manifest_binding,
            workspace: self.context.workspace.clone(),
            project_branch: self.context.project_branch.clone(),
            binding: binding.clone(),
            deadline_epoch_millis: self.context.deadline_epoch_millis,
        })
    }

    fn reconcile_outcome(
        &self,
        broker: &DurableInvocationBroker,
        invocation_id: &StableId,
    ) -> Result<(), SimpleChatPipelineError> {
        let Some(outcome) = self.runtime.records.outcome(invocation_id)? else {
            return Ok(());
        };
        let events = self
            .runtime
            .ledger
            .events(invocation_id)
            .map_err(broker_error)?;
        if events
            .iter()
            .any(|event| matches!(event, InvocationLedgerEventV1::Settled { .. }))
        {
            return Ok(());
        }
        let attempted = events
            .iter()
            .any(|event| matches!(event, InvocationLedgerEventV1::DispatchAttempted { .. }));
        if !attempted {
            return Err(SimpleChatPipelineError::IncompleteEvidence);
        }
        if !events
            .iter()
            .any(|event| matches!(event, InvocationLedgerEventV1::DispatchAccepted { .. }))
        {
            broker
                .accept_dispatch(invocation_id)
                .map_err(broker_error)?;
        }
        if let Some(outbox) = self
            .runtime
            .ledger
            .pending_dispatches()
            .map_err(broker_error)?
            .into_iter()
            .find(|entry| entry.dispatch.invocation_id == *invocation_id)
        {
            broker
                .mark_dispatch_delivered(&outbox.outbox_id)
                .map_err(broker_error)?;
        }
        broker
            .settle(invocation_id, canonical_hash(&outcome)?, false)
            .map_err(broker_error)
    }
}

struct FileToolHostPortV1 {
    runtime: FileToolAuthorityRuntimeV1,
}

impl ApprovedHostDispatchPortV1 for FileToolHostPortV1 {
    fn dispatch(&self, dispatch: &ApprovedDispatchV1) -> Result<DeliveryAcceptanceV1, BrokerError> {
        let record = match self.runtime.records.invocation_for_dispatch(dispatch) {
            Ok(Some(record)) => record,
            Ok(None) => return Ok(DeliveryAcceptanceV1::RejectedDefinitelyNotStarted),
            Err(_) => return Ok(DeliveryAcceptanceV1::Ambiguous),
        };
        let descriptor = self
            .runtime
            .descriptors
            .get(record.call.capability_id.as_str())
            .ok_or(BrokerError::IdentityConflict)?;
        if record.authority_manifest_id != dispatch.manifest_id
            || dispatch.payload_hash != record.proposal.payload_hash
            || dispatch.capability_id != record.proposal.capability_id
            || canonical_hash(&record.payload).ok().as_deref()
                != Some(dispatch.payload_hash.as_str())
            || record.manifest_binding.capability_id != dispatch.capability_id
            || record.manifest_binding.adapter_version != descriptor.version
            || record.manifest_binding.descriptor_hash != descriptor.version_hash
            || !dispatch.lease_ids.is_empty()
        {
            return Err(BrokerError::IdentityConflict);
        }
        if self
            .runtime
            .projects
            .revalidate_workspace_v1(&record.workspace)
            .is_err()
            || revalidate_optional_branch(&record.workspace, record.project_branch.as_deref())
                .is_err()
        {
            return Ok(DeliveryAcceptanceV1::RejectedDefinitelyNotStarted);
        }
        let mut envelope = ApprovedInvocationEnvelopeV1 {
            schema_version: SchemaVersion::V1,
            invocation_id: dispatch.invocation_id.clone(),
            decision_id: dispatch.invocation_id.clone(),
            host_generation: self.runtime.generation,
            capability_id: descriptor.capability_id.clone(),
            adapter_version: descriptor.version.clone(),
            binding_hash: descriptor.version_hash.clone(),
            extension: None,
            required_isolation_profile: descriptor.required_isolation.clone(),
            kind: descriptor.kind,
            enforced_scopes: vec![scope_for(&descriptor.capability_id).to_owned()],
            deadline_epoch_millis: record.deadline_epoch_millis,
            cancellation_token: digest_id("cancel.tool", dispatch.invocation_id.as_str())
                .map_err(|_| BrokerError::Unavailable)?,
            lease_handles: Vec::new(),
            max_output_bytes: descriptor.max_output_bytes,
            payload: record.payload.clone(),
            core_authentication_tag: String::new(),
        };
        envelope
            .sign(self.runtime.core_key.as_slice())
            .map_err(|_| BrokerError::Unavailable)?;
        let dispatcher = FileToolDispatcherV1 {
            projects: self.runtime.projects.clone(),
            records: self.runtime.records.clone(),
            record,
        };
        match self
            .runtime
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
                    .runtime
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

struct FileToolDispatcherV1 {
    projects: ProjectCoordinator,
    records: Arc<ToolRecordStore>,
    record: ToolInvocationRecordV1,
}

impl AdmittedInvocationDispatcherV1 for FileToolDispatcherV1 {
    type Output = Result<ToolOutcomeRecordV1, SimpleChatPipelineError>;

    fn dispatch(
        &self,
        envelope: &ApprovedInvocationEnvelopeV1,
        _admission: &AdmissionReceipt,
        cancellation: &CancellationToken,
    ) -> Self::Output {
        let outcome = self.execute(envelope, cancellation);
        self.records.record_outcome(&outcome)?;
        Ok(outcome)
    }
}

impl FileToolDispatcherV1 {
    fn execute(
        &self,
        envelope: &ApprovedInvocationEnvelopeV1,
        cancellation: &CancellationToken,
    ) -> ToolOutcomeRecordV1 {
        let path = self
            .record
            .call
            .arguments
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("invalid")
            .to_owned();
        let result: Result<(Value, String), String> = (|| {
            self.projects
                .revalidate_workspace_v1(&self.record.workspace)
                .map_err(|error| error.to_string())?;
            revalidate_optional_branch(
                &self.record.workspace,
                self.record.project_branch.as_deref(),
            )
            .map_err(|error| error.to_string())?;
            let files = ProjectFiles::new(FileAuthority {
                root: self.record.workspace.root.clone(),
                allow_write: false,
            })
            .map_err(|error| error.to_string())?;
            self.projects
                .revalidate_workspace_v1(&self.record.workspace)
                .map_err(|error| error.to_string())?;
            revalidate_optional_branch(
                &self.record.workspace,
                self.record.project_branch.as_deref(),
            )
            .map_err(|error| error.to_string())?;
            match self.record.binding.limit {
                StoredFileToolLimitV1::Read { maximum_bytes } => {
                    let read = files
                        .read_v1(
                            &FileReadRequestV1 {
                                path: PathBuf::from(&path),
                                maximum_bytes,
                            },
                            cancellation,
                        )
                        .map_err(|error| error.to_string())?;
                    let content = String::from_utf8(read.bytes)
                        .map_err(|_| "file is not UTF-8 text".to_owned())?;
                    let value = json!({
                        "path": path,
                        "content": content,
                        "contentHash": read.content_hash,
                        "bytes": read.effect.bytes_observed_or_written,
                    });
                    enforce_result_bound(&value)?;
                    Ok((
                        value,
                        format!(
                            "Read {} bytes from {}.",
                            read.effect.bytes_observed_or_written,
                            read.effect.relative_path.display()
                        ),
                    ))
                }
                StoredFileToolLimitV1::Search { maximum_results } => {
                    let needle = self.record.call.arguments["query"]
                        .as_str()
                        .ok_or_else(|| "search query is invalid".to_owned())?;
                    let search = files
                        .search_v1(
                            &FileSearchRequestV1 {
                                path: PathBuf::from(&path),
                                needle: needle.to_owned(),
                                maximum_results,
                            },
                            cancellation,
                        )
                        .map_err(|error| error.to_string())?;
                    let match_count = search.offsets.len();
                    let value = json!({
                        "path": path,
                        "query": needle,
                        "offsets": search.offsets,
                        "contentHash": search.effect.before_content_hash,
                        "bytesObserved": search.effect.bytes_observed_or_written,
                    });
                    enforce_result_bound(&value)?;
                    Ok((
                        value,
                        format!(
                            "Found {} match(es) in {}.",
                            match_count,
                            search.effect.relative_path.display()
                        ),
                    ))
                }
            }
        })();
        match result {
            Ok((result, summary)) => ToolOutcomeRecordV1 {
                schema_version: 1,
                invocation_id: envelope.invocation_id.clone(),
                call_id: self.record.call.call_id.clone(),
                capability_id: self.record.call.capability_id.clone(),
                path,
                result,
                is_error: false,
                summary,
            },
            Err(error) => ToolOutcomeRecordV1 {
                schema_version: 1,
                invocation_id: envelope.invocation_id.clone(),
                call_id: self.record.call.call_id.clone(),
                capability_id: self.record.call.capability_id.clone(),
                path,
                result: json!({"error": error}),
                is_error: true,
                summary: "Project-file operation was denied or failed within its frozen root."
                    .into(),
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ToolInvocationRecordV1 {
    schema_version: u16,
    outer_invocation_id: StableId,
    turn: u32,
    call: ModelToolCallV1,
    proposal: WorkerInvocationProposalV1,
    payload: Value,
    authority_manifest_id: StableId,
    manifest_binding: CapabilityBindingV1,
    workspace: WorkspaceBindingV1,
    #[serde(default)]
    project_branch: Option<String>,
    binding: StoredFileToolBindingV1,
    deadline_epoch_millis: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ToolOutcomeRecordV1 {
    schema_version: u16,
    invocation_id: StableId,
    call_id: String,
    capability_id: String,
    path: String,
    result: Value,
    is_error: bool,
    summary: String,
}

#[derive(Clone)]
struct ToolRecordStore {
    store: LocalHistoryStore,
    write_lock: Arc<Mutex<()>>,
}

impl ToolRecordStore {
    fn open(path: &Path) -> Result<Self, SimpleChatPipelineError> {
        Ok(Self {
            store: LocalHistoryStore::open(path).map_err(local_store_error)?,
            write_lock: Arc::new(Mutex::new(())),
        })
    }

    fn record_invocation(
        &self,
        record: &ToolInvocationRecordV1,
    ) -> Result<bool, SimpleChatPipelineError> {
        if let Some(existing) = self.invocation(&record.proposal.proposal_id)? {
            return if existing == *record {
                Ok(true)
            } else {
                Err(SimpleChatPipelineError::Store(
                    "tool proposal identity was reused with changed frozen authority".into(),
                ))
            };
        }
        self.append(
            "pipeline.tool-invocation-prepared",
            &record.proposal.proposal_id,
            serde_json::to_value(record).map_err(json_error)?,
        )?;
        Ok(false)
    }

    fn record_outcome(
        &self,
        outcome: &ToolOutcomeRecordV1,
    ) -> Result<bool, SimpleChatPipelineError> {
        if let Some(existing) = self.outcome(&outcome.invocation_id)? {
            return if existing == *outcome {
                Ok(true)
            } else {
                Err(SimpleChatPipelineError::Store(
                    "tool outcome identity was reused with changed evidence".into(),
                ))
            };
        }
        self.append(
            "pipeline.tool-outcome",
            &digest_id("record.tool-outcome", outcome.invocation_id.as_str())?,
            serde_json::to_value(outcome).map_err(json_error)?,
        )?;
        Ok(false)
    }

    fn record_exchange(
        &self,
        outer_invocation_id: &StableId,
        turn: u32,
        exchange: &ModelToolExchangeV1,
    ) -> Result<(), SimpleChatPipelineError> {
        let key = digest_id(
            "record.model-tool-exchange",
            &format!("{}:{turn}", outer_invocation_id.as_str()),
        )?;
        let value = json!({
            "schemaVersion": 1,
            "outerInvocationId": outer_invocation_id,
            "turn": turn,
            "exchange": exchange,
        });
        if let Some(existing) = self
            .events("pipeline.model-tool-exchange")?
            .into_iter()
            .find(|candidate| {
                candidate.get("outerInvocationId").and_then(Value::as_str)
                    == Some(outer_invocation_id.as_str())
                    && candidate.get("turn").and_then(Value::as_u64) == Some(u64::from(turn))
            })
        {
            return if existing == value {
                Ok(())
            } else {
                Err(SimpleChatPipelineError::Store(
                    "model/tool exchange identity was reused with changed provider context".into(),
                ))
            };
        }
        self.append("pipeline.model-tool-exchange", &key, value)
    }

    fn invocation(
        &self,
        proposal_id: &StableId,
    ) -> Result<Option<ToolInvocationRecordV1>, SimpleChatPipelineError> {
        self.events("pipeline.tool-invocation-prepared")?
            .into_iter()
            .map(|value| serde_json::from_value(value).map_err(json_error))
            .collect::<Result<Vec<ToolInvocationRecordV1>, _>>()
            .map(|records| {
                records
                    .into_iter()
                    .find(|record| &record.proposal.proposal_id == proposal_id)
            })
    }

    fn invocation_for_dispatch(
        &self,
        dispatch: &ApprovedDispatchV1,
    ) -> Result<Option<ToolInvocationRecordV1>, SimpleChatPipelineError> {
        self.invocation(&dispatch.proposal_id)
    }

    fn outcome(
        &self,
        invocation_id: &StableId,
    ) -> Result<Option<ToolOutcomeRecordV1>, SimpleChatPipelineError> {
        self.events("pipeline.tool-outcome")?
            .into_iter()
            .map(|value| serde_json::from_value(value).map_err(json_error))
            .collect::<Result<Vec<ToolOutcomeRecordV1>, _>>()
            .map(|records| {
                records
                    .into_iter()
                    .find(|record| &record.invocation_id == invocation_id)
            })
    }

    fn events(&self, kind: &str) -> Result<Vec<Value>, SimpleChatPipelineError> {
        Ok(self
            .store
            .events(TOOL_RECORD_CHAT_ID, STORE_BRANCH_ID)
            .map_err(local_store_error)?
            .into_iter()
            .filter(|event| event.kind == kind)
            .filter_map(|event| event.payload.get("record").cloned())
            .collect())
    }

    fn append(
        &self,
        kind: &str,
        key: &StableId,
        record: Value,
    ) -> Result<(), SimpleChatPipelineError> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| SimpleChatPipelineError::Store("tool record lock poisoned".into()))?;
        let head = self
            .store
            .events(TOOL_RECORD_CHAT_ID, STORE_BRANCH_ID)
            .map_err(local_store_error)?
            .len();
        let expected_head = u64::try_from(head)
            .map_err(|_| SimpleChatPipelineError::Store("tool record sequence exhausted".into()))?;
        self.store
            .commit(&CommitBatch {
                chat_id: TOOL_RECORD_CHAT_ID.into(),
                branch_id: STORE_BRANCH_ID.into(),
                expected_head,
                events: vec![Event {
                    event_id: digest_id(
                        "record.tool-event",
                        &format!("{kind}:{}", canonical_hash(&record)?),
                    )?
                    .to_string(),
                    kind: kind.into(),
                    payload: json!({"schemaVersion":1,"record":record}),
                }],
                attempt: None,
                checkpoint: None,
                deduplication: Some(Deduplication {
                    key_type: kind.into(),
                    key: key.to_string(),
                    request_hash: String::new(),
                }),
                outbox: Vec::new(),
            })
            .map_err(local_store_error)?;
        Ok(())
    }
}

struct CommittedToolResultAckV1;

impl CommittedWorkerResultPortV1 for CommittedToolResultAckV1 {
    fn deliver(&self, _result: &WorkerResultOutboxV1) -> Result<DeliveryAcceptanceV1, BrokerError> {
        Ok(DeliveryAcceptanceV1::Accepted)
    }
}

fn legacy_manifest(manifest: &AuthorityManifestV1) -> AuthorityManifest {
    AuthorityManifest {
        manifest_id: manifest.manifest_id.clone(),
        capability_bindings: manifest
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
        summary: manifest.summary.clone(),
    }
}

fn validate_call_arguments(
    binding: &StoredFileToolBindingV1,
    arguments: &Value,
) -> Result<(), SimpleChatPipelineError> {
    let object = arguments
        .as_object()
        .ok_or_else(|| invalid_tool("tool arguments must be an object"))?;
    let expected_keys: BTreeSet<&str> = match binding.limit {
        StoredFileToolLimitV1::Read { .. } => BTreeSet::from(["path"]),
        StoredFileToolLimitV1::Search { .. } => BTreeSet::from(["path", "query"]),
    };
    let observed_keys = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if observed_keys != expected_keys {
        return Err(invalid_tool(
            "tool arguments contain missing or unknown fields",
        ));
    }
    let path = object
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty() && path.len() <= 4096 && !path.contains('\0'))
        .ok_or_else(|| invalid_tool("tool path is empty, oversized, or malformed"))?;
    if Path::new(path).is_absolute() {
        return Err(invalid_tool(
            "tool path must be relative to the frozen project root",
        ));
    }
    if matches!(binding.limit, StoredFileToolLimitV1::Search { .. }) {
        object
            .get("query")
            .and_then(Value::as_str)
            .filter(|query| {
                !query.is_empty()
                    && query.len() <= MAXIMUM_FILE_SEARCH_QUERY_BYTES
                    && !query.contains('\0')
            })
            .ok_or_else(|| invalid_tool("search query is empty, oversized, or malformed"))?;
    }
    Ok(())
}

fn exact_unsigned_configuration(
    configuration: &Value,
    fixed: &[(&str, Value)],
    numeric_name: &str,
    minimum: u64,
    maximum: u64,
) -> Result<usize, SimpleChatPipelineError> {
    let object = configuration
        .as_object()
        .ok_or_else(|| invalid_tool("tool configuration must be an object"))?;
    if object.len() != fixed.len() + 1
        || fixed
            .iter()
            .any(|(name, expected)| object.get(*name) != Some(expected))
    {
        return Err(invalid_tool(
            "tool configuration does not match the installed native adapter contract",
        ));
    }
    let value = object
        .get(numeric_name)
        .and_then(Value::as_u64)
        .filter(|value| (minimum..=maximum).contains(value))
        .ok_or_else(|| invalid_tool("tool configuration limit is invalid"))?;
    usize::try_from(value).map_err(|_| invalid_tool("tool configuration limit is invalid"))
}

fn file_read_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": {"type":"string","minLength":1,"maxLength":4096}
        },
        "required": ["path"]
    })
}

fn file_search_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": {"type":"string","minLength":1,"maxLength":4096},
            "query": {"type":"string","minLength":1,"maxLength":16384}
        },
        "required": ["path", "query"]
    })
}

fn enforce_result_bound(value: &Value) -> Result<(), String> {
    if serde_json::to_vec(value).map_or(true, |bytes| bytes.len() > MAXIMUM_TOOL_RESULT_BYTES) {
        Err("tool result exceeds the provider continuation bound".into())
    } else {
        Ok(())
    }
}

fn bounded_activity_text(mut value: String) -> String {
    if value.len() <= MAXIMUM_ACTIVITY_TEXT_BYTES {
        return value;
    }
    let mut end = MAXIMUM_ACTIVITY_TEXT_BYTES;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value.truncate(end);
    value.push('…');
    value
}

fn revalidate_optional_branch(
    workspace: &WorkspaceBindingV1,
    expected_branch: Option<&str>,
) -> Result<(), SimpleChatPipelineError> {
    expected_branch.map_or(Ok(()), |expected| {
        revalidate_git_branch(&workspace.root, expected).map_err(SimpleChatPipelineError::Authority)
    })
}

fn scope_for(capability_id: &str) -> &'static str {
    match capability_id {
        FILE_READ_CAPABILITY_ID => FILE_READ_SCOPE,
        FILE_SEARCH_CAPABILITY_ID => FILE_SEARCH_SCOPE,
        _ => "invalid",
    }
}

fn current_epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn canonical_hash<T: Serialize>(value: &T) -> Result<String, SimpleChatPipelineError> {
    let bytes = serde_jcs::to_vec(value).map_err(json_error)?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn digest_id(prefix: &str, material: &str) -> Result<StableId, SimpleChatPipelineError> {
    let digest = format!("{:x}", Sha256::digest(material.as_bytes()));
    stable(&format!("{prefix}.{}", &digest[..40]))
}

fn stable(value: &str) -> Result<StableId, SimpleChatPipelineError> {
    StableId::parse(value.to_owned())
        .map_err(|error| SimpleChatPipelineError::InvalidInput(error.to_string()))
}

fn invalid_tool(message: &str) -> SimpleChatPipelineError {
    SimpleChatPipelineError::InvalidInput(message.to_owned())
}

fn broker_error(error: BrokerError) -> SimpleChatPipelineError {
    SimpleChatPipelineError::Broker(error.to_string())
}

fn local_store_error(error: StoreError) -> SimpleChatPipelineError {
    SimpleChatPipelineError::Store(error.to_string())
}

fn json_error(error: impl std::fmt::Display) -> SimpleChatPipelineError {
    SimpleChatPipelineError::Store(error.to_string())
}

#[cfg(test)]
mod tests {
    use aworkit_capability_host::{
        AdapterRegistry, ModelAssistantContentV1, ModelToolCallV1, ModelToolResultV1,
    };
    use aworkit_protocol::{AttestedExtensionSetV1, attested_extension_set_hash_v1};
    use tempfile::TempDir;

    use super::*;

    fn manifest(
        id: &str,
        binding: CapabilityBindingV1,
    ) -> Result<AuthorityManifestV1, SimpleChatPipelineError> {
        Ok(AuthorityManifestV1 {
            manifest_id: stable(id)?,
            manifest_hash: format!("sha256:{}", "1".repeat(64)),
            capability_bindings: vec![binding],
            summary: "test project-file authority".into(),
        })
    }

    fn read_call(call_id: &str, path: &str) -> ModelToolCallV1 {
        ModelToolCallV1 {
            call_id: call_id.into(),
            provider_call_id: Some(call_id.into()),
            capability_id: FILE_READ_CAPABILITY_ID.into(),
            name: FILE_READ_PROVIDER_NAME.into(),
            arguments: json!({"path":path}),
            provider_context: None,
        }
    }

    fn stage_pending(
        authority: &BoundFileToolAuthorityV1,
        outer_invocation_id: &StableId,
        call: &ModelToolCallV1,
    ) -> StableId {
        let binding = authority.context.bindings.first().expect("tool binding");
        let manifest_binding = authority
            .context
            .manifest
            .capability_bindings
            .iter()
            .find(|candidate| candidate.capability_id.as_str() == binding.capability_id)
            .cloned()
            .expect("manifest binding");
        let record = authority
            .prepare_invocation_record(outer_invocation_id, 1, call, binding, manifest_binding)
            .expect("invocation record");
        authority
            .runtime
            .records
            .record_invocation(&record)
            .expect("record pending invocation");
        let broker = DurableInvocationBroker::new(
            authority.runtime.ledger.clone(),
            TOOL_APPROVAL_TTL_MILLIS,
        );
        match broker
            .propose(
                &legacy_manifest(&authority.context.manifest),
                record.proposal,
                current_epoch_millis(),
            )
            .expect("authorize pending invocation")
        {
            BrokerDecisionV1::DispatchReady(dispatch) => dispatch.invocation_id,
            other => panic!("unexpected pending decision: {other:?}"),
        }
    }

    #[test]
    fn dispatch_drain_uses_each_pending_records_manifest() {
        let root = TempDir::new().expect("root");
        let workspace_a = root.path().join("workspace-a");
        let workspace_b = root.path().join("workspace-b");
        std::fs::create_dir_all(&workspace_a).expect("workspace A");
        std::fs::create_dir_all(&workspace_b).expect("workspace B");
        std::fs::write(workspace_a.join("notes.txt"), "first run").expect("file A");
        std::fs::write(workspace_b.join("notes.txt"), "second run").expect("file B");

        let projects = ProjectCoordinator::open(root.path().join("projects")).expect("projects");
        let descriptors = file_tool_descriptors().expect("descriptors");
        let mut registry = AdapterRegistry::default();
        for descriptor in descriptors.values() {
            registry
                .register_capability(descriptor.clone())
                .expect("register descriptor");
        }
        let generation = ProcessGeneration(19);
        let mut attested = AttestedExtensionSetV1 {
            host_id: stable("host.tool-manifest-test").expect("host ID"),
            host_generation: generation,
            host_protocol: 1,
            extensions: Vec::new(),
            set_hash: String::new(),
        };
        attested.set_hash = attested_extension_set_hash_v1(&attested).expect("attestation hash");
        let frozen = registry
            .materialize_attested_set(&attested)
            .expect("frozen registry");
        let core_key = Arc::new(CoreAuthenticationKey::random().expect("core key"));
        let host = Arc::new(
            CapabilityHost::from_attested_registry(frozen, core_key.copy(), 2)
                .expect("capability host"),
        );
        let runtime = FileToolAuthorityRuntimeV1::open(
            &root.path().join("tool-invocations.sqlite3"),
            projects.clone(),
            host,
            descriptors.clone(),
            generation,
            core_key,
        )
        .expect("tool authority");
        let tool = freeze_file_tool_bindings(&[SimpleChatToolBindingV1 {
            capability_id: FILE_READ_CAPABILITY_ID.into(),
            configuration: json!({
                "authorityMode":"project_files",
                "effect":"read",
                "maximumBytes":PROJECT_FILE_READ_MAXIMUM_BYTES_V1,
            }),
        }])
        .expect("frozen tool")
        .remove(0);
        let capability_binding = file_tool_capability_binding(
            &tool,
            descriptors
                .get(FILE_READ_CAPABILITY_ID)
                .expect("read descriptor"),
        )
        .expect("capability binding");
        let authority_a = runtime.bind(FrozenFileToolAuthorityContextV1 {
            manifest: manifest("manifest.tool-run-a", capability_binding.clone())
                .expect("manifest A"),
            run_id: stable("run.tool-run-a").expect("run A"),
            node_id: stable("agent.1").expect("node"),
            workspace: projects
                .resolve_workspace_v1(&workspace_a)
                .expect("binding A"),
            project_branch: None,
            bindings: vec![tool.clone()],
            deadline_epoch_millis: current_epoch_millis().saturating_add(60_000),
        });
        let authority_b = runtime.bind(FrozenFileToolAuthorityContextV1 {
            manifest: manifest("manifest.tool-run-b", capability_binding.clone())
                .expect("manifest B"),
            run_id: stable("run.tool-run-b").expect("run B"),
            node_id: stable("agent.1").expect("node"),
            workspace: projects
                .resolve_workspace_v1(&workspace_b)
                .expect("binding B"),
            project_branch: None,
            bindings: vec![tool.clone()],
            deadline_epoch_millis: current_epoch_millis().saturating_add(60_000),
        });
        let outer_a = stable("invocation.outer-tool-run-a").expect("outer A");
        let outer_b = stable("invocation.outer-tool-run-b").expect("outer B");
        let call_a = read_call("call.tool-run-a", "notes.txt");
        let call_b = read_call("call.tool-run-b", "notes.txt");

        let pending_a = stage_pending(&authority_a, &outer_a, &call_a);
        let settled_b = authority_b
            .invoke_v1(&outer_b, 1, &call_b, &CancellationToken::default())
            .expect("second Run drains both manifests");
        assert_eq!(settled_b.result.content["content"], "second run");
        assert!(
            runtime
                .records
                .outcome(&pending_a)
                .expect("stale pending outcome")
                .is_some()
        );

        let settled_a = authority_a
            .invoke_v1(&outer_a, 1, &call_a, &CancellationToken::default())
            .expect("first Run reconciles without replay");
        assert_eq!(settled_a.result.content["content"], "first run");
        assert!(settled_a.activity.replayed);
        assert!(
            [pending_a, settled_b.activity.invocation_id]
                .iter()
                .all(|invocation_id| runtime
                    .ledger
                    .settlement(invocation_id)
                    .expect("settlement")
                    .is_some_and(|(_, uncertain)| !uncertain))
        );

        let workspace_c = root.path().join("workspace-c");
        std::fs::create_dir_all(workspace_c.join(".git")).expect("workspace C Git metadata");
        std::fs::write(workspace_c.join("notes.txt"), "must not dispatch").expect("file C");
        std::fs::write(
            workspace_c.join(".git/HEAD"),
            b"ref: refs/heads/feature/frozen\n",
        )
        .expect("frozen HEAD");
        let authority_c = runtime.bind(FrozenFileToolAuthorityContextV1 {
            manifest: manifest("manifest.tool-run-c", capability_binding).expect("manifest C"),
            run_id: stable("run.tool-run-c").expect("run C"),
            node_id: stable("agent.1").expect("node"),
            workspace: projects
                .resolve_workspace_v1(&workspace_c)
                .expect("binding C"),
            project_branch: Some("feature/frozen".into()),
            bindings: vec![tool],
            deadline_epoch_millis: current_epoch_millis().saturating_add(60_000),
        });
        let outer_c = stable("invocation.outer-tool-run-c").expect("outer C");
        let call_c = read_call("call.tool-run-c", "notes.txt");
        let pending_c = stage_pending(&authority_c, &outer_c, &call_c);
        std::fs::write(
            workspace_c.join(".git/HEAD"),
            b"ref: refs/heads/feature/drifted\n",
        )
        .expect("branch switch");
        let broker = DurableInvocationBroker::new(runtime.ledger.clone(), TOOL_APPROVAL_TTL_MILLIS);
        let _ = broker.deliver_dispatches(&FileToolHostPortV1 {
            runtime: runtime.clone(),
        });
        assert!(
            runtime
                .ledger
                .settlement(&pending_c)
                .expect("branch-drift settlement")
                .is_some_and(|(_, uncertain)| !uncertain)
        );
        assert!(
            runtime
                .records
                .outcome(&pending_c)
                .expect("branch-drift outcome lookup")
                .is_none(),
            "host must reject branch drift before the file adapter executes"
        );
        assert!(
            authority_c
                .invoke_v1(
                    &stable("invocation.outer-tool-run-c-second").expect("outer C second"),
                    1,
                    &read_call("call.tool-run-c-second", "notes.txt"),
                    &CancellationToken::default(),
                )
                .unwrap_err()
                .to_string()
                .contains("Git HEAD drifted")
        );
    }

    #[test]
    fn search_contract_keeps_one_worst_case_exchange_persistence_safe() {
        let binding = freeze_file_tool_bindings(&[SimpleChatToolBindingV1 {
            capability_id: FILE_SEARCH_CAPABILITY_ID.into(),
            configuration: json!({
                "authorityMode":"project_files",
                "effect":"search",
                "maximumResults":PROJECT_FILE_SEARCH_MAXIMUM_RESULTS_V1,
            }),
        }])
        .expect("search binding")
        .remove(0);
        let path = "p".repeat(4096);
        let query = "\u{1}".repeat(MAXIMUM_FILE_SEARCH_QUERY_BYTES);
        let arguments = json!({"path":path,"query":query});
        validate_call_arguments(&binding, &arguments).expect("maximum search arguments");
        let call = ModelToolCallV1 {
            call_id: "call.search-bound".into(),
            provider_call_id: Some("call.search-bound".into()),
            capability_id: FILE_SEARCH_CAPABILITY_ID.into(),
            name: FILE_SEARCH_PROVIDER_NAME.into(),
            arguments: arguments.clone(),
            provider_context: None,
        };
        let exchange = ModelToolExchangeV1 {
            assistant_content: vec![ModelAssistantContentV1::ToolCall { call }],
            results: vec![ModelToolResultV1 {
                call_id: "call.search-bound".into(),
                content: json!({
                    "path":arguments["path"],
                    "query":arguments["query"],
                    "offsets":vec![1_048_575_u64; usize::try_from(PROJECT_FILE_SEARCH_MAXIMUM_RESULTS_V1).unwrap()],
                    "contentHash":format!("sha256:{}", "a".repeat(64)),
                    "bytesObserved":1_048_576,
                }),
                is_error: false,
            }],
        };
        assert!(serde_json::to_vec(&exchange).unwrap().len() <= 512 * 1024);

        let oversized = json!({
            "path":"notes.txt",
            "query":"x".repeat(MAXIMUM_FILE_SEARCH_QUERY_BYTES + 1),
        });
        assert!(validate_call_arguments(&binding, &oversized).is_err());
    }
}
