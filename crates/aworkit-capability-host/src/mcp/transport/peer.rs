//! Production MCP peer backed by the official Rust SDK.

use std::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    future::Future,
    sync::{Arc, Mutex, mpsc},
};

use aworkit_protocol::StableId;
use rmcp::{
    ClientHandler, ClientLifecycleMode, ClientServiceExt, Peer, RoleClient, ServiceError,
    model::{
        CallToolRequest, CallToolRequestParams, CancelledNotificationParam, ClientInfo,
        ClientRequest, GetPromptRequest, GetPromptRequestParams, JsonRpcMessage,
        PaginatedRequestParams, ProgressNotificationParam, ProgressToken, ProtocolVersion,
        ReadResourceRequest, ReadResourceRequestParams, RequestId, ServerCapabilities,
        ServerNotification, ServerResult,
    },
    service::{NotificationContext, PeerRequestOptions, RunningService},
    transport::{
        Transport,
        streamable_http_client::{
            StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
        },
    },
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tokio::{process::Command, runtime::Runtime};

use crate::{
    McpCallKindV1, McpCallV1, McpCancellationEvidenceV1, McpCatalogV1, McpDispatchMilestoneV1,
    McpFeatureSetV1, McpInitializeRequestV1, McpInitializeResponseV1, McpPeerCallResultV1,
    McpPeerErrorV1, McpPeerPort, McpProgressV1, McpServerManifestV1, McpToolDescriptorV1,
    McpTransportKindV1, SecretMaterializationV1,
};

use super::{
    MCP_PROTOCOL_2024_11_05, MCP_PROTOCOL_2025_03_26, MCP_PROTOCOL_2025_06_18,
    MCP_PROTOCOL_2025_11_25, MCP_PROTOCOL_2026_07_28, McpPeerTransportConfigV1,
    McpStdioTransportConfigV1, McpTransportConfigurationError, McpTransportEndpointV1,
    ProductionMcpPeerLimitsV1, fold_environment_name, validate_configs, validate_limits,
};
use super::{
    http::SecretHttpClient, secrets::MaterializedTransportSecrets, stdio::BoundedStdioTransport,
};

type SdkClientService = RunningService<RoleClient, AworkitClientHandler>;
const MAXIMUM_CATALOG_PAGES_PER_SECTION: usize = 1024;

/// Production implementation of the existing synchronous `McpPeerPort`.
///
/// A dedicated Tokio runtime owns transport tasks. Synchronous callers wait on
/// channels, so invoking this port from a different async runtime never nests
/// `block_on` and cannot panic for that reason.
pub struct ProductionMcpPeer {
    sessions: Mutex<BTreeMap<String, Arc<LiveMcpSession>>>,
    staged_secrets: Mutex<BTreeMap<String, Arc<MaterializedTransportSecrets>>>,
    initializing: Mutex<BTreeSet<String>>,
    configs: BTreeMap<String, McpPeerTransportConfigV1>,
    limits: ProductionMcpPeerLimitsV1,
    runtime: Runtime,
}

impl ProductionMcpPeer {
    pub fn new(
        configs: Vec<McpPeerTransportConfigV1>,
    ) -> Result<Self, McpTransportConfigurationError> {
        Self::with_limits(configs, ProductionMcpPeerLimitsV1::default())
    }

    pub fn with_limits(
        configs: Vec<McpPeerTransportConfigV1>,
        limits: ProductionMcpPeerLimitsV1,
    ) -> Result<Self, McpTransportConfigurationError> {
        let configs = validate_configs(configs)?;
        validate_limits(&limits)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("aworkit-mcp")
            .build()
            .map_err(|_| McpTransportConfigurationError::RuntimeUnavailable)?;
        Ok(Self {
            sessions: Mutex::new(BTreeMap::new()),
            staged_secrets: Mutex::new(BTreeMap::new()),
            initializing: Mutex::new(BTreeSet::new()),
            configs,
            limits,
            runtime,
        })
    }

    /// Stages one already-materialized lease delivery for the next connection.
    /// Values are copied only into non-formattable, zeroizing transport memory.
    pub fn stage_materialized_secrets(
        &self,
        server_id: &StableId,
        materialization: SecretMaterializationV1,
    ) -> Result<(), McpTransportConfigurationError> {
        let key = server_id.as_str();
        if !self.configs.contains_key(key) {
            return Err(McpTransportConfigurationError::UnknownServer);
        }
        let secrets = Arc::new(MaterializedTransportSecrets::from_materialization(
            materialization,
        )?);
        let initializing = self
            .initializing
            .lock()
            .map_err(|_| McpTransportConfigurationError::Poisoned)?;
        if initializing.contains(key) {
            return Err(McpTransportConfigurationError::ServerActive);
        }
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| McpTransportConfigurationError::Poisoned)?;
        if sessions.contains_key(key) {
            return Err(McpTransportConfigurationError::ServerActive);
        }
        let mut staged = self
            .staged_secrets
            .lock()
            .map_err(|_| McpTransportConfigurationError::Poisoned)?;
        if staged.contains_key(key) {
            return Err(McpTransportConfigurationError::SecretsAlreadyStaged);
        }
        staged.insert(key.to_owned(), secrets);
        Ok(())
    }

    pub fn clear_staged_secrets(
        &self,
        server_id: &StableId,
    ) -> Result<(), McpTransportConfigurationError> {
        self.staged_secrets
            .lock()
            .map_err(|_| McpTransportConfigurationError::Poisoned)?
            .remove(server_id.as_str());
        Ok(())
    }

    fn run<T, F>(&self, future: F) -> Result<T, McpPeerErrorV1>
    where
        T: Send + 'static,
        F: Future<Output = T> + Send + 'static,
    {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.runtime.spawn(async move {
            let output = future.await;
            let _ = sender.send(output);
        });
        receiver.recv().map_err(|_| {
            peer_error(
                "runtime_unavailable",
                "MCP runtime task ended without a result",
                McpDispatchMilestoneV1::Unknown,
                true,
            )
        })
    }

    fn release_initialization(&self, key: &str) {
        if let Ok(mut initializing) = self.initializing.lock() {
            initializing.remove(key);
        }
    }
}

impl McpPeerPort for ProductionMcpPeer {
    fn initialize(
        &self,
        manifest: &McpServerManifestV1,
        request: &McpInitializeRequestV1,
    ) -> Result<McpInitializeResponseV1, McpPeerErrorV1> {
        let key = manifest.server_id.as_str().to_owned();
        let config = self.configs.get(&key).cloned().ok_or_else(|| {
            not_started(
                "transport_not_configured",
                "MCP transport is not configured",
            )
        })?;
        validate_binding(manifest, request, &config)?;
        {
            let mut initializing = self.initializing.lock().map_err(|_| lock_error())?;
            if !initializing.insert(key.clone()) {
                return Err(not_started(
                    "initialization_in_progress",
                    "MCP transport initialization is already in progress",
                ));
            }
        }

        let old_session = match self.sessions.lock() {
            Ok(mut sessions) => sessions.remove(&key),
            Err(_) => {
                self.release_initialization(&key);
                return Err(lock_error());
            }
        };
        if old_session
            .as_ref()
            .is_some_and(|session| session.has_active_calls())
        {
            if let Some(old_session) = old_session {
                if let Ok(mut sessions) = self.sessions.lock() {
                    sessions.insert(key.clone(), old_session);
                }
            }
            self.release_initialization(&key);
            return Err(not_started(
                "calls_active",
                "MCP transport cannot reconnect while calls are active",
            ));
        }

        let secrets = if let Some(session) = &old_session {
            session.secrets.clone()
        } else {
            match self.staged_secrets.lock() {
                Ok(mut staged) => staged
                    .remove(&key)
                    .unwrap_or_else(|| Arc::new(MaterializedTransportSecrets::empty())),
                Err(_) => {
                    self.release_initialization(&key);
                    return Err(lock_error());
                }
            }
        };
        let http_config = match &config.endpoint {
            McpTransportEndpointV1::StreamableHttp(http) => Some(http),
            McpTransportEndpointV1::Stdio(_) => None,
        };
        if secrets.validate(manifest, http_config).is_err() {
            if let Some(old_session) = old_session {
                if let Ok(mut sessions) = self.sessions.lock() {
                    sessions.insert(key.clone(), old_session);
                }
            } else if let Ok(mut staged) = self.staged_secrets.lock() {
                staged.insert(key.clone(), secrets);
            }
            self.release_initialization(&key);
            return Err(not_started(
                "secret_material_mismatch",
                "MCP materialized secret fields do not match the attested binding",
            ));
        }

        if let Some(session) = old_session {
            let close_timeout = self.limits.close_timeout;
            let close_result =
                self.run(async move { close_live_session(session, close_timeout).await });
            match close_result {
                Ok(Ok(())) => {}
                Ok(Err(error)) | Err(error) => {
                    if let Ok(mut staged) = self.staged_secrets.lock() {
                        staged.insert(key.clone(), secrets);
                    }
                    self.release_initialization(&key);
                    return Err(error);
                }
            }
        }

        let manifest_owned = manifest.clone();
        let request_owned = request.clone();
        let limits = self.limits.clone();
        let connection_secrets = secrets.clone();
        let established = self.run(async move {
            establish_session(
                config,
                manifest_owned,
                request_owned,
                connection_secrets,
                limits,
            )
            .await
        });
        let artifact = match established {
            Ok(Ok(artifact)) => artifact,
            Ok(Err(error)) | Err(error) => {
                if let Ok(mut staged) = self.staged_secrets.lock() {
                    staged.insert(key.clone(), secrets);
                }
                self.release_initialization(&key);
                return Err(error);
            }
        };
        let response = artifact.response.clone();
        let insertion = self
            .sessions
            .lock()
            .map_err(|_| lock_error())
            .and_then(|mut sessions| match sessions.entry(key.clone()) {
                Entry::Vacant(entry) => {
                    entry.insert(artifact.session);
                    Ok(())
                }
                Entry::Occupied(_) => Err(not_started(
                    "binding_race",
                    "MCP transport binding changed during initialization",
                )),
            });
        self.release_initialization(&key);
        insertion?;
        Ok(response)
    }

    fn invoke(
        &self,
        manifest: &McpServerManifestV1,
        call: &McpCallV1,
    ) -> Result<McpPeerCallResultV1, McpPeerErrorV1> {
        let session = self
            .sessions
            .lock()
            .map_err(|_| lock_error())?
            .get(manifest.server_id.as_str())
            .cloned()
            .ok_or_else(|| not_started("unknown_session", "MCP transport session is not open"))?;
        session.validate_manifest(manifest)?;
        let request = build_call_request(call, self.limits.maximum_result_bytes)?;
        let invocation_id = call.invocation_id.as_str().to_owned();
        {
            let mut active = session.active.lock().map_err(|_| lock_error())?;
            if active
                .insert(invocation_id.clone(), ActiveRequest::default())
                .is_some()
            {
                return Err(not_started(
                    "duplicate_invocation",
                    "MCP transport invocation is already active",
                ));
            }
        }
        let session_for_call = session.clone();
        let maximum_progress_events = manifest.maximum_progress_events;
        let timeout = self.limits.request_timeout;
        let wire = self.run(async move {
            execute_call(
                session_for_call,
                invocation_id,
                request,
                maximum_progress_events,
                timeout,
            )
            .await
        });
        let wire = match wire {
            Ok(result) => result?,
            Err(error) => {
                session.remove_active(call.invocation_id.as_str());
                return Err(error);
            }
        };
        let mut result = match (call.kind, wire.response) {
            (McpCallKindV1::Tool, ServerResult::CallToolResult(result)) => {
                serde_json::to_value(result)
            }
            (McpCallKindV1::Resource, ServerResult::ReadResourceResult(result)) => {
                serde_json::to_value(result)
            }
            (McpCallKindV1::Prompt, ServerResult::GetPromptResult(result)) => {
                serde_json::to_value(result)
            }
            _ => {
                return Err(started(
                    "unexpected_response",
                    "MCP server returned an unexpected response type",
                    false,
                ));
            }
        }
        .map_err(|_| {
            started(
                "invalid_response",
                "MCP server response could not be normalized",
                false,
            )
        })?;
        enforce_result_bound(&result, self.limits.maximum_result_bytes)?;
        session.secrets.redact_json(&mut result);
        enforce_result_bound(&result, self.limits.maximum_result_bytes)?;
        Ok(McpPeerCallResultV1 {
            result,
            progress: wire.progress,
        })
    }

    fn cancel(
        &self,
        manifest: &McpServerManifestV1,
        invocation_id: &StableId,
    ) -> Result<McpCancellationEvidenceV1, McpPeerErrorV1> {
        let session = self
            .sessions
            .lock()
            .map_err(|_| lock_error())?
            .get(manifest.server_id.as_str())
            .cloned()
            .ok_or_else(|| not_started("unknown_session", "MCP transport session is not open"))?;
        session.validate_manifest(manifest)?;
        let request_id = {
            let mut active = session.active.lock().map_err(|_| lock_error())?;
            let active = active.get_mut(invocation_id.as_str()).ok_or_else(|| {
                not_started(
                    "unknown_invocation",
                    "MCP transport invocation is not active",
                )
            })?;
            active.cancellation_requested = true;
            if active.cancellation_sent {
                return Ok(McpCancellationEvidenceV1::Unknown);
            }
            let Some(request_id) = active.request_id.clone() else {
                return Ok(McpCancellationEvidenceV1::Unknown);
            };
            active.cancellation_sent = true;
            request_id
        };
        let peer = session.peer.clone();
        let timeout = self.limits.request_timeout;
        let cancellation = self.run(async move {
            tokio::time::timeout(
                timeout,
                peer.notify_cancelled(CancelledNotificationParam::new(
                    Some(request_id),
                    Some("Aworkit invocation cancelled".to_owned()),
                )),
            )
            .await
        })?;
        match cancellation {
            Ok(Ok(())) => Ok(McpCancellationEvidenceV1::Unknown),
            Ok(Err(error)) => Err(service_peer_error(
                "cancel_failed",
                "MCP cancellation notification failed",
                &error,
                McpDispatchMilestoneV1::Started,
                &session.secrets,
            )),
            Err(_) => Err(started(
                "cancel_timeout",
                "MCP cancellation notification timed out",
                false,
            )),
        }
    }

    fn close(&self, manifest: &McpServerManifestV1) -> Result<(), McpPeerErrorV1> {
        let session = {
            let mut sessions = self.sessions.lock().map_err(|_| lock_error())?;
            let session = sessions
                .get(manifest.server_id.as_str())
                .cloned()
                .ok_or_else(|| {
                    not_started("unknown_session", "MCP transport session is not open")
                })?;
            session.validate_manifest(manifest)?;
            if session.has_active_calls() {
                return Err(not_started(
                    "calls_active",
                    "MCP transport cannot close while calls are active",
                ));
            }
            sessions
                .remove(manifest.server_id.as_str())
                .expect("validated session remains under the map lock")
        };
        let timeout = self.limits.close_timeout;
        let key = manifest.server_id.as_str().to_owned();
        let secrets = session.secrets.clone();
        match self.run(async move { close_live_session(session, timeout).await }) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) | Err(error) => {
                if let Ok(mut staged) = self.staged_secrets.lock() {
                    staged.entry(key).or_insert(secrets);
                }
                Err(error)
            }
        }
    }
}

impl Drop for ProductionMcpPeer {
    fn drop(&mut self) {
        if let Ok(mut sessions) = self.sessions.lock() {
            for session in sessions.values() {
                if let Ok(service) = session.service.lock()
                    && let Some(service) = service.as_ref()
                {
                    service.cancellation_token().cancel();
                }
            }
            sessions.clear();
        }
    }
}

struct InitializationArtifact {
    session: Arc<LiveMcpSession>,
    response: McpInitializeResponseV1,
}

struct LiveMcpSession {
    server_id: StableId,
    binding_hash: String,
    transport: McpTransportKindV1,
    peer: Peer<RoleClient>,
    service: Mutex<Option<SdkClientService>>,
    active: Mutex<BTreeMap<String, ActiveRequest>>,
    progress: Arc<ProgressRegistry>,
    secrets: Arc<MaterializedTransportSecrets>,
}

impl LiveMcpSession {
    fn validate_manifest(&self, manifest: &McpServerManifestV1) -> Result<(), McpPeerErrorV1> {
        if self.server_id != manifest.server_id
            || self.binding_hash != manifest.binding_hash
            || self.transport != manifest.transport
        {
            return Err(not_started(
                "binding_drift",
                "MCP transport manifest no longer matches the live binding",
            ));
        }
        Ok(())
    }

    fn has_active_calls(&self) -> bool {
        self.active.lock().map_or(true, |active| !active.is_empty())
    }

    fn remove_active(&self, invocation_id: &str) {
        if let Ok(mut active) = self.active.lock() {
            active.remove(invocation_id);
        }
    }
}

#[derive(Default)]
struct ActiveRequest {
    request_id: Option<RequestId>,
    cancellation_requested: bool,
    cancellation_sent: bool,
}

#[derive(Clone)]
struct AworkitClientHandler {
    info: ClientInfo,
}

impl ClientHandler for AworkitClientHandler {
    fn get_info(&self) -> ClientInfo {
        self.info.clone()
    }

    fn on_progress(
        &self,
        _params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + Send + '_ {
        // Progress is recorded synchronously at the transport boundary below.
        // RMCP intentionally dispatches notification handlers as independent
        // tasks, which does not preserve their wire order relative to a final
        // response.
        std::future::ready(())
    }
}

struct ProgressObservingTransport<T> {
    inner: T,
    progress: Arc<ProgressRegistry>,
}

impl<T> ProgressObservingTransport<T> {
    fn new(inner: T, progress: Arc<ProgressRegistry>) -> Self {
        Self { inner, progress }
    }
}

impl<T> Transport<RoleClient> for ProgressObservingTransport<T>
where
    T: Transport<RoleClient>,
{
    type Error = T::Error;

    fn send(
        &mut self,
        item: rmcp::service::TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.inner.send(item)
    }

    async fn receive(&mut self) -> Option<rmcp::service::RxJsonRpcMessage<RoleClient>> {
        let message = self.inner.receive().await?;
        if let JsonRpcMessage::Notification(notification) = &message
            && let ServerNotification::ProgressNotification(progress) = &notification.notification
        {
            self.progress.record(progress.params.clone());
        }
        Some(message)
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.inner.close().await
    }
}

#[derive(Default)]
struct ProgressRegistry {
    slots: Mutex<BTreeMap<String, ProgressSlot>>,
}

struct ProgressSlot {
    events: Vec<McpProgressV1>,
    maximum_events: usize,
    last_progress: Option<f64>,
    violated: bool,
    secrets: Arc<MaterializedTransportSecrets>,
}

impl ProgressRegistry {
    fn register(
        &self,
        token: &ProgressToken,
        maximum_events: usize,
        secrets: Arc<MaterializedTransportSecrets>,
    ) -> Result<(), McpPeerErrorV1> {
        let key = progress_key(token)?;
        let mut slots = self.slots.lock().map_err(|_| lock_error())?;
        if slots
            .insert(
                key,
                ProgressSlot {
                    events: Vec::new(),
                    maximum_events,
                    last_progress: None,
                    violated: false,
                    secrets,
                },
            )
            .is_some()
        {
            return Err(started(
                "progress_token_collision",
                "MCP progress token was reused while active",
                false,
            ));
        }
        Ok(())
    }

    fn record(&self, params: ProgressNotificationParam) {
        let Ok(key) = progress_key(&params.progress_token) else {
            return;
        };
        let Ok(mut slots) = self.slots.lock() else {
            return;
        };
        let Some(slot) = slots.get_mut(&key) else {
            return;
        };
        let ordered = params.progress.is_finite()
            && params
                .total
                .is_none_or(|total| total.is_finite() && total >= 0.0)
            && slot
                .last_progress
                .is_none_or(|previous| params.progress > previous);
        if !ordered || slot.events.len() >= slot.maximum_events {
            slot.violated = true;
            return;
        }
        let raw_message = params.message.unwrap_or_else(|| match params.total {
            Some(total) => format!("{} / {}", params.progress, total),
            None => params.progress.to_string(),
        });
        let message = slot.secrets.redact_text(&raw_message);
        if message.len() > 16 * 1024 {
            slot.violated = true;
            return;
        }
        slot.last_progress = Some(params.progress);
        slot.events.push(McpProgressV1 {
            sequence: slot.events.len() as u64 + 1,
            message,
        });
    }

    fn take(&self, token: &ProgressToken) -> Result<Vec<McpProgressV1>, McpPeerErrorV1> {
        let key = progress_key(token)?;
        let slot = self
            .slots
            .lock()
            .map_err(|_| lock_error())?
            .remove(&key)
            .ok_or_else(|| {
                started(
                    "progress_state_missing",
                    "MCP progress state was lost before settlement",
                    false,
                )
            })?;
        if slot.violated {
            return Err(started(
                "progress_violation",
                "MCP server progress exceeded ordering or size bounds",
                false,
            ));
        }
        Ok(slot.events)
    }
}

struct WireCallResult {
    response: ServerResult,
    progress: Vec<McpProgressV1>,
}

async fn establish_session(
    config: McpPeerTransportConfigV1,
    manifest: McpServerManifestV1,
    request: McpInitializeRequestV1,
    secrets: Arc<MaterializedTransportSecrets>,
    limits: ProductionMcpPeerLimitsV1,
) -> Result<InitializationArtifact, McpPeerErrorV1> {
    let (lifecycle, mut client_info) = lifecycle_for(&request)?;
    client_info.client_info.name = "aworkit-capability-host".to_owned();
    client_info.client_info.version = env!("CARGO_PKG_VERSION").to_owned();
    let progress = Arc::new(ProgressRegistry::default());
    let handler = AworkitClientHandler { info: client_info };
    let initialization = async {
        let service = match &config.endpoint {
            McpTransportEndpointV1::Stdio(stdio) => {
                let transport = build_stdio_transport(stdio, &secrets, &limits)?;
                let transport = ProgressObservingTransport::new(transport, progress.clone());
                let retry_legacy = matches!(
                    &lifecycle,
                    ClientLifecycleMode::Auto {
                        legacy_version: Some(_),
                        ..
                    }
                );
                match handler
                    .clone()
                    .serve_with_lifecycle(transport, lifecycle.clone())
                    .await
                {
                    Ok(service) => service,
                    Err(error)
                        if retry_legacy
                            && error
                                .to_string()
                                .contains("connection closed: discover response") =>
                    {
                        // Some pre-2026 STDIO servers terminate when their first
                        // request is the new server/discover handshake. Start a
                        // fresh process and use the newest attested legacy
                        // initialize version instead.
                        let transport = build_stdio_transport(stdio, &secrets, &limits)?;
                        let transport =
                            ProgressObservingTransport::new(transport, progress.clone());
                        handler
                            .clone()
                            .serve_with_lifecycle(transport, ClientLifecycleMode::Initialize)
                            .await
                            .map_err(|error| {
                                not_started(
                                    "initialization_failed",
                                    &secrets.redact_text(&error.to_string()),
                                )
                            })?
                    }
                    Err(error) => {
                        return Err(not_started(
                            "initialization_failed",
                            &secrets.redact_text(&error.to_string()),
                        ));
                    }
                }
            }
            McpTransportEndpointV1::StreamableHttp(http) => {
                let client = SecretHttpClient::new(
                    secrets.clone(),
                    http.bearer_token_secret_slot.clone(),
                    maximum_wire_bytes(&limits),
                )
                .map_err(|_| {
                    not_started(
                        "http_client_unavailable",
                        "MCP HTTP client could not be created",
                    )
                })?;
                let mut sdk_config =
                    StreamableHttpClientTransportConfig::with_uri(http.endpoint.clone());
                sdk_config.allow_stateless = http.allow_stateless;
                sdk_config.max_sse_event_size = http.maximum_sse_event_bytes;
                sdk_config.reinit_on_expired_session = false;
                let transport = StreamableHttpClientTransport::with_client(client, sdk_config);
                let transport = ProgressObservingTransport::new(transport, progress.clone());
                handler
                    .clone()
                    .serve_with_lifecycle(transport, lifecycle)
                    .await
                    .map_err(|error| {
                        not_started(
                            "initialization_failed",
                            &secrets.redact_text(&error.to_string()),
                        )
                    })?
            }
        };
        let peer_info = service.peer_info().ok_or_else(|| {
            not_started(
                "missing_peer_info",
                "MCP server did not provide negotiated peer information",
            )
        })?;
        let protocol_version = numeric_protocol(&peer_info.protocol_version).ok_or_else(|| {
            not_started(
                "unsupported_protocol",
                "MCP server selected an unsupported protocol version",
            )
        })?;
        if protocol_version < request.minimum_protocol_version
            || protocol_version > request.maximum_protocol_version
        {
            return Err(not_started(
                "protocol_out_of_range",
                "MCP server selected a protocol outside the attested range",
            ));
        }
        let features = feature_set(&peer_info.capabilities);
        let catalog = discover_catalog(service.peer(), &features, &limits).await?;
        let response = McpInitializeResponseV1 {
            server_id: manifest.server_id.clone(),
            protocol_version,
            features,
            catalog,
        };
        let peer = service.peer().clone();
        let session = Arc::new(LiveMcpSession {
            server_id: manifest.server_id,
            binding_hash: manifest.binding_hash,
            transport: manifest.transport,
            peer,
            service: Mutex::new(Some(service)),
            active: Mutex::new(BTreeMap::new()),
            progress,
            secrets,
        });
        Ok(InitializationArtifact { session, response })
    };
    tokio::time::timeout(limits.initialization_timeout, initialization)
        .await
        .map_err(|_| {
            not_started(
                "initialization_timeout",
                "MCP initialization or discovery timed out",
            )
        })?
}

fn build_stdio_transport(
    config: &McpStdioTransportConfigV1,
    secrets: &MaterializedTransportSecrets,
    limits: &ProductionMcpPeerLimitsV1,
) -> Result<BoundedStdioTransport, McpPeerErrorV1> {
    let mut environment_names = inherited_runtime_environment()
        .into_iter()
        .map(|(name, _)| fold_environment_name(name))
        .collect::<BTreeSet<_>>();
    let mut command = Command::new(&config.executable);
    command.args(&config.arguments).env_clear();
    for (name, value) in inherited_runtime_environment() {
        command.env(name, value);
    }
    for (name, value) in &config.public_environment {
        environment_names.insert(fold_environment_name(name));
        command.env(name, value);
    }
    for (name, value) in secrets.environment() {
        if !environment_names.insert(fold_environment_name(name)) {
            return Err(not_started(
                "environment_collision",
                "MCP public and secret environment targets overlap",
            ));
        }
        let value = std::str::from_utf8(value).map_err(|_| {
            not_started(
                "invalid_environment_secret",
                "MCP environment secret is not valid UTF-8",
            )
        })?;
        command.env(name, value);
    }
    if let Some(directory) = &config.working_directory {
        command.current_dir(directory);
    }
    BoundedStdioTransport::spawn(&mut command, maximum_wire_bytes(limits)).map_err(|error| {
        not_started(
            "spawn_failed",
            &format!("MCP STDIO server could not be started: {error}"),
        )
    })
}

/// Preserve the OS runtime variables required by common local MCP launchers
/// such as `npx.cmd`, while keeping arbitrary parent secrets out of the child.
fn inherited_runtime_environment() -> Vec<(&'static str, std::ffi::OsString)> {
    let names: &[&str] = if cfg!(windows) {
        &[
            "SystemRoot",
            "ComSpec",
            "Path",
            "PATHEXT",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "TEMP",
            "TMP",
        ]
    } else {
        &["PATH", "HOME", "TMPDIR", "TMP", "TEMP"]
    };
    names
        .iter()
        .filter_map(|name| std::env::var_os(name).map(|value| (*name, value)))
        .collect()
}

fn maximum_wire_bytes(limits: &ProductionMcpPeerLimitsV1) -> usize {
    limits
        .maximum_result_bytes
        .max(limits.maximum_catalog_bytes)
        .max(limits.maximum_schema_bytes)
        .saturating_add(1024 * 1024)
}

async fn discover_catalog(
    peer: &Peer<RoleClient>,
    features: &McpFeatureSetV1,
    limits: &ProductionMcpPeerLimitsV1,
) -> Result<McpCatalogV1, McpPeerErrorV1> {
    let mut bytes = 0usize;
    let mut tools = Vec::new();
    if features.tools {
        let mut cursor = None;
        let mut cursors = BTreeSet::new();
        let mut pages = 0;
        loop {
            enforce_page_count(&mut pages)?;
            let page = peer
                .list_tools(Some(PaginatedRequestParams::default().with_cursor(cursor)))
                .await
                .map_err(|error| discovery_error("tools", &error))?;
            for tool in page.tools {
                bytes = add_catalog_bytes(bytes, &tool, limits.maximum_catalog_bytes)?;
                let schema = serde_json::to_vec(tool.input_schema.as_ref()).map_err(|_| {
                    not_started(
                        "invalid_tool_schema",
                        "MCP tool schema could not be encoded",
                    )
                })?;
                if schema.len() > limits.maximum_schema_bytes {
                    return Err(not_started(
                        "tool_schema_too_large",
                        "MCP tool schema exceeded the configured bound",
                    ));
                }
                tools.push(McpToolDescriptorV1 {
                    name: tool.name.into_owned(),
                    input_schema_hash: format!("sha256:{:x}", Sha256::digest(&schema)),
                    // Server annotations are hints, not proof of side-effect freedom.
                    side_effect_known_read_only: false,
                    description: tool
                        .description
                        .map_or_else(String::new, |value| value.into_owned()),
                    input_schema: Value::Object(tool.input_schema.as_ref().clone()),
                });
                enforce_catalog_count(tools.len(), 0, 0, limits)?;
            }
            cursor = checked_next_cursor(page.next_cursor, &mut cursors)?;
            if cursor.is_none() {
                break;
            }
        }
    }

    let mut resources = Vec::new();
    if features.resources {
        let mut cursor = None;
        let mut cursors = BTreeSet::new();
        let mut pages = 0;
        loop {
            enforce_page_count(&mut pages)?;
            let page = peer
                .list_resources(Some(PaginatedRequestParams::default().with_cursor(cursor)))
                .await
                .map_err(|error| discovery_error("resources", &error))?;
            for resource in page.resources {
                bytes = add_catalog_bytes(bytes, &resource, limits.maximum_catalog_bytes)?;
                resources.push(resource.uri);
                enforce_catalog_count(tools.len(), resources.len(), 0, limits)?;
            }
            cursor = checked_next_cursor(page.next_cursor, &mut cursors)?;
            if cursor.is_none() {
                break;
            }
        }
    }

    let mut prompts = Vec::new();
    if features.prompts {
        let mut cursor = None;
        let mut cursors = BTreeSet::new();
        let mut pages = 0;
        loop {
            enforce_page_count(&mut pages)?;
            let page = peer
                .list_prompts(Some(PaginatedRequestParams::default().with_cursor(cursor)))
                .await
                .map_err(|error| discovery_error("prompts", &error))?;
            for prompt in page.prompts {
                bytes = add_catalog_bytes(bytes, &prompt, limits.maximum_catalog_bytes)?;
                prompts.push(prompt.name);
                enforce_catalog_count(tools.len(), resources.len(), prompts.len(), limits)?;
            }
            cursor = checked_next_cursor(page.next_cursor, &mut cursors)?;
            if cursor.is_none() {
                break;
            }
        }
    }
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    resources.sort();
    prompts.sort();
    if tools.windows(2).any(|pair| pair[0].name == pair[1].name)
        || resources.windows(2).any(|pair| pair[0] == pair[1])
        || prompts.windows(2).any(|pair| pair[0] == pair[1])
    {
        return Err(not_started(
            "duplicate_catalog_entry",
            "MCP discovery returned duplicate catalog entries",
        ));
    }
    Ok(McpCatalogV1 {
        tools,
        resources,
        prompts,
    })
}

async fn execute_call(
    session: Arc<LiveMcpSession>,
    invocation_id: String,
    request: ClientRequest,
    maximum_progress_events: usize,
    timeout: std::time::Duration,
) -> Result<WireCallResult, McpPeerErrorV1> {
    let options = PeerRequestOptions::with_timeout(timeout).with_max_total_timeout(timeout);
    let handle = match session
        .peer
        .send_cancellable_request(request, options)
        .await
    {
        Ok(handle) => handle,
        Err(error) => {
            session.remove_active(&invocation_id);
            return Err(service_peer_error(
                "dispatch_failed",
                "MCP request could not be dispatched",
                &error,
                McpDispatchMilestoneV1::DefinitelyNotStarted,
                &session.secrets,
            ));
        }
    };
    let request_id = handle.id.clone();
    let progress_token = handle.progress_token.clone();
    session.progress.register(
        &progress_token,
        maximum_progress_events,
        session.secrets.clone(),
    )?;
    let send_deferred_cancellation = {
        let mut active = session.active.lock().map_err(|_| lock_error())?;
        let state = active.get_mut(&invocation_id).ok_or_else(|| {
            started(
                "active_state_missing",
                "MCP invocation state was lost before dispatch",
                false,
            )
        })?;
        state.request_id = Some(request_id.clone());
        if state.cancellation_requested && !state.cancellation_sent {
            state.cancellation_sent = true;
            true
        } else {
            false
        }
    };
    if send_deferred_cancellation {
        let _ = session
            .peer
            .notify_cancelled(CancelledNotificationParam::new(
                Some(request_id),
                Some("Aworkit invocation cancelled".to_owned()),
            ))
            .await;
    }
    let response = handle.await_response().await;
    let progress = session.progress.take(&progress_token);
    session.remove_active(&invocation_id);
    let response = response.map_err(|error| {
        service_peer_error(
            "request_failed",
            "MCP request did not produce a terminal response",
            &error,
            McpDispatchMilestoneV1::Started,
            &session.secrets,
        )
    })?;
    Ok(WireCallResult {
        response,
        progress: progress?,
    })
}

async fn close_live_session(
    session: Arc<LiveMcpSession>,
    timeout: std::time::Duration,
) -> Result<(), McpPeerErrorV1> {
    let mut service = session
        .service
        .lock()
        .map_err(|_| lock_error())?
        .take()
        .ok_or_else(|| not_started("session_closed", "MCP transport is already closed"))?;
    match service.close_with_timeout(timeout).await {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(started(
            "close_timeout",
            "MCP transport did not close within the configured bound",
            true,
        )),
        Err(_) => Err(started(
            "close_failed",
            "MCP transport cleanup task failed",
            true,
        )),
    }
}

fn build_call_request(
    call: &McpCallV1,
    maximum_arguments_bytes: usize,
) -> Result<ClientRequest, McpPeerErrorV1> {
    let serialized = serde_json::to_vec(&call.arguments).map_err(|_| {
        not_started(
            "invalid_arguments",
            "MCP invocation arguments could not be encoded",
        )
    })?;
    if serialized.len() > maximum_arguments_bytes {
        return Err(not_started(
            "arguments_too_large",
            "MCP invocation arguments exceeded the configured bound",
        ));
    }
    match call.kind {
        McpCallKindV1::Tool => {
            let arguments = optional_arguments(&call.arguments)?;
            let mut params = CallToolRequestParams::new(call.name.clone());
            if let Some(arguments) = arguments {
                params = params.with_arguments(arguments);
            }
            Ok(CallToolRequest::new(params).into())
        }
        McpCallKindV1::Resource => {
            if !matches!(&call.arguments, Value::Null)
                && !matches!(&call.arguments, Value::Object(arguments) if arguments.is_empty())
            {
                return Err(not_started(
                    "invalid_arguments",
                    "MCP resource reads do not accept invocation arguments",
                ));
            }
            Ok(ReadResourceRequest::new(ReadResourceRequestParams::new(call.name.clone())).into())
        }
        McpCallKindV1::Prompt => {
            let arguments = optional_arguments(&call.arguments)?;
            let mut params = GetPromptRequestParams::new(call.name.clone());
            if let Some(arguments) = arguments {
                params = params.with_arguments(arguments);
            }
            Ok(GetPromptRequest::new(params).into())
        }
    }
}

fn optional_arguments(value: &Value) -> Result<Option<Map<String, Value>>, McpPeerErrorV1> {
    match value {
        Value::Null => Ok(None),
        Value::Object(arguments) if arguments.is_empty() => Ok(None),
        Value::Object(arguments) => Ok(Some(arguments.clone())),
        _ => Err(not_started(
            "invalid_arguments",
            "MCP invocation arguments must be a JSON object",
        )),
    }
}

fn lifecycle_for(
    request: &McpInitializeRequestV1,
) -> Result<(ClientLifecycleMode, ClientInfo), McpPeerErrorV1> {
    let mut supported = [
        MCP_PROTOCOL_2024_11_05,
        MCP_PROTOCOL_2025_03_26,
        MCP_PROTOCOL_2025_06_18,
        MCP_PROTOCOL_2025_11_25,
        MCP_PROTOCOL_2026_07_28,
    ]
    .into_iter()
    .filter(|version| {
        *version >= request.minimum_protocol_version && *version <= request.maximum_protocol_version
    })
    .collect::<Vec<_>>();
    supported.sort_unstable_by(|left, right| right.cmp(left));
    let Some(highest) = supported.first().copied() else {
        return Err(not_started(
            "unsupported_protocol_range",
            "MCP attested protocol range has no supported wire version",
        ));
    };
    let mut info = ClientInfo::default();
    if supported.contains(&MCP_PROTOCOL_2026_07_28) {
        let legacy = supported
            .iter()
            .copied()
            .find(|version| *version < MCP_PROTOCOL_2026_07_28)
            .and_then(wire_protocol);
        info.protocol_version = legacy.clone().unwrap_or(ProtocolVersion::V_2026_07_28);
        let preferred_versions = supported.into_iter().filter_map(wire_protocol).collect();
        Ok((
            ClientLifecycleMode::Auto {
                preferred_versions,
                legacy_version: legacy,
            },
            info,
        ))
    } else {
        info.protocol_version = wire_protocol(highest).ok_or_else(|| {
            not_started(
                "unsupported_protocol_range",
                "MCP legacy protocol version is unsupported",
            )
        })?;
        Ok((ClientLifecycleMode::Initialize, info))
    }
}

fn wire_protocol(version: u16) -> Option<ProtocolVersion> {
    match version {
        MCP_PROTOCOL_2024_11_05 => Some(ProtocolVersion::V_2024_11_05),
        MCP_PROTOCOL_2025_03_26 => Some(ProtocolVersion::V_2025_03_26),
        MCP_PROTOCOL_2025_06_18 => Some(ProtocolVersion::V_2025_06_18),
        MCP_PROTOCOL_2025_11_25 => Some(ProtocolVersion::V_2025_11_25),
        MCP_PROTOCOL_2026_07_28 => Some(ProtocolVersion::V_2026_07_28),
        _ => None,
    }
}

fn numeric_protocol(version: &ProtocolVersion) -> Option<u16> {
    match version.as_str() {
        "2024-11-05" => Some(MCP_PROTOCOL_2024_11_05),
        "2025-03-26" => Some(MCP_PROTOCOL_2025_03_26),
        "2025-06-18" => Some(MCP_PROTOCOL_2025_06_18),
        "2025-11-25" => Some(MCP_PROTOCOL_2025_11_25),
        "2026-07-28" => Some(MCP_PROTOCOL_2026_07_28),
        _ => None,
    }
}

fn feature_set(capabilities: &ServerCapabilities) -> McpFeatureSetV1 {
    McpFeatureSetV1 {
        tools: capabilities.tools.is_some(),
        resources: capabilities.resources.is_some(),
        prompts: capabilities.prompts.is_some(),
        progress: true,
        cancellation: true,
    }
}

fn validate_binding(
    manifest: &McpServerManifestV1,
    request: &McpInitializeRequestV1,
    config: &McpPeerTransportConfigV1,
) -> Result<(), McpPeerErrorV1> {
    if request.server_id != manifest.server_id
        || request.host_generation != manifest.host_generation
        || config.server_id != manifest.server_id
        || config.binding_hash != manifest.binding_hash
        || config.transport_kind() != manifest.transport
    {
        return Err(not_started(
            "binding_mismatch",
            "MCP transport configuration does not match the attested manifest",
        ));
    }
    if request.minimum_protocol_version != manifest.minimum_protocol_version
        || request.maximum_protocol_version != manifest.maximum_protocol_version
    {
        return Err(not_started(
            "protocol_binding_mismatch",
            "MCP protocol request does not match the attested manifest",
        ));
    }
    Ok(())
}

fn add_catalog_bytes<T: serde::Serialize>(
    current: usize,
    value: &T,
    maximum: usize,
) -> Result<usize, McpPeerErrorV1> {
    let bytes = serde_json::to_vec(value).map_err(|_| {
        not_started(
            "invalid_catalog",
            "MCP discovery entry could not be encoded",
        )
    })?;
    let total = current.saturating_add(bytes.len());
    if total > maximum {
        return Err(not_started(
            "catalog_too_large",
            "MCP discovery catalog exceeded the configured byte bound",
        ));
    }
    Ok(total)
}

fn enforce_result_bound(value: &Value, maximum: usize) -> Result<(), McpPeerErrorV1> {
    let bytes = serde_json::to_vec(value).map_err(|_| {
        started(
            "invalid_response",
            "MCP server response could not be measured",
            false,
        )
    })?;
    if bytes.len() > maximum {
        return Err(started(
            "response_too_large",
            "MCP server response exceeded the configured bound",
            false,
        ));
    }
    Ok(())
}

fn enforce_catalog_count(
    tools: usize,
    resources: usize,
    prompts: usize,
    limits: &ProductionMcpPeerLimitsV1,
) -> Result<(), McpPeerErrorV1> {
    if tools.saturating_add(resources).saturating_add(prompts) > limits.maximum_catalog_entries {
        return Err(not_started(
            "catalog_too_large",
            "MCP discovery catalog exceeded the configured entry bound",
        ));
    }
    Ok(())
}

fn checked_next_cursor(
    cursor: Option<String>,
    seen: &mut BTreeSet<String>,
) -> Result<Option<String>, McpPeerErrorV1> {
    if let Some(cursor) = &cursor
        && (!seen.insert(cursor.clone()) || cursor.len() > 16 * 1024)
    {
        return Err(not_started(
            "invalid_pagination",
            "MCP discovery pagination repeated or exceeded its bound",
        ));
    }
    Ok(cursor)
}

fn enforce_page_count(pages: &mut usize) -> Result<(), McpPeerErrorV1> {
    *pages = pages.saturating_add(1);
    if *pages > MAXIMUM_CATALOG_PAGES_PER_SECTION {
        return Err(not_started(
            "invalid_pagination",
            "MCP discovery pagination exceeded its page bound",
        ));
    }
    Ok(())
}

fn progress_key(token: &ProgressToken) -> Result<String, McpPeerErrorV1> {
    serde_json::to_string(token).map_err(|_| {
        started(
            "invalid_progress_token",
            "MCP progress token could not be normalized",
            false,
        )
    })
}

fn discovery_error(section: &str, error: &ServiceError) -> McpPeerErrorV1 {
    peer_error(
        "discovery_failed",
        &format!("MCP {section} discovery failed"),
        McpDispatchMilestoneV1::DefinitelyNotStarted,
        service_transport_lost(error),
    )
}

fn service_peer_error(
    code: &str,
    prefix: &str,
    error: &ServiceError,
    dispatch: McpDispatchMilestoneV1,
    secrets: &MaterializedTransportSecrets,
) -> McpPeerErrorV1 {
    let message = secrets.redact_text(&format!("{prefix}: {error}"));
    peer_error(code, &message, dispatch, service_transport_lost(error))
}

fn service_transport_lost(error: &ServiceError) -> bool {
    matches!(
        error,
        ServiceError::TransportClosed | ServiceError::TransportSend(_)
    )
}

fn lock_error() -> McpPeerErrorV1 {
    peer_error(
        "state_unavailable",
        "MCP transport state lock is unavailable",
        McpDispatchMilestoneV1::Unknown,
        false,
    )
}

fn not_started(code: &str, message: &str) -> McpPeerErrorV1 {
    peer_error(
        code,
        message,
        McpDispatchMilestoneV1::DefinitelyNotStarted,
        false,
    )
}

fn started(code: &str, message: &str, transport_lost: bool) -> McpPeerErrorV1 {
    peer_error(
        code,
        message,
        McpDispatchMilestoneV1::Started,
        transport_lost,
    )
}

fn peer_error(
    code: &str,
    message: &str,
    dispatch: McpDispatchMilestoneV1,
    transport_lost: bool,
) -> McpPeerErrorV1 {
    let mut message = message.replace(['\0', '\r', '\n'], " ");
    if message.len() > 2048 {
        let mut boundary = 2048;
        while !message.is_char_boundary(boundary) {
            boundary -= 1;
        }
        message.truncate(boundary);
    }
    McpPeerErrorV1 {
        code: code.to_owned(),
        message,
        dispatch,
        transport_lost,
    }
}
