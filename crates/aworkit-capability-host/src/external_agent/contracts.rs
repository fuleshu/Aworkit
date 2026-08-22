//! Aworkit-owned lifecycle contracts for configured external-agent adapters.

use aworkit_protocol::{ProcessGeneration, StableId};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use thiserror::Error;

use crate::{CapabilityOutcomeV1, ForwardableMcpSetV1};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalAgentProtocolV1 {
    CodexAppServer,
    AgentClientProtocol,
}

/// Exact configured and core-attested target identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalAgentManifestV1 {
    pub target_id: StableId,
    pub adapter_version: String,
    pub binding_hash: String,
    pub host_generation: ProcessGeneration,
    pub configured: bool,
    pub enabled: bool,
    pub core_attested: bool,
    pub protocol: ExternalAgentProtocolV1,
    pub maximum_active_sessions: usize,
    pub maximum_progress_events: usize,
    pub allowed_workspace_roots: Vec<String>,
    pub allowed_mcp_server_ids: Vec<String>,
    pub secret_slots: Vec<String>,
}

/// Features observed during the adapter's native initialization handshake.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalAgentCapabilitySetV1 {
    pub progress: bool,
    pub native_sessions: bool,
    pub continuation: bool,
    pub cancellation: bool,
    pub approval_requests: bool,
    pub steering: bool,
    pub artifacts: bool,
    pub selected_mcp_forwarding: bool,
}

/// Source visibility is reported, never upgraded by Aworkit.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalAgentVisibilityV1 {
    FullLifecycle,
    PartialLifecycle,
    OpaqueRemote,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalAgentNegotiationV1 {
    pub target_id: StableId,
    pub host_generation: ProcessGeneration,
    pub capabilities: ExternalAgentCapabilitySetV1,
    pub visibility: ExternalAgentVisibilityV1,
    pub protocol_version: String,
}

/// A public Aworkit reference. The adapter-native session value stays private
/// inside the manager and is never treated as canonical history by the host.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeSessionRefV1 {
    pub target_id: StableId,
    pub host_generation: ProcessGeneration,
    pub reference_hash: String,
}

/// Explicit approved start input; it contains no plaintext credential values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalAgentStartV1 {
    pub invocation_id: StableId,
    pub task: String,
    pub desired_result: String,
    pub workspace_roots: Vec<String>,
    pub deadline_epoch_millis: u64,
    pub maximum_turns: u32,
    pub lease_handles: Vec<StableId>,
    pub forwarded_mcp: Option<ForwardableMcpSetV1>,
}

/// Explicit approved continuation of one retained native session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalAgentContinueV1 {
    pub invocation_id: StableId,
    pub native_session: NativeSessionRefV1,
    pub input: String,
    pub deadline_epoch_millis: u64,
    pub expected_cursor: Option<String>,
    pub forwarded_mcp: Option<ForwardableMcpSetV1>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ExternalAgentRawContentV1 {
    AssistantOutput(String),
    Progress(String),
    ReasoningRaw(String),
    ReasoningSummary(String),
    ArtifactReference(String),
    Diagnostic(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalAgentRawEventV1 {
    pub sequence: u64,
    pub content: ExternalAgentRawContentV1,
}

/// Native approval request normalized without granting it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalApprovalRequestV1 {
    pub request_id: StableId,
    pub invocation_id: StableId,
    pub summary: String,
    pub requested_scopes: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalApprovalDecisionV1 {
    Approved,
    Denied,
}

/// Core-authenticated approval resolution fenced to one native session and
/// one active invocation. Fields are private so callers cannot construct an
/// unauthenticated in-process grant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalApprovalResolutionV1 {
    request_id: StableId,
    decision_id: StableId,
    host_generation: ProcessGeneration,
    native_session: NativeSessionRefV1,
    invocation_id: StableId,
    decision: ExternalApprovalDecisionV1,
    granted_scopes: Vec<String>,
    core_authentication_tag: String,
}

impl ExternalApprovalResolutionV1 {
    pub fn issue(
        authentication_key: &[u8],
        request_id: StableId,
        decision_id: StableId,
        host_generation: ProcessGeneration,
        native_session: NativeSessionRefV1,
        invocation_id: StableId,
        decision: ExternalApprovalDecisionV1,
        granted_scopes: Vec<String>,
    ) -> Result<Self, ExternalApprovalAuthenticationErrorV1> {
        if authentication_key.len() < 32 {
            return Err(ExternalApprovalAuthenticationErrorV1::InvalidKey);
        }
        let mut value = Self {
            request_id,
            decision_id,
            host_generation,
            native_session,
            invocation_id,
            decision,
            granted_scopes,
            core_authentication_tag: String::new(),
        };
        value.core_authentication_tag = value.authentication_tag(authentication_key)?;
        Ok(value)
    }

    pub(crate) fn verify(
        &self,
        authentication_key: &[u8],
    ) -> Result<(), ExternalApprovalAuthenticationErrorV1> {
        let mut mac = Hmac::<Sha256>::new_from_slice(authentication_key)
            .map_err(|_| ExternalApprovalAuthenticationErrorV1::InvalidKey)?;
        let supplied = decode_tag(&self.core_authentication_tag)?;
        mac.update(&self.authentication_bytes()?);
        mac.verify_slice(&supplied)
            .map_err(|_| ExternalApprovalAuthenticationErrorV1::Authentication)
    }

    #[must_use]
    pub fn request_id(&self) -> &StableId {
        &self.request_id
    }

    #[must_use]
    pub fn decision_id(&self) -> &StableId {
        &self.decision_id
    }

    #[must_use]
    pub fn host_generation(&self) -> ProcessGeneration {
        self.host_generation
    }

    #[must_use]
    pub fn native_session(&self) -> &NativeSessionRefV1 {
        &self.native_session
    }

    #[must_use]
    pub fn invocation_id(&self) -> &StableId {
        &self.invocation_id
    }

    #[must_use]
    pub fn decision(&self) -> ExternalApprovalDecisionV1 {
        self.decision
    }

    #[must_use]
    pub fn granted_scopes(&self) -> &[String] {
        &self.granted_scopes
    }

    fn authentication_tag(
        &self,
        authentication_key: &[u8],
    ) -> Result<String, ExternalApprovalAuthenticationErrorV1> {
        let mut mac = Hmac::<Sha256>::new_from_slice(authentication_key)
            .map_err(|_| ExternalApprovalAuthenticationErrorV1::InvalidKey)?;
        mac.update(&self.authentication_bytes()?);
        Ok(format!("hmac-sha256:{:x}", mac.finalize().into_bytes()))
    }

    fn authentication_bytes(&self) -> Result<Vec<u8>, ExternalApprovalAuthenticationErrorV1> {
        serde_json::to_vec(&(
            &self.request_id,
            &self.decision_id,
            self.host_generation,
            &self.native_session,
            &self.invocation_id,
            self.decision,
            &self.granted_scopes,
        ))
        .map_err(|_| ExternalApprovalAuthenticationErrorV1::Encoding)
    }
}

fn decode_tag(value: &str) -> Result<[u8; 32], ExternalApprovalAuthenticationErrorV1> {
    let digest = value
        .strip_prefix("hmac-sha256:")
        .ok_or(ExternalApprovalAuthenticationErrorV1::Authentication)?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ExternalApprovalAuthenticationErrorV1::Authentication);
    }
    let mut output = [0_u8; 32];
    for (index, byte) in output.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&digest[index * 2..index * 2 + 2], 16)
            .map_err(|_| ExternalApprovalAuthenticationErrorV1::Authentication)?;
    }
    Ok(output)
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ExternalApprovalAuthenticationErrorV1 {
    #[error("external approval authentication key is invalid")]
    InvalidKey,
    #[error("external approval resolution encoding failed")]
    Encoding,
    #[error("external approval resolution authentication failed")]
    Authentication,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalTerminalStatusV1 {
    Succeeded,
    Failed,
    CancelledWithEvidence,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalTerminalV1 {
    pub status: ExternalTerminalStatusV1,
    pub result: Option<Value>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalEffectEvidenceV1 {
    DefinitelyNotStarted,
    Started,
    Unknown,
}

/// One native lifecycle update. Native IDs are consumed only by the manager.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalAgentPeerUpdateV1 {
    pub invocation_id: StableId,
    pub native_session_id: String,
    pub continuation_cursor: Option<String>,
    pub events: Vec<ExternalAgentRawEventV1>,
    pub approval_request: Option<ExternalApprovalRequestV1>,
    pub terminal: Option<ExternalTerminalV1>,
    pub effect: ExternalEffectEvidenceV1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalDispatchMilestoneV1 {
    DefinitelyNotStarted,
    Started,
    Unknown,
}

#[derive(Clone, Debug, Eq, Error, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[error("{code}: {message}")]
pub struct ExternalAgentPeerErrorV1 {
    pub code: String,
    pub message: String,
    pub dispatch: ExternalDispatchMilestoneV1,
    pub native_session_lost: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExternalCancellationEvidenceV1 {
    ConfirmedBeforeEffect,
    ConfirmedAfterStart,
    Refused,
    Unsupported,
    Unknown,
}

/// Public lifecycle update returned through the invocation gateway.
#[derive(Clone, Debug, PartialEq)]
pub struct ExternalAgentUpdateV1 {
    pub native_session: NativeSessionRefV1,
    pub continuation_cursor: Option<String>,
    pub events: Vec<ExternalAgentRawEventV1>,
    pub approval_request: Option<ExternalApprovalRequestV1>,
    pub terminal: Option<CapabilityOutcomeV1>,
    pub result: Option<Value>,
    pub visibility: ExternalAgentVisibilityV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalAgentHealthV1 {
    pub target_id: StableId,
    pub host_generation: ProcessGeneration,
    pub active_sessions: usize,
    pub reserved_sessions: usize,
    pub sessions_requiring_close: usize,
    pub maximum_active_sessions: usize,
    pub degraded: bool,
    pub capabilities: ExternalAgentCapabilitySetV1,
    pub visibility: ExternalAgentVisibilityV1,
}

/// Adapter-native operations. Native history/session objects never cross this
/// boundary; only the bounded strings and Aworkit DTOs above do.
pub trait ExternalAgentPeerPort: Send + Sync {
    fn negotiate(
        &self,
        manifest: &ExternalAgentManifestV1,
    ) -> Result<ExternalAgentNegotiationV1, ExternalAgentPeerErrorV1>;

    fn start(
        &self,
        manifest: &ExternalAgentManifestV1,
        request: &ExternalAgentStartV1,
    ) -> Result<ExternalAgentPeerUpdateV1, ExternalAgentPeerErrorV1>;

    fn continue_session(
        &self,
        manifest: &ExternalAgentManifestV1,
        native_session_id: &str,
        request: &ExternalAgentContinueV1,
    ) -> Result<ExternalAgentPeerUpdateV1, ExternalAgentPeerErrorV1>;

    fn resolve_approval(
        &self,
        manifest: &ExternalAgentManifestV1,
        native_session_id: &str,
        resolution: &ExternalApprovalResolutionV1,
    ) -> Result<ExternalAgentPeerUpdateV1, ExternalAgentPeerErrorV1>;

    fn cancel(
        &self,
        manifest: &ExternalAgentManifestV1,
        native_session_id: &str,
        invocation_id: &StableId,
    ) -> Result<ExternalCancellationEvidenceV1, ExternalAgentPeerErrorV1>;

    fn close_session(
        &self,
        manifest: &ExternalAgentManifestV1,
        native_session_id: &str,
    ) -> Result<(), ExternalAgentPeerErrorV1>;
}
