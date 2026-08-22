//! Pinned backend identity, isolation profile, and enforcement evidence.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_IDENTITY_BYTES: usize = 256;
const MAX_EVIDENCE_BYTES: usize = 4 * 1024;

/// Returns the canonical content digest used by transfer and identity fields.
#[must_use]
pub fn content_hash_v1(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

pub(crate) fn is_content_hash(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn is_bounded_identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_IDENTITY_BYTES && !value.chars().any(char::is_control)
}

pub(crate) fn is_bounded_evidence(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_EVIDENCE_BYTES && !value.contains('\0')
}

/// Whether isolation is mandatory or merely an explicit optional binding.
///
/// Neither value permits this runtime to execute on the host. `Preferred`
/// lets a higher authority present an unavailable result and make a new,
/// separately approved decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationRequirementV1 {
    Required,
    Preferred,
}

/// Where the backend says execution takes place.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendExecutionLocationV1 {
    Local,
    Remote,
}

/// Exact adapter and environment identity frozen by trusted-core resolution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PinnedBackendIdentityV1 {
    pub backend_id: String,
    pub adapter_version: String,
    pub adapter_hash: String,
    pub environment_id: String,
    pub environment_hash: String,
}

impl PinnedBackendIdentityV1 {
    pub(crate) fn validate(&self) -> bool {
        is_bounded_identity(&self.backend_id)
            && is_bounded_identity(&self.adapter_version)
            && is_content_hash(&self.adapter_hash)
            && is_bounded_identity(&self.environment_id)
            && is_content_hash(&self.environment_hash)
    }
}

/// Enforcement categories are separate so unsupported strength is precise.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnforcementCategoryV1 {
    Mounts,
    Network,
    User,
    Processes,
    Resources,
    ResidualState,
}

impl EnforcementCategoryV1 {
    pub(crate) const ALL: [Self; 6] = [
        Self::Mounts,
        Self::Network,
        Self::User,
        Self::Processes,
        Self::Resources,
        Self::ResidualState,
    ];
}

/// Static backend claims. These are capabilities to verify, not proof that a
/// particular session is isolated.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolationBackendManifestV1 {
    pub backend_id: String,
    pub adapter_version: String,
    pub adapter_hash: String,
    pub execution_location: BackendExecutionLocationV1,
    pub supported_hosts: BTreeSet<String>,
    pub verifiable_enforcement: BTreeSet<EnforcementCategoryV1>,
    pub enforces_deadlines: bool,
    pub supports_cancellation: bool,
    pub verifies_cleanup: bool,
    pub maximum_transfer_bytes: usize,
}

impl IsolationBackendManifestV1 {
    /// Validates the manifest as an isolation-capable adapter declaration.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        is_bounded_identity(&self.backend_id)
            && is_bounded_identity(&self.adapter_version)
            && is_content_hash(&self.adapter_hash)
            && !self.supported_hosts.is_empty()
            && self
                .supported_hosts
                .iter()
                .all(|host| is_bounded_identity(host))
            && EnforcementCategoryV1::ALL
                .iter()
                .all(|category| self.verifiable_enforcement.contains(category))
            && self.enforces_deadlines
            && self.supports_cancellation
            && self.verifies_cleanup
            && self.maximum_transfer_bytes > 0
    }

    #[must_use]
    pub(crate) fn matches_pin(&self, pin: &PinnedBackendIdentityV1) -> bool {
        self.backend_id == pin.backend_id
            && self.adapter_version == pin.adapter_version
            && self.adapter_hash == pin.adapter_hash
    }
}

/// Why an explicitly selected backend cannot be used.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum BackendUnavailableReasonV1 {
    NotInstalled,
    Disabled,
    UnsupportedHost { host: String },
    Unhealthy { detail: String },
    RemoteUnreachable { detail: String },
    IdentityDrift { field: String },
    ProfileUnsupported { category: EnforcementCategoryV1 },
    VerificationUnavailable { category: EnforcementCategoryV1 },
    DeadlineElapsed,
    CancelledBeforeDispatch,
}

/// Availability probing never authorizes execution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "reason")]
pub enum BackendAvailabilityV1 {
    Available,
    Unavailable(BackendUnavailableReasonV1),
}

/// An exact source-to-target mount realization.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MountAccessV1 {
    ReadOnly,
    ReadWrite,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MountRealizationV1 {
    pub source: String,
    pub source_identity: String,
    pub target: String,
    pub access: MountAccessV1,
}

impl MountRealizationV1 {
    fn validate(&self) -> bool {
        is_bounded_identity(&self.source)
            && is_content_hash(&self.source_identity)
            && is_bounded_identity(&self.target)
    }
}

/// Network access realized by the backend.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode", content = "endpoints")]
pub enum NetworkPolicyV1 {
    Denied,
    LoopbackOnly,
    AllowList(BTreeSet<String>),
}

impl NetworkPolicyV1 {
    fn validate(&self) -> bool {
        match self {
            Self::Denied | Self::LoopbackOnly => true,
            Self::AllowList(endpoints) => {
                !endpoints.is_empty()
                    && endpoints
                        .iter()
                        .all(|endpoint| is_bounded_identity(endpoint))
            }
        }
    }
}

/// User and privilege state inside the environment.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserPolicyV1 {
    pub principal: String,
    pub host_user_visible: bool,
    pub privilege_escalation_denied: bool,
}

impl UserPolicyV1 {
    fn validate(&self) -> bool {
        is_bounded_identity(&self.principal) && self.privilege_escalation_denied
    }
}

/// Process containment and count limits.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessLimitsV1 {
    pub maximum_processes: u32,
    pub maximum_open_files: u32,
    pub descendant_containment: bool,
}

impl ProcessLimitsV1 {
    fn validate(&self) -> bool {
        self.maximum_processes > 0 && self.maximum_open_files > 0 && self.descendant_containment
    }
}

/// Backend-enforced resource ceilings.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceLimitsV1 {
    pub memory_bytes: u64,
    pub cpu_time_millis: u64,
    pub writable_bytes: u64,
}

impl ResourceLimitsV1 {
    fn validate(&self) -> bool {
        self.memory_bytes > 0 && self.cpu_time_millis > 0
    }
}

/// Required treatment of environment state after execution.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidualStatePolicyV1 {
    DestroyEnvironment,
    RevertToPinnedSnapshot,
}

/// Exact policy value associated with an enforcement claim.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "category", content = "realization")]
pub enum EnforcementRealizationV1 {
    Mounts(BTreeSet<MountRealizationV1>),
    Network(NetworkPolicyV1),
    User(UserPolicyV1),
    Processes(ProcessLimitsV1),
    Resources(ResourceLimitsV1),
    ResidualState(ResidualStatePolicyV1),
}

impl EnforcementRealizationV1 {
    #[must_use]
    pub fn category(&self) -> EnforcementCategoryV1 {
        match self {
            Self::Mounts(_) => EnforcementCategoryV1::Mounts,
            Self::Network(_) => EnforcementCategoryV1::Network,
            Self::User(_) => EnforcementCategoryV1::User,
            Self::Processes(_) => EnforcementCategoryV1::Processes,
            Self::Resources(_) => EnforcementCategoryV1::Resources,
            Self::ResidualState(_) => EnforcementCategoryV1::ResidualState,
        }
    }
}

/// Verification state for one concrete enforcement realization.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnforcementVerificationV1 {
    Verified,
    Unverified,
    Unsupported,
    Drifted,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnforcementClaimV1 {
    pub realization: EnforcementRealizationV1,
    pub verification: EnforcementVerificationV1,
    pub evidence: String,
}

/// Core-pinned isolation policy. `profile_hash` covers every other field.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolationProfileV1 {
    pub profile_id: String,
    pub profile_version: String,
    pub profile_hash: String,
    pub requirement: IsolationRequirementV1,
    pub backend: PinnedBackendIdentityV1,
    pub workspace_id: String,
    pub host_platform: String,
    pub mounts: BTreeSet<MountRealizationV1>,
    pub network: NetworkPolicyV1,
    pub user: UserPolicyV1,
    pub processes: ProcessLimitsV1,
    pub resources: ResourceLimitsV1,
    pub residual_state: ResidualStatePolicyV1,
}

impl IsolationProfileV1 {
    /// Recomputes the immutable profile identity after construction.
    pub fn rehash(&mut self) -> Result<(), &'static str> {
        if !self.fields_are_valid() {
            return Err("isolation profile is malformed");
        }
        let encoded = serde_json::to_vec(&(
            &self.profile_id,
            &self.profile_version,
            self.requirement,
            &self.backend,
            &self.workspace_id,
            &self.host_platform,
            &self.mounts,
            &self.network,
            &self.user,
            &self.processes,
            &self.resources,
            self.residual_state,
        ))
        .map_err(|_| "isolation profile cannot be encoded")?;
        self.profile_hash = content_hash_v1(&encoded);
        Ok(())
    }

    /// Checks structure and detects mutation after the trusted-core pin.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        if !self.fields_are_valid() || !is_content_hash(&self.profile_hash) {
            return false;
        }
        let mut copy = self.clone();
        copy.rehash().is_ok() && copy.profile_hash == self.profile_hash
    }

    #[must_use]
    pub fn expected_realizations(&self) -> Vec<EnforcementRealizationV1> {
        vec![
            EnforcementRealizationV1::Mounts(self.mounts.clone()),
            EnforcementRealizationV1::Network(self.network.clone()),
            EnforcementRealizationV1::User(self.user.clone()),
            EnforcementRealizationV1::Processes(self.processes.clone()),
            EnforcementRealizationV1::Resources(self.resources.clone()),
            EnforcementRealizationV1::ResidualState(self.residual_state),
        ]
    }

    fn fields_are_valid(&self) -> bool {
        is_bounded_identity(&self.profile_id)
            && is_bounded_identity(&self.profile_version)
            && self.backend.validate()
            && is_bounded_identity(&self.workspace_id)
            && is_bounded_identity(&self.host_platform)
            && !self.mounts.is_empty()
            && self.mounts.iter().all(MountRealizationV1::validate)
            && self.network.validate()
            && self.user.validate()
            && self.processes.validate()
            && self.resources.validate()
    }
}

/// Verification report for one prepared backend session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnforcementReportV1 {
    pub session_id: String,
    pub backend: PinnedBackendIdentityV1,
    pub profile_id: String,
    pub profile_hash: String,
    pub claims: Vec<EnforcementClaimV1>,
}

impl EnforcementReportV1 {
    /// Security-boundary strength is derived only from exact verified claims.
    #[must_use]
    pub fn is_verified_for(&self, profile: &IsolationProfileV1) -> bool {
        if !is_bounded_identity(&self.session_id)
            || self.backend != profile.backend
            || self.profile_id != profile.profile_id
            || self.profile_hash != profile.profile_hash
            || self.claims.len() != EnforcementCategoryV1::ALL.len()
        {
            return false;
        }
        let expected = profile.expected_realizations();
        let mut seen = BTreeSet::new();
        self.claims.iter().all(|claim| {
            let category = claim.realization.category();
            seen.insert(category)
                && claim.verification == EnforcementVerificationV1::Verified
                && is_bounded_evidence(&claim.evidence)
                && expected.contains(&claim.realization)
        }) && seen.len() == EnforcementCategoryV1::ALL.len()
    }
}
