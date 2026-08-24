//! Frozen per-Run MCP tool runtime used by the desktop Agent tool loop.
//!
//! The runtime owns the capability-host session manager plus one replaceable
//! transport peer. Production installs a `ProductionMcpPeer` whose transports
//! are built from saved Settings at first freeze; tests install a scripted
//! peer. Sessions are keyed by the core-attested manifest, so a changed server
//! binding fails closed with binding drift instead of hot-replacing an active
//! Run's session. Each Run additionally freezes the exact discovery snapshot
//! it opened so later Runs cannot observe a silently mutated catalog.

use std::{collections::BTreeMap, sync::Mutex};

use aworkit_capability_host::{
    CancellationToken, McpCallOutcomeV1, McpCallV1, McpCancellationReceiptV1,
    McpCapabilitySnapshotV1, McpPeerPort, McpServerManifestV1, McpSessionManager,
    McpTransportEndpointV1, ProductionMcpPeer, SecretMaterializationV1,
};
use aworkit_protocol::{ProcessGeneration, StableId};
use std::sync::Arc;

/// The capability-id prefix for MCP tools: `mcp://<server>/<tool>`.
pub(crate) const MCP_CAPABILITY_PREFIX: &str = "mcp://";
pub(crate) const MCP_ADAPTER_ID: &str = "adapter.mcp.v1";
pub(crate) const MCP_ADAPTER_VERSION: &str = "1.0.0";
pub(crate) const MCP_SCOPE: &str = "mcp.invoke";
/// A model-facing MCP tool name uses the `mcp__<server>__<tool>` spelling so
/// provider tool lists stay readable. Provider tool names only admit
/// `[A-Za-z0-9_-]`; any other character (for example the `.` StableIds allow in
/// server ids, or tool-name punctuation) is deterministically folded to `_`
/// so the name always passes provider-side name validation.
pub(crate) fn mcp_provider_name(server_id: &str, tool: &str) -> String {
    let sanitize = |value: &str| {
        value
            .bytes()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-') {
                    byte
                } else {
                    b'_'
                }
            })
            .collect::<Vec<_>>()
    };
    format!(
        "mcp__{}__{}",
        String::from_utf8(sanitize(server_id)).expect("sanitized server id"),
        String::from_utf8(sanitize(tool)).expect("sanitized tool name"),
    )
}

/// Split an `mcp://<server>/<tool>` capability id into its two exact parts.
pub(crate) fn split_mcp_capability(capability_id: &str) -> Result<(&str, &str), String> {
    let remainder = capability_id
        .strip_prefix(MCP_CAPABILITY_PREFIX)
        .ok_or_else(|| format!("capability '{capability_id}' has no mcp:// prefix"))?;
    let (server, tool) = remainder
        .split_once('/')
        .filter(|(server, tool)| !server.is_empty() && !tool.is_empty() && !tool.contains('/'))
        .ok_or_else(|| {
            format!("capability '{capability_id}' must name exactly mcp://<server>/<tool>")
        })?;
    Ok((server, tool))
}

type McpSecretStager =
    dyn Fn(&StableId, SecretMaterializationV1) -> Result<(), String> + Send + Sync;

/// One frozen-Run server preparation handed from the owning service to the
/// pipeline: the core-attested manifest, the exact transport endpoint used to
/// build the production peer, and the already-materialized credential slots.
/// No secret values leave this struct.
pub(crate) struct McpRunServerPreparationV1 {
    pub manifest: McpServerManifestV1,
    pub endpoint: McpTransportEndpointV1,
    pub materialization: Option<SecretMaterializationV1>,
}

#[derive(Default)]
struct McpToolRuntimeStateV1 {
    peer: Option<Arc<dyn McpPeerPort>>,
    manager: Option<Arc<McpSessionManager>>,
    stager: Option<Arc<McpSecretStager>>,
    /// Run-frozen discovery snapshots: run id -> server id -> snapshot.
    frozen: BTreeMap<String, BTreeMap<String, McpCapabilitySnapshotV1>>,
    /// Dispatch-scoped cancellation tokens keyed by the broker's scoped token
    /// id. One entry per admitted MCP dispatch; removed once settled.
    dispatch_tokens: BTreeMap<String, CancellationToken>,
}

/// Session manager plus per-Run freeze state. The peer is installed exactly
/// once per application generation; a second differing installation fails
/// closed rather than swapping sessions under an active Run.
pub(crate) struct McpToolRuntimeV1 {
    generation: ProcessGeneration,
    state: Mutex<McpToolRuntimeStateV1>,
}

impl McpToolRuntimeV1 {
    /// Creates an uninstalled runtime. Sessions cannot open until a peer is
    /// installed by the owning service (production) or a test.
    #[must_use]
    pub(crate) fn new(generation: ProcessGeneration) -> Self {
        Self {
            generation,
            state: Mutex::new(McpToolRuntimeStateV1::default()),
        }
    }

    /// Installs a production peer and wires credential staging through it.
    /// Fails closed when a peer is already installed for this generation.
    pub(crate) fn install_production_peer(
        &self,
        peer: Arc<ProductionMcpPeer>,
    ) -> Result<(), String> {
        let stager: Arc<McpSecretStager> = Arc::new({
            let peer = peer.clone();
            move |server_id: &StableId, materialization: SecretMaterializationV1| {
                peer.stage_materialized_secrets(server_id, materialization)
                    .map_err(|error| format!("MCP credential binding could not be staged: {error}"))
            }
        });
        self.install(peer, Some(stager))
    }

    /// Installs a scripted/test peer with no secret staging.
    pub(crate) fn install_scripted_peer(&self, peer: Arc<dyn McpPeerPort>) -> Result<(), String> {
        self.install(peer, None)
    }

    /// Whether no transport peer has been installed for this generation yet.
    pub(crate) fn needs_install(&self) -> Result<bool, String> {
        Ok(self
            .state
            .lock()
            .map_err(|_| "MCP tool runtime lock poisoned".to_owned())?
            .peer
            .is_none())
    }

    fn install(
        &self,
        peer: Arc<dyn McpPeerPort>,
        stager: Option<Arc<McpSecretStager>>,
    ) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "MCP tool runtime lock poisoned".to_owned())?;
        if state.peer.is_some() {
            return Err(
                "an MCP transport peer is already installed for this application generation".into(),
            );
        }
        let manager = Arc::new(McpSessionManager::new(self.generation, peer.clone()));
        state.peer = Some(peer);
        state.manager = Some(manager);
        state.stager = stager;
        Ok(())
    }

    /// Opens or reuses the exact attested session for a frozen Run and records
    /// the discovery snapshot in that Run's freeze map. A changed manifest
    /// fails closed with binding drift.
    pub(crate) fn open_frozen(
        &self,
        run_id: &StableId,
        manifest: &McpServerManifestV1,
    ) -> Result<McpCapabilitySnapshotV1, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "MCP tool runtime lock poisoned".to_owned())?;
        let run_key = run_id.as_str().to_owned();
        let server_key = manifest.server_id.as_str().to_owned();
        if let Some(snapshot) = state
            .frozen
            .get(&run_key)
            .and_then(|sessions| sessions.get(&server_key))
        {
            return Ok(snapshot.clone());
        }
        let manager = state.manager.clone().ok_or_else(|| {
            "no MCP transport peer is installed for this application generation".to_owned()
        })?;
        let snapshot = manager
            .open(manifest.clone())
            .map_err(|error| format!("MCP session for '{server_key}' failed: {error}"))?;
        state
            .frozen
            .entry(run_key)
            .or_default()
            .insert(server_key, snapshot.clone());
        Ok(snapshot)
    }

    /// Stages invocation-scoped credential material on the installed peer.
    pub(crate) fn stage_secrets(
        &self,
        server_id: &StableId,
        materialization: SecretMaterializationV1,
    ) -> Result<(), String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "MCP tool runtime lock poisoned".to_owned())?;
        let stager = state.stager.clone().ok_or_else(|| {
            "the installed MCP peer does not accept credential staging".to_owned()
        })?;
        stager(server_id, materialization)
    }

    /// Invokes exactly one discovered operation on the frozen session.
    pub(crate) fn invoke(
        &self,
        server_id: &StableId,
        call: &McpCallV1,
    ) -> Result<McpCallOutcomeV1, String> {
        let manager = self
            .state
            .lock()
            .map_err(|_| "MCP tool runtime lock poisoned".to_owned())?
            .manager
            .clone()
            .ok_or_else(|| {
                "no MCP transport peer is installed for this application generation".to_owned()
            })?;
        manager
            .invoke(server_id, call)
            .map_err(|error| format!("MCP call failed: {error}"))
    }

    /// Registers the dispatch-scoped cancellation token for one admitted MCP
    /// dispatch and returns it. The scoped id comes from the broker envelope,
    /// so a cancel request can target exactly one session invocation.
    pub(crate) fn register_dispatch_token(
        &self,
        token_id: &StableId,
    ) -> Result<CancellationToken, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "MCP tool runtime lock poisoned".to_owned())?;
        let token = CancellationToken::default();
        state
            .dispatch_tokens
            .insert(token_id.as_str().to_owned(), token.clone());
        Ok(token)
    }

    /// Drops the scoped cancellation entry once a dispatch settled.
    pub(crate) fn unregister_dispatch_token(&self, token_id: &StableId) {
        if let Ok(mut state) = self.state.lock() {
            state.dispatch_tokens.remove(token_id.as_str());
        }
    }

    /// Session-scoped cancellation: cancels the dispatch token and asks the
    /// session manager to cancel the in-flight invocation through its reserved
    /// control path. Unsupported or lost cancellation never becomes proof
    /// that an effect did not occur — that classification stays with the
    /// session manager's receipts.
    pub(crate) fn cancel_dispatch(
        &self,
        token_id: &StableId,
        server_id: &StableId,
        invocation_id: &StableId,
    ) -> Result<McpCancellationReceiptV1, String> {
        let (manager, token) = {
            let state = self
                .state
                .lock()
                .map_err(|_| "MCP tool runtime lock poisoned".to_owned())?;
            (
                state.manager.clone().ok_or_else(|| {
                    "no MCP transport peer is installed for this application generation".to_owned()
                })?,
                state
                    .dispatch_tokens
                    .get(token_id.as_str())
                    .cloned()
                    .ok_or_else(|| "unknown MCP dispatch cancellation scope".to_owned())?,
            )
        };
        token.cancel();
        manager
            .cancel(server_id, invocation_id)
            .map_err(|error| format!("MCP cancellation failed: {error}"))
    }
}
