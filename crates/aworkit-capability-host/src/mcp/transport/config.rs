//! Non-secret configuration and bounded runtime policy for production MCP peers.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::Duration,
};

use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::McpTransportKindV1;

/// Numeric protocol identifiers used by the Aworkit-owned MCP contracts.
///
/// The wire protocol still uses the official date strings. Keeping the mapping
/// here lets the pre-existing attestation contract express a closed version
/// range without exposing SDK-native types to the trusted core.
pub const MCP_PROTOCOL_2024_11_05: u16 = 1;
pub const MCP_PROTOCOL_2025_03_26: u16 = 2;
pub const MCP_PROTOCOL_2025_06_18: u16 = 3;
pub const MCP_PROTOCOL_2025_11_25: u16 = 4;
pub const MCP_PROTOCOL_2026_07_28: u16 = 5;

const MAX_ARGUMENTS: usize = 512;
const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_ENVIRONMENT_ENTRIES: usize = 256;
const MAX_ENVIRONMENT_VALUE_BYTES: usize = 64 * 1024;
const MIN_SSE_EVENT_BYTES: usize = 1024;
const MAX_SSE_EVENT_BYTES: usize = 16 * 1024 * 1024;

/// One immutable, core-hash-bound transport configuration.
///
/// This document is intentionally secret-free and therefore may be persisted,
/// serialized, or shown in diagnostics. Secret environment/header values are
/// accepted only through `ProductionMcpPeer::stage_materialized_secrets`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpPeerTransportConfigV1 {
    pub server_id: StableId,
    pub binding_hash: String,
    pub endpoint: McpTransportEndpointV1,
}

impl McpPeerTransportConfigV1 {
    #[must_use]
    pub const fn transport_kind(&self) -> McpTransportKindV1 {
        match &self.endpoint {
            McpTransportEndpointV1::Stdio(_) => McpTransportKindV1::Stdio,
            McpTransportEndpointV1::StreamableHttp(_) => McpTransportKindV1::StreamableHttp,
        }
    }
}

/// Transport-specific public configuration. No variant contains credentials.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "transport",
    content = "configuration",
    rename_all = "snake_case"
)]
pub enum McpTransportEndpointV1 {
    Stdio(McpStdioTransportConfigV1),
    StreamableHttp(McpStreamableHttpTransportConfigV1),
}

/// STDIO process configuration.
///
/// The executable and working directory must be absolute. The child starts
/// with a cleared environment; entries here are explicitly public. Materialized
/// secret fields targeting environment variables are injected separately.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpStdioTransportConfigV1 {
    pub executable: PathBuf,
    #[serde(default)]
    pub arguments: Vec<String>,
    pub working_directory: Option<PathBuf>,
    #[serde(default)]
    pub public_environment: BTreeMap<String, String>,
}

/// Streamable HTTP configuration for stateful legacy and stateless modern peers.
///
/// `bearer_token_secret_slot` names a materialized field; it never contains the
/// token itself. All other materialized `Header` targets are sent verbatim.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpStreamableHttpTransportConfigV1 {
    pub endpoint: String,
    #[serde(default = "default_allow_stateless")]
    pub allow_stateless: bool,
    #[serde(default = "default_maximum_sse_event_bytes")]
    pub maximum_sse_event_bytes: usize,
    pub bearer_token_secret_slot: Option<String>,
}

const fn default_allow_stateless() -> bool {
    true
}

const fn default_maximum_sse_event_bytes() -> usize {
    1024 * 1024
}

/// Host-side bounds applied before data reaches `McpSessionManager`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductionMcpPeerLimitsV1 {
    pub initialization_timeout: Duration,
    pub request_timeout: Duration,
    pub close_timeout: Duration,
    pub maximum_catalog_entries: usize,
    pub maximum_catalog_bytes: usize,
    pub maximum_schema_bytes: usize,
    pub maximum_result_bytes: usize,
}

impl Default for ProductionMcpPeerLimitsV1 {
    fn default() -> Self {
        Self {
            initialization_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(300),
            close_timeout: Duration::from_secs(5),
            maximum_catalog_entries: 16_384,
            maximum_catalog_bytes: 8 * 1024 * 1024,
            maximum_schema_bytes: 1024 * 1024,
            maximum_result_bytes: 8 * 1024 * 1024,
        }
    }
}

pub(super) fn validate_configs(
    configs: Vec<McpPeerTransportConfigV1>,
) -> Result<BTreeMap<String, McpPeerTransportConfigV1>, McpTransportConfigurationError> {
    let mut indexed = BTreeMap::new();
    for config in configs {
        validate_config(&config)?;
        let key = config.server_id.as_str().to_owned();
        if indexed.insert(key, config).is_some() {
            return Err(McpTransportConfigurationError::DuplicateServer);
        }
    }
    Ok(indexed)
}

pub(super) fn validate_limits(
    limits: &ProductionMcpPeerLimitsV1,
) -> Result<(), McpTransportConfigurationError> {
    if limits.initialization_timeout.is_zero()
        || limits.request_timeout.is_zero()
        || limits.close_timeout.is_zero()
        || limits.maximum_catalog_entries == 0
        || limits.maximum_catalog_entries > 16_384
        || limits.maximum_catalog_bytes == 0
        || limits.maximum_schema_bytes == 0
        || limits.maximum_schema_bytes > limits.maximum_catalog_bytes
        || limits.maximum_result_bytes == 0
    {
        return Err(McpTransportConfigurationError::InvalidLimits);
    }
    Ok(())
}

fn validate_config(
    config: &McpPeerTransportConfigV1,
) -> Result<(), McpTransportConfigurationError> {
    if !valid_hash(&config.binding_hash) {
        return Err(McpTransportConfigurationError::InvalidBindingHash);
    }
    match &config.endpoint {
        McpTransportEndpointV1::Stdio(stdio) => validate_stdio(stdio),
        McpTransportEndpointV1::StreamableHttp(http) => validate_http(http),
    }
}

fn validate_stdio(
    config: &McpStdioTransportConfigV1,
) -> Result<(), McpTransportConfigurationError> {
    if !absolute_nonempty_path(&config.executable)
        || config
            .working_directory
            .as_deref()
            .is_some_and(|path| !absolute_nonempty_path(path))
        || config.arguments.len() > MAX_ARGUMENTS
        || config
            .arguments
            .iter()
            .any(|argument| argument.len() > MAX_ARGUMENT_BYTES || argument.contains('\0'))
        || config.public_environment.len() > MAX_ENVIRONMENT_ENTRIES
    {
        return Err(McpTransportConfigurationError::InvalidStdioConfiguration);
    }
    let mut names = BTreeSet::new();
    for (name, value) in &config.public_environment {
        if !valid_environment_name(name)
            || value.len() > MAX_ENVIRONMENT_VALUE_BYTES
            || value.contains('\0')
            || !names.insert(fold_environment_name(name))
        {
            return Err(McpTransportConfigurationError::InvalidStdioConfiguration);
        }
    }
    Ok(())
}

fn validate_http(
    config: &McpStreamableHttpTransportConfigV1,
) -> Result<(), McpTransportConfigurationError> {
    let url = reqwest_mcp::Url::parse(&config.endpoint)
        .map_err(|_| McpTransportConfigurationError::InvalidHttpConfiguration)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !config.allow_stateless
        || !(MIN_SSE_EVENT_BYTES..=MAX_SSE_EVENT_BYTES).contains(&config.maximum_sse_event_bytes)
        || config
            .bearer_token_secret_slot
            .as_deref()
            .is_some_and(|slot| !valid_slot_name(slot))
    {
        return Err(McpTransportConfigurationError::InvalidHttpConfiguration);
    }
    Ok(())
}

pub(super) fn valid_environment_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && !value.starts_with('=')
}

pub(super) fn fold_environment_name(value: &str) -> String {
    if cfg!(windows) {
        value.to_ascii_uppercase()
    } else {
        value.to_owned()
    }
}

fn valid_slot_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn absolute_nonempty_path(value: &Path) -> bool {
    value.is_absolute() && !value.as_os_str().is_empty()
}

fn valid_hash(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum McpTransportConfigurationError {
    #[error("duplicate MCP server transport configuration")]
    DuplicateServer,
    #[error("MCP transport binding hash is malformed")]
    InvalidBindingHash,
    #[error("MCP STDIO transport configuration is malformed")]
    InvalidStdioConfiguration,
    #[error("MCP Streamable HTTP transport configuration is malformed")]
    InvalidHttpConfiguration,
    #[error("MCP production peer limits are malformed")]
    InvalidLimits,
    #[error("MCP transport runtime could not be created")]
    RuntimeUnavailable,
    #[error("MCP server has no transport configuration")]
    UnknownServer,
    #[error("MCP server already has staged secret material")]
    SecretsAlreadyStaged,
    #[error("MCP server is active and cannot accept replacement secret material")]
    ServerActive,
    #[error("MCP secret material does not match its attested slots or transport targets")]
    InvalidSecretMaterial,
    #[error("MCP transport state lock is unavailable")]
    Poisoned,
}
