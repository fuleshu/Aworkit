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
    time::Duration,
};

use aworkit_capability_host::{
    AdmissionReceipt, AdmittedInvocationDispatcherV1, ApprovedInvocationEnvelopeV1,
    BuiltInProcessTools, CancellationToken, CapabilityDescriptor, CapabilityHost, CapabilityKind,
    FileAuthority, FileGrepRequestV1, FileListRequestV1, FileReadRequestV1, FileSearchRequestV1,
    FileWriteRequestV1, FrozenModelGateway, HostToolLimitsV1, McpCallKindV1, McpCallOutcomeV1,
    McpCallV1, McpServerManifestV1, ModelToolCallV1, ModelToolDefinitionV1, ModelToolExchangeV1,
    ModelToolResultV1, NativeProcessPort, OutcomeDispositionV1, ProjectFiles, PythonInvocationV1,
    ShellInvocationV1, SideEffectClass, ToolAuthorityModeV1, WebTools,
};
use aworkit_local_store::{CommitBatch, Deduplication, Event, LocalHistoryStore, StoreError};
use aworkit_protocol::{ProcessGeneration, SchemaVersion, StableId};
use aworkit_trusted_core::{
    ApprovalChallengeV1, ApprovalRequirement, ApprovalResponseV1, ApprovedDispatchV1,
    ApprovedHostDispatchPortV1, AuthorityManifest, AuthorityManifestV1, BrokerDecisionV1,
    BrokerError, CapabilityBinding, CapabilityBindingV1, CommittedWorkerResultPortV1,
    DeliveryAcceptanceV1, DurableInvocationBroker, InvocationLedgerEventV1, InvocationLedgerPortV1,
    ProjectCoordinator, WorkerInvocationProposalV1, WorkerResultOutboxV1, WorkspaceBindingV1,
    is_definitely_not_started_settlement_v1,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{
    PROJECT_FILE_READ_MAXIMUM_BYTES_V1, PROJECT_FILE_SEARCH_MAXIMUM_RESULTS_V1,
    mcp_tools::{
        MCP_ADAPTER_ID, MCP_ADAPTER_VERSION, MCP_CAPABILITY_PREFIX, MCP_SCOPE, McpToolRuntimeV1,
        mcp_provider_name, split_mcp_capability,
    },
    model_tool_loop::{
        ModelToolInvocationPortV1, ModelToolLoopRequestV1, SettledModelToolCallV1, ToolInvokeV1,
        execute_model_tool_loop_v1,
    },
    pipeline::{CoreAuthenticationKey, LocalInvocationLedger, WorkflowPipelineError},
    project_scope::revalidate_git_branch,
    run_events::RunEventStream,
};

pub(crate) const FILE_TOOL_ADAPTER_VERSION: &str = "1.0.0";
pub(crate) const FILE_READ_CAPABILITY_ID: &str = "tool.files.read";
pub(crate) const FILE_SEARCH_CAPABILITY_ID: &str = "tool.files.search";
pub(crate) const FILE_LIST_CAPABILITY_ID: &str = "tool.files.list";
pub(crate) const FILE_GREP_CAPABILITY_ID: &str = "tool.files.grep";
pub(crate) const FILE_EDIT_CAPABILITY_ID: &str = "tool.files.edit";
pub(crate) const FILE_WRITE_CAPABILITY_ID: &str = "tool.files.write";
pub(crate) const SHELL_CAPABILITY_ID: &str = "tool.shell.host";
pub(crate) const PYTHON_CAPABILITY_ID: &str = "tool.python.host";
pub(crate) const TODO_CAPABILITY_ID: &str = "tool.todo";
pub(crate) const WEB_SEARCH_CAPABILITY_ID: &str = "tool.web_search";
pub(crate) const WEB_FETCH_CAPABILITY_ID: &str = "tool.web_fetch";
const FILE_READ_PROVIDER_NAME: &str = "aworkit_read_project_file";
const FILE_SEARCH_PROVIDER_NAME: &str = "aworkit_search_project_file";
const FILE_LIST_PROVIDER_NAME: &str = "aworkit_list_project_files";
const FILE_GREP_PROVIDER_NAME: &str = "aworkit_grep_project_files";
const FILE_EDIT_PROVIDER_NAME: &str = "aworkit_edit_project_file";
const FILE_WRITE_PROVIDER_NAME: &str = "aworkit_write_project_file";
const SHELL_PROVIDER_NAME: &str = "aworkit_host_shell";
const PYTHON_PROVIDER_NAME: &str = "aworkit_host_python";
const TODO_PROVIDER_NAME: &str = "aworkit_todo";
const WEB_SEARCH_PROVIDER_NAME: &str = "aworkit_web_search";
const WEB_FETCH_PROVIDER_NAME: &str = "aworkit_web_fetch";
const FILE_READ_ADAPTER_ID: &str = "adapter.project-files.read";
const FILE_SEARCH_ADAPTER_ID: &str = "adapter.project-files.search";
const FILE_LIST_ADAPTER_ID: &str = "adapter.project-files.list";
const FILE_GREP_ADAPTER_ID: &str = "adapter.project-files.grep";
const FILE_EDIT_ADAPTER_ID: &str = "adapter.project-files.edit";
const FILE_WRITE_ADAPTER_ID: &str = "adapter.project-files.write";
const SHELL_ADAPTER_ID: &str = "adapter.host-tools.shell";
const PYTHON_ADAPTER_ID: &str = "adapter.host-tools.python";
const TODO_ADAPTER_ID: &str = "adapter.run-tools.todo";
const WEB_SEARCH_ADAPTER_ID: &str = "adapter.web-tools.search";
const WEB_FETCH_ADAPTER_ID: &str = "adapter.web-tools.fetch";
const FILE_READ_SCOPE: &str = "project.read";
const FILE_SEARCH_SCOPE: &str = "project.search";
const FILE_LIST_SCOPE: &str = "project.list";
const FILE_GREP_SCOPE: &str = "project.grep";
const FILE_EDIT_SCOPE: &str = "project.edit";
const FILE_WRITE_SCOPE: &str = "project.write";
const SHELL_SCOPE: &str = "host.shell";
const PYTHON_SCOPE: &str = "host.python";
const TODO_SCOPE: &str = "run.todo";
const WEB_SEARCH_SCOPE: &str = "web.search";
const WEB_FETCH_SCOPE: &str = "web.fetch";
const TOOL_RECORD_CHAT_ID: &str = "pipeline.tool-invocations";
const TOOL_BROKER_CHAT_ID: &str = "broker.tool-invocations";
const TOOL_HOST_DESTINATION: &str = "aworkit.capability-host.tools";
const TOOL_WORKER_DESTINATION: &str = "aworkit.workflow-worker.tools";
const STORE_BRANCH_ID: &str = "main";
#[cfg(test)]
const TOOL_NODE_TYPE: &str = "agent";
const TOOL_APPROVAL_TTL_MILLIS: u64 = 60_000;
const MAXIMUM_TOOL_PAYLOAD_BYTES: usize = 256 * 1024;
pub(crate) const MAXIMUM_TOOL_RESULT_BYTES: usize = 512 * 1024;
const MAXIMUM_FILE_SEARCH_QUERY_BYTES: usize = 16 * 1024;
const MAXIMUM_ACTIVITY_TEXT_BYTES: usize = 512;
pub(crate) const PROJECT_FILE_LIST_MAXIMUM_ENTRIES_V1: u64 = 1000;
pub(crate) const PROJECT_FILE_GREP_MAXIMUM_MATCHES_V1: u64 = 512;
pub(crate) const PROJECT_FILE_WRITE_MAXIMUM_BYTES_V1: u64 = 1024 * 1024;
pub(crate) const WEB_SEARCH_MAXIMUM_RESULTS_V1: u64 = 8;
pub(crate) const WEB_FETCH_MAXIMUM_DOWNLOAD_BYTES_V1: u64 = 1024 * 1024;
pub(crate) const WEB_FETCH_MAXIMUM_EXTRACT_BYTES_V1: u64 = 32 * 1024;
pub(crate) const SUBAGENT_CAPABILITY_ID: &str = "tool.subagent";
const SUBAGENT_PROVIDER_NAME: &str = "aworkit_spawn_subagent";
const SUBAGENT_ADAPTER_ID: &str = "adapter.subagent.v1";
const SUBAGENT_SCOPE: &str = "run.subagent";
const SUBAGENT_MAXIMUM_TURNS: u64 = 8;
const SUBAGENT_MAXIMUM_TOOL_CALLS: u32 = 8;
const SUBAGENT_MAXIMUM_TOKENS: u64 = 64_000;
const SUBAGENT_MAXIMUM_TASK_BYTES: usize = 16 * 1024;
const SUBAGENT_MAXIMUM_CONTEXT_BYTES: usize = 32 * 1024;
const SUBAGENT_MAXIMUM_INPUT_BYTES: usize = 384 * 1024;
const SUBAGENT_MAXIMUM_OUTPUT_BYTES: usize = 64 * 1024;
/// Read-only, approval-free tools a subagent child may invoke. The subagent
/// tool itself is excluded, capping the v1 delegation depth at one.
pub(crate) const SUBAGENT_CHILD_TOOL_IDS: [&str; 7] = [
    FILE_READ_CAPABILITY_ID,
    FILE_SEARCH_CAPABILITY_ID,
    FILE_LIST_CAPABILITY_ID,
    FILE_GREP_CAPABILITY_ID,
    WEB_SEARCH_CAPABILITY_ID,
    WEB_FETCH_CAPABILITY_ID,
    TODO_CAPABILITY_ID,
];

/// Approval-free tool ids that settle without a user decision.
pub(crate) fn approval_free_tool_ids() -> BTreeSet<&'static str> {
    BTreeSet::from([
        FILE_READ_CAPABILITY_ID,
        FILE_SEARCH_CAPABILITY_ID,
        FILE_LIST_CAPABILITY_ID,
        FILE_GREP_CAPABILITY_ID,
        TODO_CAPABILITY_ID,
        WEB_SEARCH_CAPABILITY_ID,
        WEB_FETCH_CAPABILITY_ID,
    ])
}

/// Secret-free tool Settings frozen for one Chat/Run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowToolBindingV1 {
    pub capability_id: String,
    pub configuration: Value,
    /// Exact model-facing definition discovered at freeze for dynamic tools
    /// (MCP). Absent for built-ins whose schemas are compile-time owned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<ModelToolDefinitionV1>,
}

/// Durable per-invocation approval challenge a tool call is suspended on.
/// The decision id doubles as the broker invocation id, so one approval
/// command resolves exactly one tool call.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolApprovalChallengeV1 {
    pub decision_id: String,
    pub invocation_id: String,
    pub nonce: String,
    pub expires_epoch_millis: u64,
    pub capability_id: String,
    pub call_id: String,
    pub summary: String,
}

fn tool_approval_challenge(
    challenge: &ApprovalChallengeV1,
    call: &ModelToolCallV1,
) -> ToolApprovalChallengeV1 {
    ToolApprovalChallengeV1 {
        decision_id: challenge.invocation_id.to_string(),
        invocation_id: challenge.invocation_id.to_string(),
        nonce: challenge.nonce.to_string(),
        expires_epoch_millis: challenge.expires_epoch_millis,
        capability_id: challenge.capability_id.to_string(),
        call_id: call.call_id.clone(),
        summary: bounded_activity_text(format!(
            "The model requested tool {}.",
            challenge.capability_id
        )),
    }
}

/// Durable, UI-safe evidence for one authority-settled provider tool call.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowToolActivityV1 {
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
    #[serde(default)]
    pub requires_approval: bool,
    /// StableId-safe internal identity for dynamic tools (MCP): a deterministic
    /// `mcp.<digest>` encoding. Built-ins keep this empty and encode their
    /// capability id directly throughout the broker/manifest chain.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub internal_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum StoredFileToolLimitV1 {
    Read {
        maximum_bytes: usize,
    },
    Search {
        maximum_results: usize,
    },
    List {
        maximum_entries: usize,
    },
    Grep {
        maximum_matches: usize,
        maximum_files: usize,
    },
    Edit {
        maximum_bytes: usize,
    },
    Write {
        maximum_bytes: usize,
    },
    Shell {
        timeout_seconds: usize,
        maximum_output_bytes: usize,
    },
    Python {
        timeout_seconds: usize,
        maximum_output_bytes: usize,
    },
    Todo,
    WebSearch {
        maximum_results: usize,
    },
    WebFetch {
        maximum_download_bytes: usize,
        maximum_extract_bytes: usize,
    },
    Subagent {
        maximum_turns: usize,
    },
    Mcp {
        server_id: String,
        tool_name: String,
        schema_hash: String,
    },
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

/// Builds the complete attested built-in descriptor matrix for this desktop
/// generation. Approval classes live in the frozen manifest bindings; the
/// descriptor declares only capability semantics and side-effect classes.
pub(crate) fn file_tool_descriptors()
-> Result<BTreeMap<String, CapabilityDescriptor>, WorkflowPipelineError> {
    let mut descriptors = BTreeMap::new();
    for (capability_id, kind, scope, schema, side_effect, workspace) in [
        (
            FILE_READ_CAPABILITY_ID,
            CapabilityKind::FileRead,
            FILE_READ_SCOPE,
            file_read_schema(),
            SideEffectClass::ReadOnly,
            true,
        ),
        (
            FILE_SEARCH_CAPABILITY_ID,
            CapabilityKind::FileSearch,
            FILE_SEARCH_SCOPE,
            file_search_schema(),
            SideEffectClass::ReadOnly,
            true,
        ),
        (
            FILE_LIST_CAPABILITY_ID,
            CapabilityKind::FileList,
            FILE_LIST_SCOPE,
            file_list_schema(),
            SideEffectClass::ReadOnly,
            true,
        ),
        (
            FILE_GREP_CAPABILITY_ID,
            CapabilityKind::FileGrep,
            FILE_GREP_SCOPE,
            file_grep_schema(),
            SideEffectClass::ReadOnly,
            true,
        ),
        (
            FILE_EDIT_CAPABILITY_ID,
            CapabilityKind::FileEdit,
            FILE_EDIT_SCOPE,
            file_edit_schema(),
            SideEffectClass::IdempotentWrite,
            true,
        ),
        (
            FILE_WRITE_CAPABILITY_ID,
            CapabilityKind::FileWrite,
            FILE_WRITE_SCOPE,
            file_write_schema(),
            SideEffectClass::IdempotentWrite,
            true,
        ),
        (
            SHELL_CAPABILITY_ID,
            CapabilityKind::Shell,
            SHELL_SCOPE,
            shell_schema(),
            SideEffectClass::NonIdempotent,
            true,
        ),
        (
            PYTHON_CAPABILITY_ID,
            CapabilityKind::Python,
            PYTHON_SCOPE,
            python_schema(),
            SideEffectClass::NonIdempotent,
            true,
        ),
        (
            TODO_CAPABILITY_ID,
            CapabilityKind::Todo,
            TODO_SCOPE,
            todo_schema(),
            SideEffectClass::Pure,
            false,
        ),
        (
            WEB_SEARCH_CAPABILITY_ID,
            CapabilityKind::WebSearch,
            WEB_SEARCH_SCOPE,
            web_search_schema(),
            SideEffectClass::ReadOnly,
            false,
        ),
        (
            WEB_FETCH_CAPABILITY_ID,
            CapabilityKind::WebFetch,
            WEB_FETCH_SCOPE,
            web_fetch_schema(),
            SideEffectClass::ReadOnly,
            false,
        ),
        (
            SUBAGENT_CAPABILITY_ID,
            CapabilityKind::Subagent,
            SUBAGENT_SCOPE,
            subagent_schema(),
            SideEffectClass::NonIdempotent,
            false,
        ),
    ] {
        let mut descriptor = CapabilityDescriptor::build(
            capability_id,
            FILE_TOOL_ADAPTER_VERSION,
            kind,
            side_effect,
        )
        .map_err(|error| WorkflowPipelineError::Host(error.to_string()))?;
        descriptor.guarantees_same_id_deduplication = false;
        descriptor.supports_cancellation = true;
        descriptor.allowed_scopes = vec![scope.to_owned()];
        descriptor.requires_workspace = workspace;
        descriptor.maximum_concurrency = 8;
        descriptor.max_input_bytes = MAXIMUM_TOOL_PAYLOAD_BYTES;
        descriptor.max_output_bytes = MAXIMUM_TOOL_RESULT_BYTES;
        descriptor.input_schema_hash = Some(canonical_hash(&schema)?);
        descriptor
            .rehash()
            .map_err(|error| WorkflowPipelineError::Host(error.to_string()))?;
        descriptors.insert(capability_id.to_owned(), descriptor);
    }
    Ok(descriptors)
}

/// Builds a per-tool dynamic descriptor for one `mcp://<server>/<tool>`
/// capability. MCP tools are server-defined, so their descriptors exist for
/// authority-binding identity only and are never registered in the frozen
/// capability-host registry; dispatch executes through the session manager.
/// The descriptor identity uses the StableId-safe `mcp.<digest>` internal id
/// because `mcp://…` ids violate the capability-host name grammar.
pub(crate) fn mcp_tool_descriptor(
    internal_id: &str,
) -> Result<CapabilityDescriptor, WorkflowPipelineError> {
    let mut descriptor = CapabilityDescriptor::build(
        internal_id,
        MCP_ADAPTER_VERSION,
        CapabilityKind::Mcp,
        SideEffectClass::Unknown,
    )
    .map_err(|error| WorkflowPipelineError::Host(error.to_string()))?;
    descriptor.guarantees_same_id_deduplication = false;
    descriptor.supports_cancellation = true;
    descriptor.allowed_scopes = vec![MCP_SCOPE.to_owned()];
    descriptor.requires_workspace = false;
    descriptor.maximum_concurrency = 4;
    descriptor.max_input_bytes = MAXIMUM_TOOL_PAYLOAD_BYTES;
    descriptor.max_output_bytes = MAXIMUM_TOOL_RESULT_BYTES;
    descriptor
        .rehash()
        .map_err(|error| WorkflowPipelineError::Host(error.to_string()))?;
    Ok(descriptor)
}

/// Validates and freezes the exact authority-relevant subset of tool Settings
/// for every built-in tool in the v1 matrix. Approval classes come from the
/// tool id, not from model output.
pub(crate) fn freeze_file_tool_bindings(
    requested: &[WorkflowToolBindingV1],
) -> Result<Vec<StoredFileToolBindingV1>, WorkflowPipelineError> {
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
                FILE_READ_PROVIDER_NAME.to_owned(),
                "Read one UTF-8 text file relative to the frozen project root.".to_owned(),
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
                FILE_SEARCH_PROVIDER_NAME.to_owned(),
                "Find exact UTF-8 text matches in one file relative to the frozen project root.".to_owned(),
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
            FILE_LIST_CAPABILITY_ID => (
                FILE_LIST_PROVIDER_NAME.to_owned(),
                "List project files matching a bounded glob (supports *, **, ?), newest first.".to_owned(),
                file_list_schema(),
                StoredFileToolLimitV1::List {
                    maximum_entries: exact_unsigned_configuration(
                        &requested.configuration,
                        &[
                            ("authorityMode", Value::String("project_files".into())),
                            ("effect", Value::String("list".into())),
                        ],
                        "maximumEntries",
                        1,
                        PROJECT_FILE_LIST_MAXIMUM_ENTRIES_V1,
                    )?,
                },
            ),
            FILE_GREP_CAPABILITY_ID => (
                FILE_GREP_PROVIDER_NAME.to_owned(),
                "Regex-search text files beneath the frozen project root with line context.".to_owned(),
                file_grep_schema(),
                StoredFileToolLimitV1::Grep {
                    maximum_matches: exact_unsigned_configuration(
                        &requested.configuration,
                        &[
                            ("authorityMode", Value::String("project_files".into())),
                            ("effect", Value::String("grep".into())),
                        ],
                        "maximumMatches",
                        1,
                        PROJECT_FILE_GREP_MAXIMUM_MATCHES_V1,
                    )?,
                    maximum_files: 128,
                },
            ),
            FILE_EDIT_CAPABILITY_ID => (
                FILE_EDIT_PROVIDER_NAME.to_owned(),
                "Replace one exact text range in a project file atomically; approval required.".to_owned(),
                file_edit_schema(),
                StoredFileToolLimitV1::Edit {
                    maximum_bytes: *freeze_configuration(
                        &requested.configuration,
                        &[
                            ("authorityMode", Value::String("project_files".into())),
                            ("effect", Value::String("write".into())),
                            ("requiresApproval", Value::Bool(true)),
                        ],
                        &[("maximumBytes", 1, PROJECT_FILE_WRITE_MAXIMUM_BYTES_V1)],
                    )?
                    .get("maximumBytes")
                    .expect("frozen maximumBytes"),
                },
            ),
            FILE_WRITE_CAPABILITY_ID => (
                FILE_WRITE_PROVIDER_NAME.to_owned(),
                "Create or replace a project file with exact content; approval required.".to_owned(),
                file_write_schema(),
                StoredFileToolLimitV1::Write {
                    maximum_bytes: *freeze_configuration(
                        &requested.configuration,
                        &[
                            ("authorityMode", Value::String("project_files".into())),
                            ("effect", Value::String("write".into())),
                            ("requiresApproval", Value::Bool(true)),
                        ],
                        &[("maximumBytes", 1, PROJECT_FILE_WRITE_MAXIMUM_BYTES_V1)],
                    )?
                    .get("maximumBytes")
                    .expect("frozen maximumBytes"),
                },
            ),
            SHELL_CAPABILITY_ID => (
                SHELL_PROVIDER_NAME.to_owned(),
                "Run one bounded host shell command; the working directory is not a sandbox. Approval required.".to_owned(),
                shell_schema(),
                StoredFileToolLimitV1::Shell {
                    timeout_seconds: *freeze_configuration(
                        &requested.configuration,
                        &[
                            ("authorityMode", Value::String("host_shell".into())),
                            ("requiresApproval", Value::Bool(true)),
                        ],
                        &[
                            ("timeoutSeconds", 1, 300),
                            ("maximumOutputBytes", 1, 262_144),
                        ],
                    )?
                    .get("timeoutSeconds")
                    .expect("frozen timeoutSeconds"),
                    maximum_output_bytes: *freeze_configuration(
                        &requested.configuration,
                        &[
                            ("authorityMode", Value::String("host_shell".into())),
                            ("requiresApproval", Value::Bool(true)),
                        ],
                        &[
                            ("timeoutSeconds", 1, 300),
                            ("maximumOutputBytes", 1, 262_144),
                        ],
                    )?
                    .get("maximumOutputBytes")
                    .expect("frozen maximumOutputBytes"),
                },
            ),
            PYTHON_CAPABILITY_ID => (
                PYTHON_PROVIDER_NAME.to_owned(),
                "Run one bounded isolated-interpreter Python script on the host; approval required.".to_owned(),
                python_schema(),
                StoredFileToolLimitV1::Python {
                    timeout_seconds: *freeze_configuration(
                        &requested.configuration,
                        &[
                            ("authorityMode", Value::String("host_python".into())),
                            ("requiresApproval", Value::Bool(true)),
                            ("isolatedInterpreter", Value::Bool(true)),
                        ],
                        &[
                            ("timeoutSeconds", 1, 300),
                            ("maximumOutputBytes", 1, 262_144),
                        ],
                    )?
                    .get("timeoutSeconds")
                    .expect("frozen timeoutSeconds"),
                    maximum_output_bytes: *freeze_configuration(
                        &requested.configuration,
                        &[
                            ("authorityMode", Value::String("host_python".into())),
                            ("requiresApproval", Value::Bool(true)),
                            ("isolatedInterpreter", Value::Bool(true)),
                        ],
                        &[
                            ("timeoutSeconds", 1, 300),
                            ("maximumOutputBytes", 1, 262_144),
                        ],
                    )?
                    .get("maximumOutputBytes")
                    .expect("frozen maximumOutputBytes"),
                },
            ),
            TODO_CAPABILITY_ID => {
                freeze_configuration(
                    &requested.configuration,
                    &[("authorityMode", Value::String("run_todo".into()))],
                    &[],
                )?;
                (
                    TODO_PROVIDER_NAME.to_owned(),
                    "Replace the Run's task list with an ordered checklist of pending/completed items.".to_owned(),
                    todo_schema(),
                    StoredFileToolLimitV1::Todo,
                )
            }
            WEB_SEARCH_CAPABILITY_ID => (
                WEB_SEARCH_PROVIDER_NAME.to_owned(),
                "Search the web over HTTPS and return bounded title/snippet/url results.".to_owned(),
                web_search_schema(),
                StoredFileToolLimitV1::WebSearch {
                    maximum_results: exact_unsigned_configuration(
                        &requested.configuration,
                        &[],
                        "maximumResults",
                        1,
                        WEB_SEARCH_MAXIMUM_RESULTS_V1,
                    )?,
                },
            ),
            WEB_FETCH_CAPABILITY_ID => (
                WEB_FETCH_PROVIDER_NAME.to_owned(),
                "Fetch one HTTPS page and return its extracted plain text within strict bounds.".to_owned(),
                web_fetch_schema(),
                StoredFileToolLimitV1::WebFetch {
                    maximum_download_bytes: *freeze_configuration(
                        &requested.configuration,
                        &[],
                        &[
                            ("maximumDownloadBytes", 1, WEB_FETCH_MAXIMUM_DOWNLOAD_BYTES_V1),
                            ("maximumExtractBytes", 1, WEB_FETCH_MAXIMUM_EXTRACT_BYTES_V1),
                        ],
                    )?
                    .get("maximumDownloadBytes")
                    .expect("frozen maximumDownloadBytes"),
                    maximum_extract_bytes: *freeze_configuration(
                        &requested.configuration,
                        &[],
                        &[
                            ("maximumDownloadBytes", 1, WEB_FETCH_MAXIMUM_DOWNLOAD_BYTES_V1),
                            ("maximumExtractBytes", 1, WEB_FETCH_MAXIMUM_EXTRACT_BYTES_V1),
                        ],
                    )?
                    .get("maximumExtractBytes")
                    .expect("frozen maximumExtractBytes"),
                },
            ),
            SUBAGENT_CAPABILITY_ID => (
                SUBAGENT_PROVIDER_NAME.to_owned(),
                "Delegate one bounded read-only subtask to a fresh subagent context; approval required.".to_owned(),
                subagent_schema(),
                StoredFileToolLimitV1::Subagent {
                    maximum_turns: *freeze_configuration(
                        &requested.configuration,
                        &[
                            ("authorityMode", Value::String("run_subagent".into())),
                            ("requiresApproval", Value::Bool(true)),
                        ],
                        &[("maximumTurns", 1, SUBAGENT_MAXIMUM_TURNS)],
                    )?
                    .get("maximumTurns")
                    .expect("frozen maximumTurns"),
                },
            ),
            id if id.starts_with(MCP_CAPABILITY_PREFIX) => freeze_mcp_binding(requested)?,
            _ => return Err(invalid_tool("tool binding has no installed native adapter")),
        };
        let internal_id = if requested.capability_id.starts_with(MCP_CAPABILITY_PREFIX) {
            mcp_internal_id(&requested.capability_id)
        } else {
            String::new()
        };
        bindings.push(StoredFileToolBindingV1 {
            capability_id: requested.capability_id.clone(),
            provider_name,
            description,
            input_schema,
            configuration_hash: canonical_hash(&requested.configuration)?,
            configuration: requested.configuration.clone(),
            limit,
            requires_approval: !requested.capability_id.starts_with(MCP_CAPABILITY_PREFIX)
                && !approval_free_tool_ids().contains(requested.capability_id.as_str()),
            internal_id,
        });
    }
    Ok(bindings)
}

/// Freezes one `mcp://<server>/<tool>` binding. The configuration carries
/// exactly the two resolution keys; the model-facing name, description, and
/// schema come from the definition discovered at freeze (or a generated
/// permissive fallback for callers without discovery). The session layer
/// enforces the exact discovered schema hash on every call.
fn freeze_mcp_binding(
    requested: &WorkflowToolBindingV1,
) -> Result<(String, String, Value, StoredFileToolLimitV1), WorkflowPipelineError> {
    let (server_id, tool) =
        split_mcp_capability(&requested.capability_id).map_err(|error| invalid_tool(&error))?;
    let object = requested
        .configuration
        .as_object()
        .ok_or_else(|| invalid_tool("tool configuration must be an object"))?;
    let observed = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if observed != BTreeSet::from(["serverId", "tool"]) {
        return Err(invalid_tool(
            "MCP tool configuration accepts exactly serverId and tool",
        ));
    }
    let configured_server = object
        .get("serverId")
        .and_then(Value::as_str)
        .filter(|value| StableId::parse((*value).to_owned()).is_ok())
        .ok_or_else(|| invalid_tool("MCP serverId is not a valid stable identifier"))?;
    let configured_tool = object
        .get("tool")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 256 && !value.contains('\0'))
        .ok_or_else(|| invalid_tool("MCP tool name is empty, oversized, or malformed"))?;
    if configured_server != server_id || configured_tool != tool {
        return Err(invalid_tool(
            "MCP capability id does not match the serverId/tool configuration",
        ));
    }
    let definition = requested
        .definition
        .clone()
        .unwrap_or_else(|| ModelToolDefinitionV1 {
            capability_id: requested.capability_id.clone(),
            name: mcp_provider_name(server_id, tool),
            description: format!("Call MCP tool '{tool}' on server '{server_id}'."),
            input_schema: json!({"type": "object", "additionalProperties": true}),
        });
    if definition.capability_id != requested.capability_id {
        return Err(invalid_tool(
            "MCP tool definition capability id does not match the binding",
        ));
    }
    let schema_hash = mcp_schema_hash(&definition.input_schema);
    Ok((
        definition.name.clone(),
        definition.description.clone(),
        definition.input_schema.clone(),
        StoredFileToolLimitV1::Mcp {
            server_id: server_id.to_owned(),
            tool_name: tool.to_owned(),
            schema_hash,
        },
    ))
}

/// Deterministic StableId-safe internal identity for one MCP capability. The
/// model-facing `mcp://<server>/<tool>` id stays in the binding while every
/// broker/manifest/descriptor layer uses this digest form.
pub(crate) fn mcp_internal_id(capability_id: &str) -> String {
    let digest = format!("{:x}", Sha256::digest(capability_id.as_bytes()));
    format!("mcp.{}", &digest[..40])
}

/// Reproduces the capability-host discovery hash for one tool schema so the
/// session layer's schema-drift check sees the exact pinned identity.
fn mcp_schema_hash(schema: &Value) -> String {
    let bytes = serde_json::to_vec(schema).unwrap_or_default();
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
pub(crate) fn file_tool_capability_binding(
    tool: &StoredFileToolBindingV1,
    descriptor: &CapabilityDescriptor,
) -> Result<CapabilityBindingV1, WorkflowPipelineError> {
    file_tool_capability_binding_with_nodes(tool, descriptor, vec![TOOL_NODE_TYPE.to_owned()])
}

pub(crate) fn file_tool_capability_binding_with_nodes(
    tool: &StoredFileToolBindingV1,
    descriptor: &CapabilityDescriptor,
    allowed_node_types: Vec<String>,
) -> Result<CapabilityBindingV1, WorkflowPipelineError> {
    let descriptor_capability_id = if tool.capability_id.starts_with(MCP_CAPABILITY_PREFIX) {
        &tool.internal_id
    } else {
        &tool.capability_id
    };
    if descriptor.capability_id != *descriptor_capability_id {
        return Err(WorkflowPipelineError::IncompleteEvidence);
    }
    let binding_capability_id = if tool.capability_id.starts_with(MCP_CAPABILITY_PREFIX) {
        stable(&tool.internal_id)?
    } else {
        stable(&tool.capability_id)?
    };
    Ok(CapabilityBindingV1 {
        capability_id: binding_capability_id,
        adapter_id: stable(match tool.capability_id.as_str() {
            FILE_READ_CAPABILITY_ID => FILE_READ_ADAPTER_ID,
            FILE_SEARCH_CAPABILITY_ID => FILE_SEARCH_ADAPTER_ID,
            FILE_LIST_CAPABILITY_ID => FILE_LIST_ADAPTER_ID,
            FILE_GREP_CAPABILITY_ID => FILE_GREP_ADAPTER_ID,
            FILE_EDIT_CAPABILITY_ID => FILE_EDIT_ADAPTER_ID,
            FILE_WRITE_CAPABILITY_ID => FILE_WRITE_ADAPTER_ID,
            SHELL_CAPABILITY_ID => SHELL_ADAPTER_ID,
            PYTHON_CAPABILITY_ID => PYTHON_ADAPTER_ID,
            TODO_CAPABILITY_ID => TODO_ADAPTER_ID,
            WEB_SEARCH_CAPABILITY_ID => WEB_SEARCH_ADAPTER_ID,
            WEB_FETCH_CAPABILITY_ID => WEB_FETCH_ADAPTER_ID,
            SUBAGENT_CAPABILITY_ID => SUBAGENT_ADAPTER_ID,
            id if id.starts_with(MCP_CAPABILITY_PREFIX) => MCP_ADAPTER_ID,
            _ => return Err(WorkflowPipelineError::IncompleteEvidence),
        })?,
        adapter_version: descriptor.version.clone(),
        descriptor_hash: descriptor.version_hash.clone(),
        extension: None,
        required_isolation_profile: descriptor.required_isolation.clone(),
        enabled: true,
        compatible: true,
        approval: if tool.requires_approval {
            ApprovalRequirement::PerInvocation
        } else {
            ApprovalRequirement::Never
        },
        allowed_node_types,
    })
}

#[derive(Clone)]
pub(crate) struct FileToolAuthorityRuntimeV1 {
    projects: ProjectCoordinator,
    records: Arc<ToolRecordStore>,
    ledger: Arc<LocalInvocationLedger>,
    host: Arc<CapabilityHost>,
    descriptors: BTreeMap<String, CapabilityDescriptor>,
    web: WebTools,
    pub(crate) mcp: Arc<McpToolRuntimeV1>,
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
    ) -> Result<Self, WorkflowPipelineError> {
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
            web: WebTools::production(),
            mcp: Arc::new(McpToolRuntimeV1::new(generation)),
            generation,
            core_key,
        })
    }

    #[cfg(test)]
    pub(crate) fn bind(
        &self,
        context: FrozenFileToolAuthorityContextV1,
    ) -> BoundFileToolAuthorityV1 {
        let stream = Arc::new(RunEventStream::new(
            context.request_id.to_string(),
            context.run_id.to_string(),
            super::semantic_events::ephemeral_semantic_event_committer(),
            CancellationToken::default(),
        ));
        self.bind_with_run_events(context, stream)
    }

    pub(crate) fn bind_with_run_events(
        &self,
        context: FrozenFileToolAuthorityContextV1,
        run_events: Arc<RunEventStream>,
    ) -> BoundFileToolAuthorityV1 {
        debug_assert!(run_events.belongs_to(context.request_id.as_str(), context.run_id.as_str()));
        BoundFileToolAuthorityV1 {
            runtime: self.clone(),
            context,
            run_events,
        }
    }

    /// Latest durable Run task list recorded by the todo tool, if any.
    pub(crate) fn todo_state(
        &self,
        run_id: &StableId,
    ) -> Result<Option<Value>, WorkflowPipelineError> {
        self.records.todo_state(run_id)
    }
}

#[derive(Clone)]
pub(crate) struct FrozenFileToolAuthorityContextV1 {
    pub manifest: AuthorityManifestV1,
    pub run_id: StableId,
    pub request_id: StableId,
    pub node_id: StableId,
    pub workspace: WorkspaceBindingV1,
    pub project_branch: Option<String>,
    pub bindings: Vec<StoredFileToolBindingV1>,
    pub deadline_epoch_millis: u64,
    /// The frozen model gateway plus its binding identity, used only by the
    /// subagent tool to run the bounded child loop inside the dispatch.
    pub model_gateway: Option<Arc<FrozenModelGateway>>,
    pub model_binding_id: Option<String>,
    pub model_version_hash: Option<String>,
    /// Frozen core-attested MCP manifests bound to this Run, keyed by server
    /// id. Sessions open on demand with exact binding-drift protection.
    pub mcp_manifests: BTreeMap<String, McpServerManifestV1>,
    /// The frozen pass cancellation token; the MCP dispatch watcher forwards
    /// it to session-scoped cancellation when the pass is cancelled.
    pub cancellation: CancellationToken,
}

pub(crate) struct BoundFileToolAuthorityV1 {
    runtime: FileToolAuthorityRuntimeV1,
    context: FrozenFileToolAuthorityContextV1,
    run_events: Arc<RunEventStream>,
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

    fn invoke_extended(
        &self,
        outer_invocation_id: &StableId,
        turn: u32,
        call: &ModelToolCallV1,
        cancellation: &CancellationToken,
    ) -> Result<ToolInvokeV1, String> {
        match self.invoke_v1(outer_invocation_id, turn, call, cancellation) {
            Ok(settled) => Ok(ToolInvokeV1::Settled(settled)),
            Err(WorkflowPipelineError::ToolApproval(challenge)) => {
                Ok(ToolInvokeV1::Approval(challenge))
            }
            Err(error) => Err(error.to_string()),
        }
    }

    fn resolve(
        &self,
        outer_invocation_id: &StableId,
        turn: u32,
        call: &ModelToolCallV1,
        response: &ApprovalResponseV1,
        cancellation: &CancellationToken,
    ) -> Result<SettledModelToolCallV1, String> {
        self.resolve_invoke_v1(outer_invocation_id, turn, call, response, cancellation)
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
    ) -> Result<SettledModelToolCallV1, WorkflowPipelineError> {
        self.run_events.publish_tool_started(call);
        let result =
            self.invoke_v1_with_delivery(outer_invocation_id, turn, call, cancellation, false);
        self.publish_tool_outcome(call, &result);
        result
    }

    /// Same broker flow with delivery scoped to exactly this invocation's
    /// outbox entry. Subagent children use it so their deliveries never scan
    /// an in-flight sibling dispatch as abandoned.
    pub(crate) fn invoke_v1_scoped(
        &self,
        outer_invocation_id: &StableId,
        turn: u32,
        call: &ModelToolCallV1,
        cancellation: &CancellationToken,
    ) -> Result<SettledModelToolCallV1, WorkflowPipelineError> {
        self.run_events.publish_tool_started(call);
        let result =
            self.invoke_v1_with_delivery(outer_invocation_id, turn, call, cancellation, true);
        self.publish_tool_outcome(call, &result);
        result
    }

    fn invoke_v1_with_delivery(
        &self,
        outer_invocation_id: &StableId,
        turn: u32,
        call: &ModelToolCallV1,
        cancellation: &CancellationToken,
        scoped_delivery: bool,
    ) -> Result<SettledModelToolCallV1, WorkflowPipelineError> {
        let (broker, proposal, replayed) = self.prepare_broker(outer_invocation_id, turn, call)?;
        let proposal_id = proposal.proposal_id.clone();
        let decision = broker
            .propose(
                &legacy_manifest(&self.context.manifest),
                proposal,
                current_epoch_millis(),
            )
            .map_err(broker_error)?;
        match decision {
            BrokerDecisionV1::AwaitingApproval(challenge) => Err(
                WorkflowPipelineError::ToolApproval(tool_approval_challenge(&challenge, call)),
            ),
            _ => self.complete_broker_decision(
                broker,
                &proposal_id,
                decision,
                replayed,
                call,
                cancellation,
                scoped_delivery,
            ),
        }
    }

    fn resolve_invoke_v1(
        &self,
        outer_invocation_id: &StableId,
        turn: u32,
        call: &ModelToolCallV1,
        response: &ApprovalResponseV1,
        cancellation: &CancellationToken,
    ) -> Result<SettledModelToolCallV1, WorkflowPipelineError> {
        self.run_events.publish_tool_started(call);
        let result =
            self.resolve_invoke_v1_inner(outer_invocation_id, turn, call, response, cancellation);
        self.publish_tool_outcome(call, &result);
        result
    }

    fn resolve_invoke_v1_inner(
        &self,
        outer_invocation_id: &StableId,
        turn: u32,
        call: &ModelToolCallV1,
        response: &ApprovalResponseV1,
        cancellation: &CancellationToken,
    ) -> Result<SettledModelToolCallV1, WorkflowPipelineError> {
        let (broker, proposal, replayed) = self.prepare_broker(outer_invocation_id, turn, call)?;
        let proposal_id = proposal.proposal_id.clone();
        let decision = broker
            .propose(
                &legacy_manifest(&self.context.manifest),
                proposal,
                current_epoch_millis(),
            )
            .map_err(broker_error)?;
        let decision = match decision {
            BrokerDecisionV1::AwaitingApproval(_) => {
                // A replayed proposal yields the pending challenge; resolve it
                // with the committed user decision.
                broker
                    .resolve_approval(&legacy_manifest(&self.context.manifest), response)
                    .map_err(broker_error)?
            }
            other => other,
        };
        match decision {
            BrokerDecisionV1::Denied => {
                // The rejection is durably recorded by the broker; the model
                // receives an explicit denial result.
                let denied = SettledModelToolCallV1 {
                    result: ModelToolResultV1 {
                        call_id: call.call_id.clone(),
                        content: json!({
                            "error": "user_rejected",
                            "detail": "The user rejected this tool invocation."
                        }),
                        is_error: true,
                    },
                    activity: WorkflowToolActivityV1 {
                        call_id: call.call_id.clone(),
                        invocation_id: response.invocation_id.clone(),
                        capability_id: call.capability_id.clone(),
                        path: String::new(),
                        status: "denied".into(),
                        summary: "Tool invocation was rejected by the user.".into(),
                        outcome_hash: String::new(),
                        replayed: false,
                    },
                };
                self.records_denied_outcome(&response.invocation_id, call)?;
                Ok(denied)
            }
            _ => self.complete_broker_decision(
                broker,
                &proposal_id,
                decision,
                replayed,
                call,
                cancellation,
                false,
            ),
        }
    }

    fn publish_tool_outcome(
        &self,
        call: &ModelToolCallV1,
        result: &Result<SettledModelToolCallV1, WorkflowPipelineError>,
    ) {
        match result {
            Ok(settled) => self.run_events.publish_tool_terminal(
                call,
                &settled.activity.status,
                settled.activity.summary.clone(),
                serde_json::to_value(&settled.result).unwrap_or(Value::Null),
            ),
            Err(WorkflowPipelineError::ToolApproval(challenge)) => {
                self.run_events.publish_tool_waiting(
                    call,
                    challenge.summary.clone(),
                    serde_json::to_value(challenge).unwrap_or(Value::Null),
                );
            }
            Err(error) => self.run_events.publish_tool_terminal(
                call,
                "failed",
                error.to_string(),
                json!({"error": error.to_string()}),
            ),
        }
    }

    fn records_denied_outcome(
        &self,
        invocation_id: &StableId,
        call: &ModelToolCallV1,
    ) -> Result<(), WorkflowPipelineError> {
        if self.runtime.records.outcome(invocation_id)?.is_some() {
            return Ok(());
        }
        self.runtime
            .records
            .record_outcome(&ToolOutcomeRecordV1 {
                schema_version: 1,
                invocation_id: invocation_id.clone(),
                call_id: call.call_id.clone(),
                capability_id: call.capability_id.clone(),
                path: String::new(),
                result: json!({"error": "user_rejected"}),
                is_error: true,
                summary: "Tool invocation was rejected by the user.".into(),
            })
            .map(|_| ())
    }

    fn prepare_broker(
        &self,
        outer_invocation_id: &StableId,
        turn: u32,
        call: &ModelToolCallV1,
    ) -> Result<(DurableInvocationBroker, WorkerInvocationProposalV1, bool), WorkflowPipelineError>
    {
        self.runtime
            .projects
            .revalidate_workspace_v1(&self.context.workspace)
            .map_err(|error| WorkflowPipelineError::Authority(error.to_string()))?;
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
        let expected_manifest_ref = if binding.capability_id.starts_with(MCP_CAPABILITY_PREFIX) {
            binding.internal_id.as_str()
        } else {
            binding.capability_id.as_str()
        };
        let manifest_binding = self
            .context
            .manifest
            .capability_bindings
            .iter()
            .find(|candidate| candidate.capability_id.as_str() == expected_manifest_ref)
            .cloned()
            .ok_or(WorkflowPipelineError::AuthorityDenied)?;
        if !manifest_binding.enabled || !manifest_binding.compatible {
            return Err(WorkflowPipelineError::AuthorityDenied);
        }
        let record = self.prepare_invocation_record(
            outer_invocation_id,
            turn,
            call,
            binding,
            manifest_binding,
        )?;
        let proposal = record.proposal.clone();
        let replayed = self.runtime.records.record_invocation(&record)?;
        Ok((
            DurableInvocationBroker::new(self.runtime.ledger.clone(), TOOL_APPROVAL_TTL_MILLIS),
            proposal,
            replayed,
        ))
    }

    fn complete_broker_decision(
        &self,
        broker: DurableInvocationBroker,
        proposal_id: &StableId,
        decision: BrokerDecisionV1,
        replayed: bool,
        call: &ModelToolCallV1,
        cancellation: &CancellationToken,
        scoped_delivery: bool,
    ) -> Result<SettledModelToolCallV1, WorkflowPipelineError> {
        if cancellation.is_cancelled() {
            return Err(WorkflowPipelineError::Host(
                "Agent tool loop was cancelled".into(),
            ));
        }
        let invocation_id = match decision {
            BrokerDecisionV1::DispatchReady(dispatch) => dispatch.invocation_id,
            BrokerDecisionV1::AlreadySettled(_) => self
                .runtime
                .ledger
                .invocation_for_proposal(proposal_id)?
                .ok_or(WorkflowPipelineError::IncompleteEvidence)?,
            BrokerDecisionV1::Denied => return Err(WorkflowPipelineError::AuthorityDenied),
            BrokerDecisionV1::AwaitingApproval(_) => {
                return Err(WorkflowPipelineError::ApprovalRequired);
            }
        };
        self.reconcile_outcome(&broker, &invocation_id)?;
        if self.runtime.ledger.settlement(&invocation_id)?.is_none() {
            let host = FileToolHostPortV1 {
                runtime: self.runtime.clone(),
                context: self.context.clone(),
                run_events: self.run_events.clone(),
            };
            // Top-level deliveries drain every pending outbox; nested
            // deliveries (subagent children) target exactly this invocation
            // so the in-flight parent dispatch is not seen as abandoned.
            let delivery = if scoped_delivery {
                broker
                    .deliver_pending_dispatch_for(&invocation_id, &host)
                    .map(|_| 0_usize)
            } else {
                broker.deliver_dispatches(&host)
            };
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
            .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
        if uncertain {
            return Err(WorkflowPipelineError::Host(
                "project-file tool outcome is uncertain; automatic replay is forbidden".into(),
            ));
        }
        let outcome = self
            .runtime
            .records
            .outcome(&invocation_id)?
            .filter(|outcome| canonical_hash(outcome).ok().as_deref() == Some(&outcome_hash));
        let _ = broker.deliver_worker_results(&CommittedToolResultAckV1);
        let Some(outcome) = outcome else {
            if is_definitely_not_started_settlement_v1(&outcome_hash, uncertain) {
                let detail = if current_epoch_millis() >= self.context.deadline_epoch_millis {
                    "tool invocation deadline elapsed before execution started"
                } else {
                    "tool invocation was rejected before execution started"
                };
                return Err(WorkflowPipelineError::Host(detail.into()));
            }
            return Err(WorkflowPipelineError::IncompleteEvidence);
        };
        Ok(SettledModelToolCallV1 {
            result: ModelToolResultV1 {
                call_id: call.call_id.clone(),
                content: outcome.result.clone(),
                is_error: outcome.is_error,
            },
            activity: WorkflowToolActivityV1 {
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
    ) -> Result<ToolInvocationRecordV1, WorkflowPipelineError> {
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
                capability_id: if binding.capability_id.starts_with(MCP_CAPABILITY_PREFIX) {
                    stable(&binding.internal_id)?
                } else {
                    stable(&binding.capability_id)?
                },
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
    ) -> Result<(), WorkflowPipelineError> {
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
            return Err(WorkflowPipelineError::IncompleteEvidence);
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
    context: FrozenFileToolAuthorityContextV1,
    run_events: Arc<RunEventStream>,
}

impl ApprovedHostDispatchPortV1 for FileToolHostPortV1 {
    fn dispatch(&self, dispatch: &ApprovedDispatchV1) -> Result<DeliveryAcceptanceV1, BrokerError> {
        let record = match self.runtime.records.invocation_for_dispatch(dispatch) {
            Ok(Some(record)) => record,
            Ok(None) => return Ok(DeliveryAcceptanceV1::RejectedDefinitelyNotStarted),
            Err(_) => return Ok(DeliveryAcceptanceV1::Ambiguous),
        };
        if record.call.capability_id.starts_with(MCP_CAPABILITY_PREFIX) {
            return self.dispatch_mcp(record, dispatch);
        }
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
            web: self.runtime.web.clone(),
            runtime: self.runtime.clone(),
            context: self.context.clone(),
            run_events: self.run_events.clone(),
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
    web: WebTools,
    runtime: FileToolAuthorityRuntimeV1,
    context: FrozenFileToolAuthorityContextV1,
    run_events: Arc<RunEventStream>,
    record: ToolInvocationRecordV1,
}

impl AdmittedInvocationDispatcherV1 for FileToolDispatcherV1 {
    type Output = Result<ToolOutcomeRecordV1, WorkflowPipelineError>;

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

impl FileToolHostPortV1 {
    /// Dispatches an MCP tool call without the capability host: the session
    /// manager is the execution boundary for server-defined tools, and their
    /// descriptors exist only for authority-binding identity. The durable
    /// broker already committed the authority decision; here the call executes
    /// exactly once (an existing outcome short-circuits redelivery) and the
    /// settlement records immediately after.
    fn dispatch_mcp(
        &self,
        record: ToolInvocationRecordV1,
        dispatch: &ApprovedDispatchV1,
    ) -> Result<DeliveryAcceptanceV1, BrokerError> {
        if record.authority_manifest_id != dispatch.manifest_id
            || dispatch.payload_hash != record.proposal.payload_hash
            || dispatch.capability_id != record.proposal.capability_id
            || canonical_hash(&record.payload).ok().as_deref()
                != Some(dispatch.payload_hash.as_str())
            || record.manifest_binding.capability_id != dispatch.capability_id
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
        if self
            .runtime
            .records
            .outcome(&dispatch.invocation_id)
            .ok()
            .flatten()
            .is_some()
        {
            return Ok(DeliveryAcceptanceV1::AlreadyAccepted);
        }
        let cancellation_token = digest_id("cancel.mcp", dispatch.invocation_id.as_str())
            .map_err(|_| BrokerError::Unavailable)?;
        let server_id = match &record.binding.limit {
            StoredFileToolLimitV1::Mcp { server_id, .. } => {
                StableId::parse(server_id.clone()).map_err(|_| BrokerError::Unavailable)?
            }
            _ => return Err(BrokerError::IdentityConflict),
        };
        let envelope = ApprovedInvocationEnvelopeV1 {
            schema_version: SchemaVersion::V1,
            invocation_id: dispatch.invocation_id.clone(),
            decision_id: dispatch.invocation_id.clone(),
            host_generation: self.runtime.generation,
            capability_id: record.call.capability_id.clone(),
            adapter_version: record.manifest_binding.adapter_version.clone(),
            binding_hash: record.manifest_binding.descriptor_hash.clone(),
            extension: None,
            required_isolation_profile: record.manifest_binding.required_isolation_profile.clone(),
            kind: CapabilityKind::Mcp,
            enforced_scopes: vec![MCP_SCOPE.to_owned()],
            deadline_epoch_millis: record.deadline_epoch_millis,
            cancellation_token: cancellation_token.clone(),
            lease_handles: Vec::new(),
            max_output_bytes: MAXIMUM_TOOL_RESULT_BYTES,
            payload: record.payload.clone(),
            core_authentication_tag: String::new(),
        };
        let dispatcher = FileToolDispatcherV1 {
            projects: self.runtime.projects.clone(),
            records: self.runtime.records.clone(),
            web: self.runtime.web.clone(),
            runtime: self.runtime.clone(),
            context: self.context.clone(),
            run_events: self.run_events.clone(),
            record,
        };
        let cancellation = self
            .runtime
            .mcp
            .register_dispatch_token(&cancellation_token)
            .map_err(|_| BrokerError::Unavailable)?;
        // Session-scoped cancellation: when the frozen pass is cancelled the
        // in-flight MCP call is cancelled through the reserved control path.
        // On normal completion the scoped token fires and the watcher exits
        // without touching the settled invocation.
        let watcher = {
            let runtime = self.runtime.clone();
            let pass_cancellation = self.context.cancellation.clone();
            let scoped_done = cancellation.clone();
            let token_id = cancellation_token.clone();
            let invocation_id = dispatch.invocation_id.clone();
            std::thread::spawn(move || {
                loop {
                    if pass_cancellation.is_cancelled() {
                        let _ = runtime
                            .mcp
                            .cancel_dispatch(&token_id, &server_id, &invocation_id);
                        return;
                    }
                    if scoped_done.is_cancelled() {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            })
        };
        let outcome = dispatcher.execute(&envelope, &cancellation);
        cancellation.cancel();
        self.runtime
            .mcp
            .unregister_dispatch_token(&cancellation_token);
        let _ = watcher.join();
        dispatcher
            .records
            .record_outcome(&outcome)
            .map_err(|_| BrokerError::Unavailable)?;
        Ok(DeliveryAcceptanceV1::Accepted)
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
            .unwrap_or("-")
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
            let allow_write = matches!(
                self.record.binding.limit,
                StoredFileToolLimitV1::Edit { .. } | StoredFileToolLimitV1::Write { .. }
            );
            let files = ProjectFiles::new(FileAuthority {
                root: self.record.workspace.root.clone(),
                allow_write,
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
            match &self.record.binding.limit {
                StoredFileToolLimitV1::Read { maximum_bytes } => {
                    let read = files
                        .read_v1(
                            &FileReadRequestV1 {
                                path: PathBuf::from(&path),
                                maximum_bytes: *maximum_bytes,
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
                                maximum_results: *maximum_results,
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
                StoredFileToolLimitV1::List { maximum_entries } => {
                    let pattern = self.record.call.arguments["pattern"]
                        .as_str()
                        .ok_or_else(|| "glob pattern is invalid".to_owned())?;
                    let list = files
                        .list_v1(
                            &FileListRequestV1 {
                                pattern: pattern.to_owned(),
                                maximum_entries: *maximum_entries,
                            },
                            cancellation,
                        )
                        .map_err(|error| error.to_string())?;
                    let value = json!({
                        "pattern": pattern,
                        "entries": list.entries,
                    });
                    enforce_result_bound(&value)?;
                    Ok((value, format!("Listed {} file(s).", list.entries.len())))
                }
                StoredFileToolLimitV1::Grep {
                    maximum_matches,
                    maximum_files,
                } => {
                    let pattern = self.record.call.arguments["pattern"]
                        .as_str()
                        .ok_or_else(|| "regex pattern is invalid".to_owned())?;
                    let grep = files
                        .grep_v1(
                            &FileGrepRequestV1 {
                                pattern: pattern.to_owned(),
                                maximum_matches: *maximum_matches,
                                maximum_files: *maximum_files,
                                maximum_file_bytes: 1024 * 1024,
                            },
                            cancellation,
                        )
                        .map_err(|error| error.to_string())?;
                    let value = json!({
                        "pattern": pattern,
                        "matches": grep.matches,
                        "filesScanned": grep.files_scanned,
                    });
                    enforce_result_bound(&value)?;
                    Ok((
                        value,
                        format!(
                            "Found {} match(es) across {} file(s).",
                            grep.matches.len(),
                            grep.files_scanned
                        ),
                    ))
                }
                StoredFileToolLimitV1::Edit { .. } => {
                    let old_string = self.record.call.arguments["old_string"]
                        .as_str()
                        .ok_or_else(|| "old_string is invalid".to_owned())?;
                    let new_string = self.record.call.arguments["new_string"]
                        .as_str()
                        .ok_or_else(|| "new_string is invalid".to_owned())?;
                    let current = files
                        .read(PathBuf::from(&path))
                        .map_err(|error| error.to_string())?;
                    let text = String::from_utf8(current.clone())
                        .map_err(|_| "file is not UTF-8 text".to_owned())?;
                    let occurrences = text.match_indices(old_string).count();
                    if occurrences == 0 {
                        return Err("old_string was not found in the file".to_owned());
                    }
                    if occurrences > 1 {
                        return Err("old_string matched more than once; make it unique".to_owned());
                    }
                    let replacement = text.replacen(old_string, new_string, 1).into_bytes();
                    files
                        .edit_hash(
                            &PathBuf::from(&path),
                            &content_hash_local(&current),
                            &replacement,
                        )
                        .map_err(|error| error.to_string())?;
                    let value = json!({
                        "path": path,
                        "oldString": old_string,
                        "newString": new_string,
                        "contentHash": content_hash_local(&replacement),
                        "bytesWritten": replacement.len(),
                    });
                    Ok((
                        value,
                        format!("Edited {} by replacing one occurrence.", path),
                    ))
                }
                StoredFileToolLimitV1::Write { .. } => {
                    let content = self.record.call.arguments["content"]
                        .as_str()
                        .ok_or_else(|| "content is invalid".to_owned())?;
                    let write = files
                        .write_v1(
                            &FileWriteRequestV1 {
                                path: PathBuf::from(&path),
                                content: content.as_bytes().to_vec(),
                                expected_content_hash: None,
                            },
                            cancellation,
                        )
                        .map_err(|error| error.to_string())?;
                    let value = json!({
                        "path": path,
                        "contentHash": write.effect.after_content_hash,
                        "bytesWritten": write.effect.bytes_observed_or_written,
                    });
                    Ok((value, format!("Wrote {} bytes to {}.", content.len(), path)))
                }
                StoredFileToolLimitV1::Shell {
                    timeout_seconds,
                    maximum_output_bytes,
                } => {
                    let command = self.record.call.arguments["command"]
                        .as_str()
                        .ok_or_else(|| "command is invalid".to_owned())?;
                    let tools = BuiltInProcessTools::new(NativeProcessPort);
                    let run = tools
                        .execute_shell(
                            &ShellInvocationV1 {
                                mode: ToolAuthorityModeV1::HostShell,
                                shell_program: shell_program()?,
                                command_text: command.to_owned(),
                                working_directory: Some(self.record.workspace.root.clone()),
                                environment: BTreeMap::new(),
                                limits: HostToolLimitsV1 {
                                    timeout: Duration::from_secs(*timeout_seconds as u64),
                                    maximum_output_bytes: *maximum_output_bytes,
                                    cancellation_grace: Duration::from_millis(100),
                                },
                            },
                            cancellation,
                        )
                        .map_err(|error| error.to_string())?;
                    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
                    let stderr = String::from_utf8_lossy(&run.stderr).into_owned();
                    let value = json!({
                        "stdout": stdout,
                        "stderr": stderr,
                        "exitCode": run.status,
                    });
                    enforce_result_bound(&value)?;
                    Ok((value, format!("Shell exited with status {:?}.", run.status)))
                }
                StoredFileToolLimitV1::Python {
                    timeout_seconds,
                    maximum_output_bytes,
                } => {
                    let script = self.record.call.arguments["script"]
                        .as_str()
                        .ok_or_else(|| "script is invalid".to_owned())?;
                    let tools = BuiltInProcessTools::new(NativeProcessPort);
                    let run = tools
                        .execute_python(
                            &PythonInvocationV1 {
                                mode: ToolAuthorityModeV1::HostPython,
                                interpreter: python_program()?,
                                script: script.to_owned(),
                                arguments: Vec::new(),
                                working_directory: Some(self.record.workspace.root.clone()),
                                environment: BTreeMap::new(),
                                limits: HostToolLimitsV1 {
                                    timeout: Duration::from_secs(*timeout_seconds as u64),
                                    maximum_output_bytes: *maximum_output_bytes,
                                    cancellation_grace: Duration::from_millis(100),
                                },
                            },
                            cancellation,
                        )
                        .map_err(|error| error.to_string())?;
                    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
                    let stderr = String::from_utf8_lossy(&run.stderr).into_owned();
                    let value = json!({
                        "stdout": stdout,
                        "stderr": stderr,
                        "exitCode": run.status,
                    });
                    enforce_result_bound(&value)?;
                    Ok((
                        value,
                        format!("Python exited with status {:?}.", run.status),
                    ))
                }
                StoredFileToolLimitV1::Todo => {
                    let todos = self.record.call.arguments["todos"].clone();
                    self.records
                        .record_todo_state(&self.record.proposal.run_id, &todos)
                        .map_err(|error| error.to_string())?;
                    let value = json!({"todos": todos});
                    Ok((value, "Updated the Run task list.".to_owned()))
                }
                StoredFileToolLimitV1::WebSearch { maximum_results } => {
                    let query = self.record.call.arguments["query"]
                        .as_str()
                        .ok_or_else(|| "query is invalid".to_owned())?;
                    let results = self
                        .web
                        .search_v1(query, *maximum_results, cancellation)
                        .map_err(|error| error.to_string())?;
                    let results_len = results.len();
                    let value = json!({"query": query, "results": results});
                    enforce_result_bound(&value)?;
                    Ok((
                        value,
                        format!("Web search returned {results_len} result(s)."),
                    ))
                }
                StoredFileToolLimitV1::WebFetch {
                    maximum_download_bytes,
                    maximum_extract_bytes,
                } => {
                    let url = self.record.call.arguments["url"]
                        .as_str()
                        .ok_or_else(|| "url is invalid".to_owned())?;
                    let fetched = self
                        .web
                        .fetch_v1(
                            url,
                            *maximum_download_bytes,
                            *maximum_extract_bytes,
                            cancellation,
                        )
                        .map_err(|error| error.to_string())?;
                    let value = json!({
                        "url": fetched.url,
                        "title": fetched.title,
                        "text": fetched.text,
                        "bytesDownloaded": fetched.bytes_downloaded,
                    });
                    enforce_result_bound(&value)?;
                    Ok((value, format!("Fetched {} from the web.", fetched.url)))
                }
                StoredFileToolLimitV1::Mcp {
                    server_id,
                    tool_name,
                    schema_hash,
                } => self.run_mcp_tool(envelope, server_id, tool_name, schema_hash, cancellation),
                StoredFileToolLimitV1::Subagent { maximum_turns } => {
                    self.run_subagent(envelope, *maximum_turns, cancellation)
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
                summary: "Tool operation was denied or failed within its frozen authority.".into(),
            },
        }
    }

    /// Executes one MCP tool call through the frozen per-Run session. The call
    /// settles inside the session manager (session-scoped in-flight bounds and
    /// per-invocation dedup); transport loss or schema drift surfaces as a
    /// failed call and is never replayed. The dispatch-scoped cancellation
    /// token fails the call closed before execution when already cancelled.
    fn run_mcp_tool(
        &self,
        envelope: &ApprovedInvocationEnvelopeV1,
        server_id: &str,
        tool_name: &str,
        schema_hash: &str,
        cancellation: &CancellationToken,
    ) -> Result<(Value, String), String> {
        if cancellation.is_cancelled() {
            return Err("MCP call was cancelled before execution".to_owned());
        }
        let server = stable(server_id).map_err(|error| error.to_string())?;
        let manifest = self.context.mcp_manifests.get(server_id).ok_or_else(|| {
            format!("MCP server '{server_id}' has no frozen manifest for this Run")
        })?;
        self.runtime
            .mcp
            .open_frozen(&self.record.proposal.run_id, manifest)
            .map_err(|error| error.to_string())?;
        let outcome = self
            .runtime
            .mcp
            .invoke(
                &server,
                &McpCallV1 {
                    invocation_id: envelope.invocation_id.clone(),
                    kind: McpCallKindV1::Tool,
                    name: tool_name.to_owned(),
                    expected_schema_hash: Some(schema_hash.to_owned()),
                    arguments: self.record.call.arguments.clone(),
                },
            )
            .map_err(|error| error.to_string())?;
        match outcome.result {
            Some(result) => {
                let value = json!({
                    "server": server_id,
                    "tool": tool_name,
                    "result": result,
                    "progress": outcome.progress,
                });
                enforce_result_bound(&value)?;
                Ok((value, format!("MCP {server_id}/{tool_name} completed.")))
            }
            None => Err(format!(
                "MCP {server_id}/{tool_name} failed: {}",
                mcp_outcome_error(&outcome)
            )),
        }
    }

    /// Runs the bounded subagent child loop: a fresh model/tool conversation
    /// over the same frozen gateway with the read-only, approval-free child
    /// tool subset. The child cannot delegate further (the subagent tool is
    /// excluded from its definitions and port) and every interaction is fenced
    /// by the frozen Run deadline. The child's own tool calls still settle
    /// through the durable broker; its model turns are covered by the parent
    /// pass-level settlement like any other model work.
    fn run_subagent(
        &self,
        envelope: &ApprovedInvocationEnvelopeV1,
        maximum_turns: usize,
        cancellation: &CancellationToken,
    ) -> Result<(Value, String), String> {
        let task = self.record.call.arguments["task"]
            .as_str()
            .ok_or_else(|| "subagent task is invalid".to_owned())?;
        let context_text = self
            .record
            .call
            .arguments
            .get("context")
            .and_then(Value::as_str)
            .unwrap_or("");
        if subagent_deadline_expired(self.record.deadline_epoch_millis) {
            return Err("subagent deadline exceeded before the child run started".to_owned());
        }
        let gateway = self
            .context
            .model_gateway
            .as_ref()
            .ok_or_else(|| "subagent execution requires a frozen model gateway".to_owned())?;
        let binding_id = self
            .context
            .model_binding_id
            .as_ref()
            .ok_or_else(|| "subagent execution requires the frozen model binding".to_owned())?;
        let version_hash = self
            .context
            .model_version_hash
            .as_ref()
            .ok_or_else(|| "subagent execution requires the frozen model version".to_owned())?;
        let definitions = self
            .context
            .bindings
            .iter()
            .filter(|binding| SUBAGENT_CHILD_TOOL_IDS.contains(&binding.capability_id.as_str()))
            .map(StoredFileToolBindingV1::definition)
            .collect::<Vec<_>>();
        let child_authority = SubagentToolPortV1 {
            inner: &BoundFileToolAuthorityV1 {
                runtime: self.runtime.clone(),
                context: self.context.clone(),
                run_events: self.run_events.clone(),
            },
            deadline_epoch_millis: self.record.deadline_epoch_millis,
        };
        let child_input = json!({"messages":[{
            "role":"user",
            "content": format!("{task}\n\nRelevant context:\n{context_text}"),
        }]});
        match execute_model_tool_loop_v1(
            gateway,
            ModelToolLoopRequestV1 {
                outer_invocation_id: &envelope.invocation_id,
                input: child_input,
                parameters: BTreeMap::new(),
                definitions,
                binding_id: binding_id.clone(),
                binding_version_hash: version_hash.clone(),
                maximum_input_bytes: SUBAGENT_MAXIMUM_INPUT_BYTES,
                maximum_output_bytes: SUBAGENT_MAXIMUM_OUTPUT_BYTES,
                // One tool turn plus one final turn is the minimum loop shape;
                // frozen values below that bound follow the same convention
                // as agent nodes and execute with the minimum.
                maximum_turns: u32::try_from(maximum_turns)
                    .map_err(|_| "subagent turn bound is out of range".to_owned())?
                    .max(2),
                maximum_tool_calls: SUBAGENT_MAXIMUM_TOOL_CALLS,
                maximum_tokens: SUBAGENT_MAXIMUM_TOKENS,
                deadline_epoch_millis: self.record.deadline_epoch_millis,
            },
            &child_authority,
            cancellation,
        ) {
            Ok(completed) => {
                let value = json!({
                    "finalText": completed.assistant_text,
                    "modelTurns": completed.attempted_model_turns,
                    "toolCalls": completed.settled_tool_calls,
                    "inputTokens": completed.input_tokens,
                    "outputTokens": completed.output_tokens,
                });
                enforce_result_bound(&value)?;
                Ok((
                    value,
                    format!(
                        "Subagent completed in {} model turn(s) with {} tool call(s).",
                        completed.attempted_model_turns, completed.settled_tool_calls
                    ),
                ))
            }
            Err(failure) => Err(format!("subagent failed: {}", failure.error)),
        }
    }
}

/// Read-only, approval-free tool port for subagent children. The allowed set
/// excludes the subagent tool itself (capping v1 depth at one) and every
/// approval-requiring tool; the frozen Run deadline fences each interaction.
struct SubagentToolPortV1<'a> {
    inner: &'a BoundFileToolAuthorityV1,
    deadline_epoch_millis: u64,
}

impl ModelToolInvocationPortV1 for SubagentToolPortV1<'_> {
    fn invoke(
        &self,
        outer_invocation_id: &StableId,
        turn: u32,
        call: &ModelToolCallV1,
        cancellation: &CancellationToken,
    ) -> Result<SettledModelToolCallV1, String> {
        self.guard(call)?;
        self.inner
            .invoke_v1_scoped(outer_invocation_id, turn, call, cancellation)
            .map_err(|error| error.to_string())
    }

    fn invoke_extended(
        &self,
        outer_invocation_id: &StableId,
        turn: u32,
        call: &ModelToolCallV1,
        cancellation: &CancellationToken,
    ) -> Result<ToolInvokeV1, String> {
        self.guard(call)?;
        match self
            .inner
            .invoke_v1_scoped(outer_invocation_id, turn, call, cancellation)
        {
            Ok(settled) => Ok(ToolInvokeV1::Settled(settled)),
            Err(WorkflowPipelineError::ToolApproval(challenge)) => {
                Ok(ToolInvokeV1::Approval(challenge))
            }
            Err(error) => Err(error.to_string()),
        }
    }

    fn commit_exchange(
        &self,
        outer_invocation_id: &StableId,
        turn: u32,
        exchange: &ModelToolExchangeV1,
    ) -> Result<(), String> {
        self.guard_deadline()?;
        self.inner
            .commit_exchange(outer_invocation_id, turn, exchange)
    }
}

impl SubagentToolPortV1<'_> {
    fn guard(&self, call: &ModelToolCallV1) -> Result<(), String> {
        if !SUBAGENT_CHILD_TOOL_IDS.contains(&call.capability_id.as_str()) {
            return Err("tool is not available to subagent children".to_owned());
        }
        self.guard_deadline()
    }

    fn guard_deadline(&self) -> Result<(), String> {
        if subagent_deadline_expired(self.deadline_epoch_millis) {
            return Err("subagent deadline exceeded".to_owned());
        }
        Ok(())
    }
}

fn shell_program() -> Result<PathBuf, String> {
    #[cfg(windows)]
    let candidate = std::env::var_os("ComSpec")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("SystemRoot")
                .map(PathBuf::from)
                .map(|root| root.join("System32").join("cmd.exe"))
        })
        .ok_or_else(|| "host shell executable is unavailable".to_owned())?;
    #[cfg(not(windows))]
    let candidate = PathBuf::from("/bin/sh");
    stable_executable(candidate, "host shell")
}

fn python_program() -> Result<PathBuf, String> {
    if let Some(configured) = std::env::var_os("AWORKIT_PYTHON_EXECUTABLE") {
        return stable_executable(PathBuf::from(configured), "host Python");
    }
    let names: &[&str] = if cfg!(windows) {
        &["python.exe", "python3.exe"]
    } else {
        &["python3", "python"]
    };
    let path = std::env::var_os("PATH")
        .ok_or_else(|| "host Python executable is unavailable: PATH is empty".to_owned())?;
    for directory in std::env::split_paths(&path) {
        for name in names {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return stable_executable(candidate, "host Python");
            }
        }
    }
    Err("host Python executable is unavailable; set AWORKIT_PYTHON_EXECUTABLE to an absolute interpreter path".into())
}

/// Converts one trusted host-tool executable choice into the exact absolute
/// filesystem identity required by the process authority boundary.
fn stable_executable(candidate: PathBuf, label: &str) -> Result<PathBuf, String> {
    if !candidate.is_absolute() {
        return Err(format!("{label} executable is not an absolute path"));
    }
    std::fs::canonicalize(&candidate).map_err(|error| {
        format!(
            "cannot resolve {label} executable '{}': {error}",
            candidate.display()
        )
    })
}

fn content_hash_local(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
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
    fn open(path: &Path) -> Result<Self, WorkflowPipelineError> {
        Ok(Self {
            store: LocalHistoryStore::open(path).map_err(local_store_error)?,
            write_lock: Arc::new(Mutex::new(())),
        })
    }

    fn record_invocation(
        &self,
        record: &ToolInvocationRecordV1,
    ) -> Result<bool, WorkflowPipelineError> {
        if let Some(existing) = self.invocation(&record.proposal.proposal_id)? {
            return if existing == *record {
                Ok(true)
            } else {
                Err(WorkflowPipelineError::Store(
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

    fn record_outcome(&self, outcome: &ToolOutcomeRecordV1) -> Result<bool, WorkflowPipelineError> {
        if let Some(existing) = self.outcome(&outcome.invocation_id)? {
            return if existing == *outcome {
                Ok(true)
            } else {
                Err(WorkflowPipelineError::Store(
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
    ) -> Result<(), WorkflowPipelineError> {
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
                Err(WorkflowPipelineError::Store(
                    "model/tool exchange identity was reused with changed provider context".into(),
                ))
            };
        }
        self.append("pipeline.model-tool-exchange", &key, value)
    }

    /// Appends one immutable todo-list snapshot for the Run. Later snapshots
    /// supersede earlier ones; the latest event for a Run is the live list.
    fn record_todo_state(
        &self,
        run_id: &StableId,
        todos: &Value,
    ) -> Result<(), WorkflowPipelineError> {
        let key = digest_id(
            "record.todo-state",
            &format!("{}:{}", run_id.as_str(), canonical_hash(todos)?),
        )?;
        self.append(
            "pipeline.todo-state",
            &key,
            json!({"schemaVersion": 1, "runId": run_id, "todos": todos}),
        )
    }

    /// Latest immutable todo-list snapshot for the Run; the newest recorded
    /// event is the live task list shown in the UI.
    pub(crate) fn todo_state(
        &self,
        run_id: &StableId,
    ) -> Result<Option<Value>, WorkflowPipelineError> {
        Ok(self
            .events("pipeline.todo-state")?
            .into_iter()
            .filter(|value| value.get("runId").and_then(Value::as_str) == Some(run_id.as_str()))
            .last()
            .and_then(|value| value.get("todos").cloned()))
    }

    fn invocation(
        &self,
        proposal_id: &StableId,
    ) -> Result<Option<ToolInvocationRecordV1>, WorkflowPipelineError> {
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
    ) -> Result<Option<ToolInvocationRecordV1>, WorkflowPipelineError> {
        self.invocation(&dispatch.proposal_id)
    }

    fn outcome(
        &self,
        invocation_id: &StableId,
    ) -> Result<Option<ToolOutcomeRecordV1>, WorkflowPipelineError> {
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

    fn events(&self, kind: &str) -> Result<Vec<Value>, WorkflowPipelineError> {
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
    ) -> Result<(), WorkflowPipelineError> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| WorkflowPipelineError::Store("tool record lock poisoned".into()))?;
        let head = self
            .store
            .events(TOOL_RECORD_CHAT_ID, STORE_BRANCH_ID)
            .map_err(local_store_error)?
            .len();
        let expected_head = u64::try_from(head)
            .map_err(|_| WorkflowPipelineError::Store("tool record sequence exhausted".into()))?;
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
) -> Result<(), WorkflowPipelineError> {
    let object = arguments
        .as_object()
        .ok_or_else(|| invalid_tool("tool arguments must be an object"))?;
    let expected_keys: BTreeSet<&str> = match binding.limit {
        StoredFileToolLimitV1::Read { .. } => BTreeSet::from(["path"]),
        StoredFileToolLimitV1::Search { .. } => BTreeSet::from(["path", "query"]),
        StoredFileToolLimitV1::List { .. } => BTreeSet::from(["pattern"]),
        StoredFileToolLimitV1::Grep { .. } => BTreeSet::from(["pattern"]),
        StoredFileToolLimitV1::Edit { .. } => BTreeSet::from(["path", "old_string", "new_string"]),
        StoredFileToolLimitV1::Write { .. } => BTreeSet::from(["path", "content"]),
        StoredFileToolLimitV1::Shell { .. } => BTreeSet::from(["command"]),
        StoredFileToolLimitV1::Python { .. } => BTreeSet::from(["script"]),
        StoredFileToolLimitV1::Todo => BTreeSet::from(["todos"]),
        StoredFileToolLimitV1::WebSearch { .. } => BTreeSet::from(["query"]),
        StoredFileToolLimitV1::WebFetch { .. } => BTreeSet::from(["url"]),
        StoredFileToolLimitV1::Subagent { .. } => BTreeSet::from(["task", "context"]),
        // MCP argument shapes are server-defined; the frozen validator only
        // bounds the payload. The session layer enforces the exact discovered
        // schema hash before the peer sees the call.
        StoredFileToolLimitV1::Mcp { .. } => return validate_mcp_arguments(arguments),
    };
    let observed_keys = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let valid_keys = if matches!(binding.limit, StoredFileToolLimitV1::Subagent { .. }) {
        // The subagent context slice is optional; the task is required.
        observed_keys.is_subset(&expected_keys) && observed_keys.contains("task")
    } else {
        observed_keys == expected_keys
    };
    if !valid_keys {
        return Err(invalid_tool(
            "tool arguments contain missing or unknown fields",
        ));
    }
    if let Some(path) = object.get("path").and_then(Value::as_str) {
        if path.is_empty()
            || path.len() > 4096
            || path.contains('\0')
            || Path::new(path).is_absolute()
        {
            return Err(invalid_tool(
                "tool path must be a bounded relative path inside the frozen project root",
            ));
        }
    }
    match binding.limit {
        StoredFileToolLimitV1::Search { .. } => {
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
        StoredFileToolLimitV1::List { .. } => {
            object
                .get("pattern")
                .and_then(Value::as_str)
                .filter(|pattern| !pattern.is_empty() && pattern.len() <= 4096)
                .ok_or_else(|| invalid_tool("glob pattern is empty or oversized"))?;
        }
        StoredFileToolLimitV1::Grep { .. } => {
            object
                .get("pattern")
                .and_then(Value::as_str)
                .filter(|pattern| !pattern.is_empty() && pattern.len() <= 16_384)
                .ok_or_else(|| invalid_tool("regex pattern is empty or oversized"))?;
        }
        StoredFileToolLimitV1::Edit { .. } => {
            for field in ["old_string", "new_string"] {
                object
                    .get(field)
                    .and_then(Value::as_str)
                    .filter(|value| {
                        value.len() <= MAXIMUM_TOOL_PAYLOAD_BYTES && !value.contains('\0')
                    })
                    .ok_or_else(|| {
                        invalid_tool("edit strings are empty, oversized, or malformed")
                    })?;
            }
        }
        StoredFileToolLimitV1::Write { .. } => {
            object
                .get("content")
                .and_then(Value::as_str)
                .filter(|value| value.len() <= 1024 * 1024 && !value.contains('\0'))
                .ok_or_else(|| invalid_tool("file content is empty, oversized, or malformed"))?;
        }
        StoredFileToolLimitV1::Shell { .. } => {
            object
                .get("command")
                .and_then(Value::as_str)
                .filter(|value| {
                    !value.is_empty() && value.len() <= 256 * 1024 && !value.contains('\0')
                })
                .ok_or_else(|| invalid_tool("shell command is empty, oversized, or malformed"))?;
        }
        StoredFileToolLimitV1::Python { .. } => {
            object
                .get("script")
                .and_then(Value::as_str)
                .filter(|value| {
                    !value.is_empty() && value.len() <= 256 * 1024 && !value.contains('\0')
                })
                .ok_or_else(|| invalid_tool("python script is empty, oversized, or malformed"))?;
        }
        StoredFileToolLimitV1::Todo => {
            let todos = object
                .get("todos")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid_tool("todo list must be an array"))?;
            if todos.len() > 64 {
                return Err(invalid_tool("todo list exceeds 64 items"));
            }
            for todo in todos {
                let todo = todo
                    .as_object()
                    .ok_or_else(|| invalid_tool("todo items must be objects"))?;
                let content = todo
                    .get("content")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty() && value.len() <= 4096)
                    .ok_or_else(|| invalid_tool("todo content is empty or oversized"))?;
                if !matches!(
                    todo.get("status").and_then(Value::as_str),
                    Some("pending" | "completed")
                ) {
                    return Err(invalid_tool("todo status must be pending or completed"));
                }
                let keys = todo.keys().map(String::as_str).collect::<BTreeSet<_>>();
                if keys != BTreeSet::from(["content", "status"]) {
                    return Err(invalid_tool("todo items accept exactly content and status"));
                }
                let _ = content;
            }
        }
        StoredFileToolLimitV1::WebSearch { .. } => {
            object
                .get("query")
                .and_then(Value::as_str)
                .filter(|query| !query.is_empty() && query.len() <= 16_384)
                .ok_or_else(|| invalid_tool("web search query is empty or oversized"))?;
        }
        StoredFileToolLimitV1::WebFetch { .. } => {
            object
                .get("url")
                .and_then(Value::as_str)
                .filter(|url| !url.is_empty() && url.len() <= 4096)
                .ok_or_else(|| invalid_tool("web fetch url is empty or oversized"))?;
        }
        StoredFileToolLimitV1::Subagent { .. } => {
            object
                .get("task")
                .and_then(Value::as_str)
                .filter(|task| {
                    !task.is_empty()
                        && task.len() <= SUBAGENT_MAXIMUM_TASK_BYTES
                        && !task.contains('\0')
                })
                .ok_or_else(|| invalid_tool("subagent task is empty, oversized, or malformed"))?;
            if let Some(context) = object.get("context") {
                context
                    .as_str()
                    .filter(|context| {
                        context.len() <= SUBAGENT_MAXIMUM_CONTEXT_BYTES && !context.contains('\0')
                    })
                    .ok_or_else(|| invalid_tool("subagent context is oversized or malformed"))?;
            }
        }
        StoredFileToolLimitV1::Read { .. } => {}
        // MCP argument payloads were bounded in the shape check above; the
        // server schema is enforced by the session layer at call time.
        StoredFileToolLimitV1::Mcp { .. } => {}
    }
    Ok(())
}

/// Bounds an MCP call's arguments without imposing the server schema, which
/// the session layer enforces through the pinned discovery hash.
fn validate_mcp_arguments(arguments: &Value) -> Result<(), WorkflowPipelineError> {
    let object = arguments
        .as_object()
        .ok_or_else(|| invalid_tool("MCP tool arguments must be an object"))?;
    if serde_json::to_vec(object).map_or(true, |bytes| bytes.len() > MAXIMUM_TOOL_PAYLOAD_BYTES) {
        return Err(invalid_tool(
            "MCP tool arguments exceed the 256 KiB payload bound",
        ));
    }
    if object.values().any(Value::is_null) {
        return Err(invalid_tool("MCP tool arguments must not contain null"));
    }
    Ok(())
}

fn exact_unsigned_configuration(
    configuration: &Value,
    fixed: &[(&str, Value)],
    numeric_name: &str,
    minimum: u64,
    maximum: u64,
) -> Result<usize, WorkflowPipelineError> {
    Ok(
        *freeze_configuration(configuration, fixed, &[(numeric_name, minimum, maximum)])?
            .get(numeric_name)
            .expect("frozen numeric field"),
    )
}

/// Validates the exact frozen Settings shape for one tool: every fixed field
/// must equal its declared value and every numeric field must fall inside its
/// declared range. Unknown or missing keys fail closed.
fn freeze_configuration(
    configuration: &Value,
    fixed: &[(&str, Value)],
    numerics: &[(&str, u64, u64)],
) -> Result<BTreeMap<String, usize>, WorkflowPipelineError> {
    let object = configuration
        .as_object()
        .ok_or_else(|| invalid_tool("tool configuration must be an object"))?;
    if object.len() != fixed.len() + numerics.len()
        || fixed
            .iter()
            .any(|(name, expected)| object.get(*name) != Some(expected))
    {
        return Err(invalid_tool(
            "tool configuration does not match the installed native adapter contract",
        ));
    }
    let mut frozen = BTreeMap::new();
    for (name, minimum, maximum) in numerics {
        let value = object
            .get(*name)
            .and_then(Value::as_u64)
            .filter(|value| (*minimum..=*maximum).contains(value))
            .ok_or_else(|| invalid_tool("tool configuration limit is invalid"))?;
        frozen.insert(
            (*name).to_owned(),
            usize::try_from(value)
                .map_err(|_| invalid_tool("tool configuration limit is invalid"))?,
        );
    }
    Ok(frozen)
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

fn file_list_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "pattern": {"type":"string","minLength":1,"maxLength":4096}
        },
        "required": ["pattern"]
    })
}

fn file_grep_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "pattern": {"type":"string","minLength":1,"maxLength":16384}
        },
        "required": ["pattern"]
    })
}

fn file_edit_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": {"type":"string","minLength":1,"maxLength":4096},
            "old_string": {"type":"string","minLength":1,"maxLength":262144},
            "new_string": {"type":"string","maxLength":262144}
        },
        "required": ["path", "old_string", "new_string"]
    })
}

fn file_write_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": {"type":"string","minLength":1,"maxLength":4096},
            "content": {"type":"string","minLength":1,"maxLength":1048576}
        },
        "required": ["path", "content"]
    })
}

fn shell_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "command": {"type":"string","minLength":1,"maxLength":262144}
        },
        "required": ["command"]
    })
}

fn python_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "script": {"type":"string","minLength":1,"maxLength":262144}
        },
        "required": ["script"]
    })
}

fn todo_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "todos": {
                "type": "array",
                "maxItems": 64,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "content": {"type":"string","minLength":1,"maxLength":4096},
                        "status": {"enum":["pending","completed"]}
                    },
                    "required": ["content","status"]
                }
            }
        },
        "required": ["todos"]
    })
}

fn web_search_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "query": {"type":"string","minLength":1,"maxLength":16384}
        },
        "required": ["query"]
    })
}

fn web_fetch_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "url": {"type":"string","minLength":1,"maxLength":4096}
        },
        "required": ["url"]
    })
}

fn subagent_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "task": {"type":"string","minLength":1,"maxLength":16384},
            "context": {"type":"string","maxLength":32768}
        },
        "required": ["task"]
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
) -> Result<(), WorkflowPipelineError> {
    expected_branch.map_or(Ok(()), |expected| {
        revalidate_git_branch(&workspace.root, expected).map_err(WorkflowPipelineError::Authority)
    })
}

fn scope_for(capability_id: &str) -> &'static str {
    match capability_id {
        FILE_READ_CAPABILITY_ID => FILE_READ_SCOPE,
        FILE_SEARCH_CAPABILITY_ID => FILE_SEARCH_SCOPE,
        FILE_LIST_CAPABILITY_ID => FILE_LIST_SCOPE,
        FILE_GREP_CAPABILITY_ID => FILE_GREP_SCOPE,
        FILE_EDIT_CAPABILITY_ID => FILE_EDIT_SCOPE,
        FILE_WRITE_CAPABILITY_ID => FILE_WRITE_SCOPE,
        SHELL_CAPABILITY_ID => SHELL_SCOPE,
        PYTHON_CAPABILITY_ID => PYTHON_SCOPE,
        TODO_CAPABILITY_ID => TODO_SCOPE,
        WEB_SEARCH_CAPABILITY_ID => WEB_SEARCH_SCOPE,
        WEB_FETCH_CAPABILITY_ID => WEB_FETCH_SCOPE,
        SUBAGENT_CAPABILITY_ID => SUBAGENT_SCOPE,
        id if id.starts_with(MCP_CAPABILITY_PREFIX) => MCP_SCOPE,
        _ => "invalid",
    }
}

/// Maps a settled MCP outcome without a result payload to a model-safe error
/// sentence. The session manager already committed the exact effect class;
/// this text never claims knowledge the evidence does not contain.
fn mcp_outcome_error(outcome: &McpCallOutcomeV1) -> String {
    match outcome.outcome.disposition {
        OutcomeDispositionV1::FailedDefiniteNotStarted => {
            "the call definitely did not start".to_owned()
        }
        OutcomeDispositionV1::FailedKnownStarted => "the call started but failed".to_owned(),
        OutcomeDispositionV1::CancelledWithEvidence => "the call was cancelled".to_owned(),
        OutcomeDispositionV1::OutcomeUncertain => {
            "the call outcome is uncertain and will not be replayed".to_owned()
        }
        OutcomeDispositionV1::Succeeded => {
            "the server reported success without a result payload".to_owned()
        }
    }
}

fn current_epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Fails closed once the frozen Run deadline has passed; every subagent
/// child interaction is fenced through this predicate.
fn subagent_deadline_expired(deadline_epoch_millis: u64) -> bool {
    current_epoch_millis() >= deadline_epoch_millis
}

fn canonical_hash<T: Serialize>(value: &T) -> Result<String, WorkflowPipelineError> {
    let bytes = serde_jcs::to_vec(value).map_err(json_error)?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn digest_id(prefix: &str, material: &str) -> Result<StableId, WorkflowPipelineError> {
    let digest = format!("{:x}", Sha256::digest(material.as_bytes()));
    stable(&format!("{prefix}.{}", &digest[..40]))
}

fn stable(value: &str) -> Result<StableId, WorkflowPipelineError> {
    StableId::parse(value.to_owned())
        .map_err(|error| WorkflowPipelineError::InvalidInput(error.to_string()))
}

fn invalid_tool(message: &str) -> WorkflowPipelineError {
    WorkflowPipelineError::InvalidInput(message.to_owned())
}

fn broker_error(error: BrokerError) -> WorkflowPipelineError {
    WorkflowPipelineError::Broker(error.to_string())
}

fn local_store_error(error: StoreError) -> WorkflowPipelineError {
    WorkflowPipelineError::Store(error.to_string())
}

fn json_error(error: impl std::fmt::Display) -> WorkflowPipelineError {
    WorkflowPipelineError::Store(error.to_string())
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
    ) -> Result<AuthorityManifestV1, WorkflowPipelineError> {
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
        let tool = freeze_file_tool_bindings(&[WorkflowToolBindingV1 {
            capability_id: FILE_READ_CAPABILITY_ID.into(),
            configuration: json!({
                "authorityMode":"project_files",
                "effect":"read",
                "maximumBytes":PROJECT_FILE_READ_MAXIMUM_BYTES_V1,
            }),
            definition: None,
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
            request_id: stable("command.tool-run-a").expect("request A"),
            node_id: stable("agent.1").expect("node"),
            workspace: projects
                .resolve_workspace_v1(&workspace_a)
                .expect("binding A"),
            project_branch: None,
            bindings: vec![tool.clone()],
            deadline_epoch_millis: current_epoch_millis().saturating_add(60_000),
            model_gateway: None,
            model_binding_id: None,
            model_version_hash: None,
            mcp_manifests: BTreeMap::new(),
            cancellation: CancellationToken::default(),
        });
        let authority_b = runtime.bind(FrozenFileToolAuthorityContextV1 {
            manifest: manifest("manifest.tool-run-b", capability_binding.clone())
                .expect("manifest B"),
            run_id: stable("run.tool-run-b").expect("run B"),
            request_id: stable("command.tool-run-b").expect("request B"),
            node_id: stable("agent.1").expect("node"),
            workspace: projects
                .resolve_workspace_v1(&workspace_b)
                .expect("binding B"),
            project_branch: None,
            bindings: vec![tool.clone()],
            deadline_epoch_millis: current_epoch_millis().saturating_add(60_000),
            model_gateway: None,
            model_binding_id: None,
            model_version_hash: None,
            mcp_manifests: BTreeMap::new(),
            cancellation: CancellationToken::default(),
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

        let expired_authority = runtime.bind(FrozenFileToolAuthorityContextV1 {
            manifest: manifest("manifest.tool-run-expired", capability_binding.clone())
                .expect("expired manifest"),
            run_id: stable("run.tool-run-expired").expect("expired run"),
            request_id: stable("command.tool-run-expired").expect("expired request"),
            node_id: stable("agent.1").expect("node"),
            workspace: projects
                .resolve_workspace_v1(&workspace_a)
                .expect("expired binding"),
            project_branch: None,
            bindings: vec![tool.clone()],
            deadline_epoch_millis: current_epoch_millis().saturating_sub(1),
            model_gateway: None,
            model_binding_id: None,
            model_version_hash: None,
            mcp_manifests: BTreeMap::new(),
            cancellation: CancellationToken::default(),
        });
        let expired_error = expired_authority
            .invoke_v1(
                &stable("invocation.outer-tool-run-expired").expect("expired outer"),
                1,
                &read_call("call.tool-run-expired", "notes.txt"),
                &CancellationToken::default(),
            )
            .expect_err("expired dispatch must fail before execution");
        assert!(matches!(expired_error, WorkflowPipelineError::Host(_)));
        assert!(
            expired_error
                .to_string()
                .contains("deadline elapsed before execution started")
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
            request_id: stable("command.tool-run-c").expect("request C"),
            node_id: stable("agent.1").expect("node"),
            workspace: projects
                .resolve_workspace_v1(&workspace_c)
                .expect("binding C"),
            project_branch: Some("feature/frozen".into()),
            bindings: vec![tool],
            deadline_epoch_millis: current_epoch_millis().saturating_add(60_000),
            model_gateway: None,
            model_binding_id: None,
            model_version_hash: None,
            mcp_manifests: BTreeMap::new(),
            cancellation: CancellationToken::default(),
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
            context: authority_c.context.clone(),
            run_events: authority_c.run_events.clone(),
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
        let binding = freeze_file_tool_bindings(&[WorkflowToolBindingV1 {
            capability_id: FILE_SEARCH_CAPABILITY_ID.into(),
            configuration: json!({
                "authorityMode":"project_files",
                "effect":"search",
                "maximumResults":PROJECT_FILE_SEARCH_MAXIMUM_RESULTS_V1,
            }),
            definition: None,
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

    #[test]
    fn subagent_deadline_predicate_fails_closed_once_the_run_deadline_passes() {
        assert!(!subagent_deadline_expired(
            current_epoch_millis().saturating_add(60_000)
        ));
        assert!(subagent_deadline_expired(
            current_epoch_millis().saturating_sub(1)
        ));
    }

    #[test]
    fn subagent_binding_freezes_the_exact_approval_contract() {
        let binding = freeze_file_tool_bindings(&[WorkflowToolBindingV1 {
            capability_id: SUBAGENT_CAPABILITY_ID.into(),
            configuration: json!({
                "authorityMode":"run_subagent",
                "requiresApproval":true,
                "maximumTurns":4,
            }),
            definition: None,
        }])
        .expect("subagent binding")
        .remove(0);
        assert_eq!(
            binding.limit,
            StoredFileToolLimitV1::Subagent { maximum_turns: 4 }
        );
        assert!(binding.requires_approval);
        assert!(matches!(
            freeze_file_tool_bindings(&[WorkflowToolBindingV1 {
                capability_id: SUBAGENT_CAPABILITY_ID.into(),
                configuration: json!({
                    "authorityMode":"run_subagent",
                    "maximumTurns":4,
                }),
                definition: None,
            }]),
            Err(WorkflowPipelineError::InvalidInput(_))
        ));
    }

    fn mcp_binding_request(definition: Option<ModelToolDefinitionV1>) -> WorkflowToolBindingV1 {
        WorkflowToolBindingV1 {
            capability_id: "mcp://serv.fixture/echo".into(),
            configuration: json!({"serverId":"serv.fixture","tool":"echo"}),
            definition,
        }
    }

    fn mcp_definition(server: &str, tool: &str) -> ModelToolDefinitionV1 {
        ModelToolDefinitionV1 {
            capability_id: format!("mcp://{server}/{tool}"),
            name: mcp_provider_name(server, tool),
            description: format!("Call MCP tool '{tool}' on server '{server}'."),
            input_schema: json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}),
        }
    }

    #[test]
    fn mcp_binding_freezes_the_two_key_contract_with_a_digest_internal_id() {
        let binding = freeze_file_tool_bindings(&[mcp_binding_request(Some(mcp_definition(
            "serv.fixture",
            "echo",
        )))])
        .expect("mcp binding")
        .remove(0);
        assert_eq!(binding.capability_id, "mcp://serv.fixture/echo");
        assert_eq!(binding.provider_name, "mcp__serv_fixture__echo");
        assert!(
            !binding.requires_approval,
            "MCP tools never suspend for approval"
        );
        assert_eq!(
            binding.limit,
            StoredFileToolLimitV1::Mcp {
                server_id: "serv.fixture".into(),
                tool_name: "echo".into(),
                schema_hash: mcp_schema_hash(&json!({
                    "type":"object",
                    "properties":{"text":{"type":"string"}},
                    "required":["text"]
                })),
            }
        );
        assert_eq!(
            binding.internal_id,
            mcp_internal_id("mcp://serv.fixture/echo")
        );
        assert!(StableId::parse(binding.internal_id.clone()).is_ok());
        assert_ne!(binding.internal_id, binding.capability_id);
    }

    #[test]
    fn mcp_binding_without_a_definition_generates_the_permissive_fallback() {
        let binding = freeze_file_tool_bindings(&[mcp_binding_request(None)])
            .expect("mcp fallback binding")
            .remove(0);
        assert_eq!(binding.provider_name, "mcp__serv_fixture__echo");
        assert_eq!(
            binding.description,
            "Call MCP tool 'echo' on server 'serv.fixture'."
        );
        assert_eq!(
            binding.input_schema,
            json!({"type":"object","additionalProperties":true})
        );
        assert_eq!(
            binding.limit,
            StoredFileToolLimitV1::Mcp {
                server_id: "serv.fixture".into(),
                tool_name: "echo".into(),
                schema_hash: mcp_schema_hash(&json!({"type":"object","additionalProperties":true})),
            }
        );
    }

    #[test]
    fn mcp_binding_rejects_unknown_configuration_keys() {
        let mut request = mcp_binding_request(None);
        request.configuration = json!({"serverId":"serv.fixture","tool":"echo","extra":true});
        assert!(matches!(
            freeze_file_tool_bindings(&[request]),
            Err(WorkflowPipelineError::InvalidInput(_))
        ));
    }

    #[test]
    fn mcp_binding_rejects_capability_configuration_mismatch() {
        let mut request = mcp_binding_request(None);
        request.configuration = json!({"serverId":"serv.other","tool":"echo"});
        assert!(matches!(
            freeze_file_tool_bindings(&[request]),
            Err(WorkflowPipelineError::InvalidInput(_))
        ));
        let mut request = mcp_binding_request(None);
        request.configuration = json!({"serverId":"serv.fixture","tool":"other"});
        assert!(matches!(
            freeze_file_tool_bindings(&[request]),
            Err(WorkflowPipelineError::InvalidInput(_))
        ));
    }

    #[test]
    fn mcp_binding_rejects_a_definition_for_a_different_capability() {
        let mut definition = mcp_definition("serv.fixture", "echo");
        definition.capability_id = "mcp://serv.other/echo".into();
        let request = mcp_binding_request(Some(definition));
        assert!(matches!(
            freeze_file_tool_bindings(&[request]),
            Err(WorkflowPipelineError::InvalidInput(_))
        ));
    }
}
