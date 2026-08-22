//! Immutable repository, session, and branch metadata.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{CanonicalCodec, ChildContinuationPlanV1, ExportPolicy, PortableError, PortablePaths};

pub const PORTABLE_FAMILY: &str = "aworkit-portable-session";
pub const PORTABLE_MAJOR: u16 = 1;
pub const PORTABLE_MINOR: u16 = 0;

/// Version negotiated before a portable repository is exposed for writing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepositoryManifest {
    pub family: String,
    pub major: u16,
    pub minor: u16,
    pub required_features: Vec<String>,
}

/// A persistent Chat/Run identity and its scrubbed frozen snapshot identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionManifest {
    pub session_id: String,
    pub chat_id: String,
    pub run_id: String,
    pub frozen_snapshot_hash: String,
    pub canonical_branch_id: String,
    pub export_policy_hash: String,
}

/// Immutable lineage; concurrent successors are branches, never time-selected tips.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchManifest {
    pub branch_id: String,
    pub session_id: String,
    pub parent_branch_id: Option<String>,
    pub parent_checkpoint_hash: Option<String>,
    pub parent_head_hash: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildContinuationManifestRequestV1 {
    pub plan: ChildContinuationPlanV1,
    pub child_session_id: String,
    pub child_run_id: String,
    pub fresh_frozen_snapshot_hash: String,
    pub verified_parent_checkpoint_hash: Option<String>,
    pub verified_parent_head_hash: String,
    pub user_confirmed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChildContinuationManifestReceiptV1 {
    pub session_manifest_hash: String,
    pub branch_manifest_hash: String,
    pub session: SessionManifest,
    pub branch: BranchManifest,
}

/// The sole mutable branch pointer, guarded by its expected generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchRef {
    pub branch_id: String,
    pub head_segment_hash: Option<String>,
    pub checkpoint_hash: Option<String>,
    pub next_ordinal: u64,
    pub generation: u64,
    pub commit_id: Option<String>,
    #[serde(default)]
    pub commit_request_hash: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "manifest", rename_all = "snake_case")]
pub enum ManifestEnvelopeV1 {
    Repository(RepositoryManifest),
    Session(SessionManifest),
    Branch(BranchManifest),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RepositoryCompatibility {
    ReadWrite,
    ReadOnlyNewerMinor,
    UnsupportedFamily,
    UnsupportedMajor,
    MissingFeatures(Vec<String>),
}

impl RepositoryCompatibility {
    #[must_use]
    pub fn writable(&self) -> bool {
        matches!(self, Self::ReadWrite)
    }
}

impl RepositoryManifest {
    pub fn compatibility(&self, supported_features: &BTreeSet<String>) -> RepositoryCompatibility {
        if self.family != PORTABLE_FAMILY {
            return RepositoryCompatibility::UnsupportedFamily;
        }
        if self.major != PORTABLE_MAJOR {
            return RepositoryCompatibility::UnsupportedMajor;
        }
        let missing: Vec<_> = self
            .required_features
            .iter()
            .filter(|feature| !supported_features.contains(*feature))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return RepositoryCompatibility::MissingFeatures(missing);
        }
        if self.minor > PORTABLE_MINOR {
            RepositoryCompatibility::ReadOnlyNewerMinor
        } else {
            RepositoryCompatibility::ReadWrite
        }
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self
            .required_features
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
            || self
                .required_features
                .iter()
                .any(|feature| !valid_name(feature))
        {
            return Err(ManifestError::Malformed);
        }
        Ok(())
    }
}

impl SessionManifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        for value in [
            &self.session_id,
            &self.chat_id,
            &self.run_id,
            &self.canonical_branch_id,
        ] {
            if !valid_name(value) {
                return Err(ManifestError::Malformed);
            }
        }
        if !valid_hash(&self.frozen_snapshot_hash)
            || self.export_policy_hash != ExportPolicy.policy_hash()
        {
            return Err(ManifestError::Malformed);
        }
        Ok(())
    }
}

impl BranchManifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        if !valid_name(&self.branch_id)
            || !valid_name(&self.session_id)
            || self
                .parent_branch_id
                .as_deref()
                .is_some_and(|value| !valid_name(value))
            || self
                .parent_checkpoint_hash
                .as_deref()
                .is_some_and(|value| !valid_hash(value))
            || self
                .parent_head_hash
                .as_deref()
                .is_some_and(|value| !valid_hash(value))
        {
            return Err(ManifestError::Malformed);
        }
        if self.parent_branch_id.as_deref() == Some(&self.branch_id) {
            return Err(ManifestError::SelfParent);
        }
        Ok(())
    }
}

/// Content-addressed manifest catalog. Identity collisions are detected by the
/// immutable repository rather than silently replacing metadata.
#[derive(Clone, Debug)]
pub struct ManifestCatalog {
    paths: PortablePaths,
    codec: CanonicalCodec,
}

impl ManifestCatalog {
    #[must_use]
    pub fn new(paths: PortablePaths) -> Self {
        Self {
            paths,
            codec: CanonicalCodec,
        }
    }

    pub fn publish(&self, manifest: ManifestEnvelopeV1) -> Result<String, ManifestError> {
        match &manifest {
            ManifestEnvelopeV1::Repository(value) => value.validate()?,
            ManifestEnvelopeV1::Session(value) => value.validate()?,
            ManifestEnvelopeV1::Branch(value) => value.validate()?,
        }
        let bytes = self.codec.encode(&manifest)?;
        Ok(self.paths.publish("manifests", &bytes)?)
    }

    pub fn read(&self, identity: &str) -> Result<ManifestEnvelopeV1, ManifestError> {
        let bytes = self.paths.read("manifests", identity)?;
        let manifest: ManifestEnvelopeV1 = self.codec.decode(&bytes)?;
        match &manifest {
            ManifestEnvelopeV1::Repository(value) => value.validate()?,
            ManifestEnvelopeV1::Session(value) => value.validate()?,
            ManifestEnvelopeV1::Branch(value) => value.validate()?,
        }
        Ok(manifest)
    }

    /// Creates only immutable child session/branch manifests after explicit
    /// local rebind resolution and user confirmation. Authority, approvals,
    /// secret handles, and imported runtime state have no fields in this flow.
    pub fn publish_child_continuation(
        &self,
        request: &ChildContinuationManifestRequestV1,
    ) -> Result<ChildContinuationManifestReceiptV1, ManifestError> {
        if !request.user_confirmed
            || !request.plan.can_create_after_user_confirmation
            || !request.plan.fresh_snapshot_required
            || !request.plan.fresh_authority_required
            || !request.plan.fresh_approvals_required
            || request.plan.imported_runtime_resumable
            || !valid_name(&request.child_session_id)
            || !valid_name(&request.child_run_id)
            || !valid_hash(&request.fresh_frozen_snapshot_hash)
            || !valid_hash(&request.verified_parent_head_hash)
            || request
                .verified_parent_checkpoint_hash
                .as_deref()
                .is_some_and(|value| !valid_hash(value))
        {
            return Err(ManifestError::ContinuationNotConfirmed);
        }
        let session = SessionManifest {
            session_id: request.child_session_id.clone(),
            chat_id: request.plan.child_chat_id.clone(),
            run_id: request.child_run_id.clone(),
            frozen_snapshot_hash: request.fresh_frozen_snapshot_hash.clone(),
            canonical_branch_id: request.plan.child_branch_id.clone(),
            export_policy_hash: ExportPolicy.policy_hash().into(),
        };
        let branch = BranchManifest {
            branch_id: request.plan.child_branch_id.clone(),
            session_id: request.child_session_id.clone(),
            parent_branch_id: Some(request.plan.parent_branch_id.clone()),
            parent_checkpoint_hash: request.verified_parent_checkpoint_hash.clone(),
            parent_head_hash: Some(request.verified_parent_head_hash.clone()),
        };
        session.validate()?;
        branch.validate()?;
        let session_manifest_hash = self.publish(ManifestEnvelopeV1::Session(session.clone()))?;
        let branch_manifest_hash = self.publish(ManifestEnvelopeV1::Branch(branch.clone()))?;
        Ok(ChildContinuationManifestReceiptV1 {
            session_manifest_hash,
            branch_manifest_hash,
            session,
            branch,
        })
    }
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn valid_hash(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("portable manifest is malformed")]
    Malformed,
    #[error("portable branch cannot name itself as parent")]
    SelfParent,
    #[error(
        "child continuation requires fresh identities, bindings, snapshot, authority, approvals, and explicit confirmation"
    )]
    ContinuationNotConfirmed,
    #[error(transparent)]
    Portable(#[from] PortableError),
    #[error(transparent)]
    Codec(#[from] crate::CodecError),
}
