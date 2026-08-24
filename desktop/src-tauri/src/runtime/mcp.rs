//! Desktop composition for a real, secret-safe MCP connection probe.
//!
//! The probe converts one unsaved Settings v2 server draft into an exact
//! capability-host binding, redeems only its named credential fields, performs
//! production initialization/discovery, and closes the transient session.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use aworkit_capability_host::{
    InjectionTargetV1, MCP_PROTOCOL_2024_11_05, MCP_PROTOCOL_2025_03_26, MCP_PROTOCOL_2025_06_18,
    MCP_PROTOCOL_2025_11_25, MCP_PROTOCOL_2026_07_28, McpPeerTransportConfigV1,
    McpServerManifestV1, McpSessionManager, McpStdioTransportConfigV1,
    McpStreamableHttpTransportConfigV1, McpTransportEndpointV1, McpTransportKindV1,
    ProductionMcpPeer, ProductionMcpPeerLimitsV1, RedeemLeaseRequestV1, SecretDeliveryV1,
    SecretFieldPlanV1, SecretLeaseClientV1, SecretLeaseHandleV1, SecretMaterializationError,
    SecretMaterializationPlanV1, SecretMaterializer,
};
use aworkit_protocol::{ProcessGeneration, StableId};
use serde::Serialize;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::{
    credentials::CredentialVault,
    dto::{McpProbeFeaturesV2, McpProbeRequestV2, McpProbeResultV2},
    settings_v2::{
        CredentialMetadataConfigurationV2, IntegrationTransportV2, NamedCredentialBindingV2,
        validate_http_url, validate_secret_free_stdio_argument,
    },
};

const HOST_GENERATION: ProcessGeneration = ProcessGeneration(1);
const MAX_BINDINGS: usize = 256;
const MAX_ARGUMENTS: usize = 512;
const MAX_TEXT_BYTES: usize = 16 * 1024;
const MAX_PROBE_CATALOG_ENTRIES: usize = 2_048;
const ADAPTER_VERSION: &str = "rmcp-3.1.4";

/// Production limits shared by the one-shot probe and frozen Run sessions.
pub(crate) fn production_peer_limits() -> ProductionMcpPeerLimitsV1 {
    ProductionMcpPeerLimitsV1 {
        initialization_timeout: Duration::from_secs(30),
        request_timeout: Duration::from_secs(30),
        close_timeout: Duration::from_secs(5),
        maximum_catalog_entries: MAX_PROBE_CATALOG_ENTRIES,
        maximum_catalog_bytes: 2 * 1024 * 1024,
        maximum_schema_bytes: 512 * 1024,
        maximum_result_bytes: 1024 * 1024,
    }
}

/// Everything one saved Settings server contributes to a frozen Run: the
/// core-attested manifest, the exact transport endpoint, and the secret
/// bindings the CredentialVault must materialize before the session opens.
pub(crate) struct PreparedMcpServerV1 {
    pub manifest: McpServerManifestV1,
    pub endpoint: McpTransportEndpointV1,
    pub secret_bindings: Vec<ProbeSecretBindingV1>,
}

/// Prepares one enabled saved MCP server for a frozen Run without opening a
/// session or touching the credential store.
pub(crate) fn prepare_mcp_server(
    server: &super::settings_v2::McpServerConfigurationV2,
    credentials: &[CredentialMetadataConfigurationV2],
) -> Result<PreparedMcpServerV1, String> {
    let server_id = stable(&server.id, "MCP server id")?;
    let (endpoint, bindings) = prepare_transport(&server.transport, credentials)?;
    let binding_hash = binding_hash(&server_id, &endpoint, &bindings)?;
    let transport = McpPeerTransportConfigV1 {
        server_id: server_id.clone(),
        binding_hash: binding_hash.clone(),
        endpoint: endpoint.clone(),
    }
    .transport_kind();
    let manifest = mcp_server_manifest(server_id, transport, binding_hash, &bindings);
    Ok(PreparedMcpServerV1 {
        manifest,
        endpoint,
        secret_bindings: bindings,
    })
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub(crate) enum ProbeSecretTargetV1 {
    Header(String),
    Environment(String),
}

impl ProbeSecretTargetV1 {
    fn injection_target(&self) -> InjectionTargetV1 {
        match self {
            Self::Header(name) => InjectionTargetV1::Header(name.clone()),
            Self::Environment(name) => InjectionTargetV1::Environment(name.clone()),
        }
    }

    fn canonical_key(&self) -> String {
        match self {
            Self::Header(name) => format!("header:{}", name.to_ascii_lowercase()),
            Self::Environment(name) if cfg!(windows) => {
                format!("environment:{}", name.to_ascii_uppercase())
            }
            Self::Environment(name) => format!("environment:{name}"),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProbeSecretBindingV1 {
    slot: String,
    target: ProbeSecretTargetV1,
    credential_ref: String,
    credential_field: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct McpProbeBindingDocumentV1<'a> {
    schema: &'static str,
    server_id: &'a StableId,
    endpoint: &'a McpTransportEndpointV1,
    secret_bindings: &'a [ProbeSecretBindingV1],
    adapter_version: &'static str,
    minimum_protocol_version: u16,
    maximum_protocol_version: u16,
    maximum_in_flight: usize,
    maximum_progress_events: usize,
}

struct OneShotSecretLeaseClient {
    lease_id: StableId,
    decision_id: StableId,
    invocation_id: StableId,
    fields: Mutex<Option<BTreeMap<String, Zeroizing<Vec<u8>>>>>,
}

impl SecretLeaseClientV1 for OneShotSecretLeaseClient {
    fn redeem(
        &self,
        request: &RedeemLeaseRequestV1,
    ) -> Result<SecretDeliveryV1, SecretMaterializationError> {
        if request.lease_id != self.lease_id
            || request.decision_id != self.decision_id
            || request.invocation_id != self.invocation_id
            || request.host_generation != HOST_GENERATION
        {
            return Err(SecretMaterializationError::LeaseDenied);
        }
        let mut fields = self
            .fields
            .lock()
            .map_err(|_| SecretMaterializationError::ChannelUnavailable)?;
        let delivery = fields
            .take()
            .ok_or(SecretMaterializationError::LeaseDenied)?;
        if delivery.keys().cloned().collect::<BTreeSet<_>>() != request.requested_fields {
            return Err(SecretMaterializationError::FieldMismatch);
        }
        Ok(SecretDeliveryV1 { fields: delivery })
    }

    fn revoke(&self, lease_id: &StableId) -> Result<(), SecretMaterializationError> {
        if lease_id != &self.lease_id {
            return Err(SecretMaterializationError::LeaseDenied);
        }
        self.fields
            .lock()
            .map_err(|_| SecretMaterializationError::ChannelUnavailable)?
            .take();
        Ok(())
    }
}

/// Runs one real production MCP initialize/discovery cycle against an unsaved
/// server draft. A disabled draft is intentionally probeable: the ephemeral
/// attestation enables only this transient connection and never changes saved
/// configuration.
pub(crate) fn probe_mcp_server(
    vault: &mut CredentialVault,
    credentials: &[CredentialMetadataConfigurationV2],
    request: McpProbeRequestV2,
) -> Result<McpProbeResultV2, String> {
    validate_text("MCP server name", &request.server.name, true)?;
    validate_text(
        "MCP probe draft fingerprint",
        &request.draft_fingerprint,
        true,
    )?;
    let server_id = stable(&request.server.id, "MCP server id")?;
    let (endpoint, bindings) = prepare_transport(&request.server.transport, credentials)?;
    let binding_hash = binding_hash(&server_id, &endpoint, &bindings)?;
    let config = McpPeerTransportConfigV1 {
        server_id: server_id.clone(),
        binding_hash: binding_hash.clone(),
        endpoint,
    };
    let transport = config.transport_kind();
    let limits = ProductionMcpPeerLimitsV1 {
        initialization_timeout: Duration::from_secs(30),
        request_timeout: Duration::from_secs(30),
        close_timeout: Duration::from_secs(5),
        maximum_catalog_entries: MAX_PROBE_CATALOG_ENTRIES,
        maximum_catalog_bytes: 2 * 1024 * 1024,
        maximum_schema_bytes: 512 * 1024,
        maximum_result_bytes: 1024 * 1024,
    };
    let peer = Arc::new(
        ProductionMcpPeer::with_limits(vec![config], limits)
            .map_err(|error| format!("MCP transport configuration is invalid: {error}"))?,
    );

    let started = Instant::now();
    if let Some(materialization) = materialize_bindings(vault, &bindings)? {
        peer.stage_materialized_secrets(&server_id, materialization)
            .map_err(|error| format!("MCP credential binding could not be staged: {error}"))?;
    }
    let manager = McpSessionManager::new(HOST_GENERATION, peer);
    let manifest = mcp_server_manifest(
        server_id.clone(),
        transport,
        binding_hash.clone(),
        &bindings,
    );
    let snapshot = manager
        .open(manifest)
        .map_err(|error| format!("MCP initialization or discovery failed: {error}"))?;

    let protocol_version = protocol_label(snapshot.protocol_version)
        .ok_or_else(|| "MCP server negotiated an unknown protocol version".to_owned());
    let close_result = manager.close(&server_id);
    let protocol_version = protocol_version?;
    close_result.map_err(|error| format!("MCP probe could not close its session: {error}"))?;

    let tool_names = snapshot
        .catalog
        .tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    let resource_names = snapshot.catalog.resources.clone();
    let prompt_names = snapshot.catalog.prompts.clone();
    let catalog_count = tool_names
        .len()
        .saturating_add(resource_names.len())
        .saturating_add(prompt_names.len());
    let latency_millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(McpProbeResultV2 {
        server_id: request.server.id,
        protocol_version: protocol_version.into(),
        features: McpProbeFeaturesV2 {
            tools: snapshot.features.tools,
            resources: snapshot.features.resources,
            prompts: snapshot.features.prompts,
            progress: snapshot.features.progress,
            cancellation: snapshot.features.cancellation,
        },
        tool_names,
        resource_names,
        prompt_names,
        binding_hash,
        catalog_hash: snapshot.catalog_hash,
        latency_millis,
        draft_fingerprint: request.draft_fingerprint,
        message: format!(
            "Connected using MCP {protocol_version}; discovered {catalog_count} catalog item(s)."
        ),
    })
}

pub(crate) fn prepare_transport(
    transport: &IntegrationTransportV2,
    credentials: &[CredentialMetadataConfigurationV2],
) -> Result<(McpTransportEndpointV1, Vec<ProbeSecretBindingV1>), String> {
    let (endpoint, bindings, target_kind) = match transport {
        IntegrationTransportV2::Http { url, headers } => {
            validate_http_url("MCP HTTP URL", url)?;
            (
                McpTransportEndpointV1::StreamableHttp(McpStreamableHttpTransportConfigV1 {
                    endpoint: url.clone(),
                    allow_stateless: true,
                    maximum_sse_event_bytes: 1024 * 1024,
                    // Named headers are injected verbatim. A user who configures
                    // Authorization therefore controls the complete value.
                    bearer_token_secret_slot: None,
                }),
                headers,
                "header",
            )
        }
        IntegrationTransportV2::Stdio {
            command,
            args,
            cwd: _,
            env,
        } => {
            validate_text("MCP STDIO command", command, true)?;
            let executable = PathBuf::from(unquote_runtime_path(command));
            if !launchable_mcp_command(&executable) {
                return Err(
                    "MCP STDIO command must be an absolute executable path or one bare command name from PATH"
                        .into(),
                );
            }
            if args.len() > MAX_ARGUMENTS {
                return Err(format!(
                    "MCP STDIO command exceeds the {MAX_ARGUMENTS} argument limit"
                ));
            }
            for argument in args {
                validate_secret_free_stdio_argument("MCP STDIO argument", argument)?;
            }
            (
                McpTransportEndpointV1::Stdio(McpStdioTransportConfigV1 {
                    executable,
                    arguments: args.clone(),
                    // MCP servers use the app's inherited directory. The generic
                    // integration schema keeps `cwd` for external agents only.
                    working_directory: None,
                    public_environment: BTreeMap::new(),
                }),
                env,
                "environment",
            )
        }
    };
    let bindings = prepare_secret_bindings(bindings, target_kind, credentials)?;
    Ok((endpoint, bindings))
}

pub(crate) fn prepare_secret_bindings(
    bindings: &[NamedCredentialBindingV2],
    target_kind: &str,
    credentials: &[CredentialMetadataConfigurationV2],
) -> Result<Vec<ProbeSecretBindingV1>, String> {
    if bindings.len() > MAX_BINDINGS {
        return Err(format!(
            "MCP transport exceeds the {MAX_BINDINGS} credential-binding limit"
        ));
    }
    let credential_index = credentials
        .iter()
        .map(|metadata| (metadata.credential_ref.as_str(), metadata))
        .collect::<BTreeMap<_, _>>();
    if credential_index.len() != credentials.len() {
        return Err("saved credential metadata contains duplicate references".into());
    }
    let mut prepared = Vec::with_capacity(bindings.len());
    let mut target_names = BTreeSet::new();
    for binding in bindings {
        let target = match target_kind {
            "header" => {
                if !valid_header_name(&binding.name) || reserved_header_name(&binding.name) {
                    return Err(format!(
                        "MCP header credential target '{}' is invalid or reserved",
                        binding.name
                    ));
                }
                ProbeSecretTargetV1::Header(binding.name.clone())
            }
            "environment" => {
                if !valid_environment_name(&binding.name) {
                    return Err(format!(
                        "MCP environment credential target '{}' is invalid",
                        binding.name
                    ));
                }
                ProbeSecretTargetV1::Environment(binding.name.clone())
            }
            _ => return Err("MCP credential target kind is invalid".into()),
        };
        if !target_names.insert(target.canonical_key()) {
            return Err(format!(
                "MCP transport contains duplicate credential target '{}'",
                binding.name
            ));
        }
        stable(&binding.credential_ref, "credential reference")?;
        validate_symbol("credential field", &binding.field)?;
        let metadata = credential_index
            .get(binding.credential_ref.as_str())
            .ok_or_else(|| {
                format!(
                    "MCP transport references unknown credential '{}'",
                    binding.credential_ref
                )
            })?;
        if !metadata
            .field_names
            .iter()
            .any(|field| field == &binding.field)
        {
            return Err(format!(
                "MCP transport references unknown field '{}' on credential '{}'",
                binding.field, binding.credential_ref
            ));
        }
        if metadata.bound_provider_id.is_some() || metadata.bound_endpoint.is_some() {
            return Err(format!(
                "credential '{}' is provider-scoped and cannot be injected into an MCP server",
                binding.credential_ref
            ));
        }
        prepared.push(ProbeSecretBindingV1 {
            slot: String::new(),
            target,
            credential_ref: binding.credential_ref.clone(),
            credential_field: binding.field.clone(),
        });
    }
    prepared.sort_by(|left, right| {
        (
            left.target.canonical_key(),
            left.credential_ref.as_str(),
            left.credential_field.as_str(),
        )
            .cmp(&(
                right.target.canonical_key(),
                right.credential_ref.as_str(),
                right.credential_field.as_str(),
            ))
    });
    for (index, binding) in prepared.iter_mut().enumerate() {
        binding.slot = format!("mcp_secret_{index:04}");
    }
    Ok(prepared)
}

pub(crate) fn materialize_bindings(
    vault: &mut CredentialVault,
    bindings: &[ProbeSecretBindingV1],
) -> Result<Option<aworkit_capability_host::SecretMaterializationV1>, String> {
    if bindings.is_empty() {
        return Ok(None);
    }
    let mut requested = BTreeMap::<String, BTreeSet<String>>::new();
    for binding in bindings {
        requested
            .entry(binding.credential_ref.clone())
            .or_default()
            .insert(binding.credential_field.clone());
    }
    let mut resolved = BTreeMap::new();
    for (credential_ref, fields) in requested {
        resolved.insert(
            credential_ref.clone(),
            vault.resolve_fields(&credential_ref, fields)?,
        );
    }
    let mut delivery = BTreeMap::new();
    for binding in bindings {
        let value = resolved
            .get(&binding.credential_ref)
            .and_then(|fields| fields.get(&binding.credential_field))
            .ok_or_else(|| "credential store omitted an approved MCP field".to_owned())?;
        delivery.insert(
            binding.slot.clone(),
            Zeroizing::new(value.as_slice().to_vec()),
        );
    }

    let lease_id = stable("lease.desktop.mcp-probe", "MCP materialization lease")?;
    let decision_id = stable("decision.desktop.mcp-probe", "MCP probe decision")?;
    let invocation_id = stable("invocation.desktop.mcp-probe", "MCP probe invocation")?;
    let client = OneShotSecretLeaseClient {
        lease_id: lease_id.clone(),
        decision_id: decision_id.clone(),
        invocation_id: invocation_id.clone(),
        fields: Mutex::new(Some(delivery)),
    };
    SecretMaterializer::new(client)
        .materialize(&SecretMaterializationPlanV1 {
            decision_id,
            invocation_id,
            host_generation: HOST_GENERATION,
            lease: SecretLeaseHandleV1 { lease_id },
            fields: bindings
                .iter()
                .map(|binding| SecretFieldPlanV1 {
                    field: binding.slot.clone(),
                    target: binding.target.injection_target(),
                })
                .collect(),
        })
        .map(Some)
        .map_err(|error| format!("MCP credential materialization failed: {error}"))
}

pub(crate) fn mcp_server_manifest(
    server_id: StableId,
    transport: McpTransportKindV1,
    binding_hash: String,
    bindings: &[ProbeSecretBindingV1],
) -> McpServerManifestV1 {
    McpServerManifestV1 {
        server_id,
        adapter_version: ADAPTER_VERSION.into(),
        binding_hash,
        host_generation: HOST_GENERATION,
        configured: true,
        enabled: true,
        core_attested: true,
        transport,
        minimum_protocol_version: MCP_PROTOCOL_2024_11_05,
        maximum_protocol_version: MCP_PROTOCOL_2026_07_28,
        maximum_in_flight: 1,
        maximum_progress_events: 128,
        secret_slots: bindings
            .iter()
            .map(|binding| binding.slot.clone())
            .collect(),
        workspace_roots: Vec::new(),
    }
}

pub(crate) fn binding_hash(
    server_id: &StableId,
    endpoint: &McpTransportEndpointV1,
    bindings: &[ProbeSecretBindingV1],
) -> Result<String, String> {
    let document = McpProbeBindingDocumentV1 {
        schema: "aworkit.desktop.mcp-probe-binding.v1",
        server_id,
        endpoint,
        secret_bindings: bindings,
        adapter_version: ADAPTER_VERSION,
        minimum_protocol_version: MCP_PROTOCOL_2024_11_05,
        maximum_protocol_version: MCP_PROTOCOL_2026_07_28,
        maximum_in_flight: 1,
        maximum_progress_events: 128,
    };
    let canonical = serde_jcs::to_vec(&document)
        .map_err(|_| "MCP transport binding could not be canonicalized".to_owned())?;
    Ok(format!("sha256:{:x}", Sha256::digest(canonical)))
}

fn protocol_label(version: u16) -> Option<&'static str> {
    match version {
        MCP_PROTOCOL_2024_11_05 => Some("2024-11-05"),
        MCP_PROTOCOL_2025_03_26 => Some("2025-03-26"),
        MCP_PROTOCOL_2025_06_18 => Some("2025-06-18"),
        MCP_PROTOCOL_2025_11_25 => Some("2025-11-25"),
        MCP_PROTOCOL_2026_07_28 => Some("2026-07-28"),
        _ => None,
    }
}

fn stable(value: &str, label: &str) -> Result<StableId, String> {
    StableId::parse(value.to_owned())
        .map_err(|_| format!("{label} '{value}' is not a valid stable identifier"))
}

fn validate_text(label: &str, value: &str, nonempty: bool) -> Result<(), String> {
    if value.len() > MAX_TEXT_BYTES {
        return Err(format!("{label} exceeds the {MAX_TEXT_BYTES}-byte limit"));
    }
    if value.contains('\0') {
        return Err(format!("{label} contains NUL"));
    }
    if nonempty && value.trim().is_empty() {
        return Err(format!("{label} cannot be empty"));
    }
    Ok(())
}

/// Accept an executable path copied from a Windows shell with one quote pair.
fn unquote_runtime_path(value: &str) -> &str {
    let trimmed = value.trim();
    if trimmed.len() >= 2
        && matches!(trimmed.as_bytes()[0], b'\'' | b'"')
        && trimmed.as_bytes()[0] == *trimmed.as_bytes().last().expect("length checked")
    {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    }
}

fn launchable_mcp_command(path: &std::path::Path) -> bool {
    path.is_absolute()
        || path.components().count() == 1
            && matches!(
                path.components().next(),
                Some(std::path::Component::Normal(_))
            )
}

fn validate_symbol(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':' | b'/')
        })
    {
        Err(format!("{label} '{value}' is invalid"))
    } else {
        Ok(())
    }
}

fn valid_header_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_environment_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && !value.starts_with('=')
}

fn reserved_header_name(value: &str) -> bool {
    let name = value.to_ascii_lowercase();
    matches!(
        name.as_str(),
        "accept"
            | "connection"
            | "content-length"
            | "content-type"
            | "expect"
            | "host"
            | "proxy-authorization"
            | "transfer-encoding"
            | "upgrade"
            | "mcp-session-id"
            | "mcp-protocol-version"
            | "last-event-id"
            | "mcp-method"
            | "mcp-name"
    ) || name.starts_with("mcp-param-")
}
