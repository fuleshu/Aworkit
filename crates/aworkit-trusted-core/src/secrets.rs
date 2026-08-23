//! Core-only credential references and invocation-scoped secret leases.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use aworkit_protocol::{ProcessGeneration, StableId};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

mod native;

pub use native::{NativeCredentialStore, NativeCredentialStoreStatusV1};

const MAX_LEASE_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_LEASE_USES: u32 = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CredentialRef(pub StableId);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CredentialMetadataV1 {
    pub credential: CredentialRef,
    pub field_names: BTreeSet<String>,
    pub revision: u64,
}

/// Plaintext store payload deliberately has no serialization or `Debug` implementation.
pub struct CredentialSecretV1 {
    fields: BTreeMap<String, Zeroizing<Vec<u8>>>,
}

/// Unforgeable outside the trusted-core crate. Store reads require the broker
/// to complete lease validation before constructing this authorization.
pub struct CredentialReadAuthorizationV1 {
    _core_only: (),
}

impl CredentialSecretV1 {
    #[must_use]
    pub fn new(fields: BTreeMap<String, Vec<u8>>) -> Self {
        Self {
            fields: fields
                .into_iter()
                .map(|(name, value)| (name, Zeroizing::new(value)))
                .collect(),
        }
    }

    fn select(
        &self,
        names: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, Zeroizing<Vec<u8>>>, SecretError> {
        names
            .iter()
            .map(|name| {
                self.fields
                    .get(name)
                    .cloned()
                    .map(|value| (name.clone(), value))
                    .ok_or(SecretError::FieldDenied)
            })
            .collect()
    }
}

impl Drop for CredentialSecretV1 {
    fn drop(&mut self) {
        for value in self.fields.values_mut() {
            value.zeroize();
        }
    }
}

/// Replaceable OS-credential-store boundary. The memory adapter is a hermetic fixture.
pub trait PlatformCredentialStorePort: Send + Sync {
    fn put(
        &self,
        credential: &CredentialRef,
        secret: CredentialSecretV1,
    ) -> Result<(), SecretError>;
    fn retrieve_for_lease(
        &self,
        credential: &CredentialRef,
        authorization: &CredentialReadAuthorizationV1,
    ) -> Result<CredentialSecretV1, SecretError>;
    fn delete(&self, credential: &CredentialRef) -> Result<(), SecretError>;
}

#[derive(Clone, Default)]
pub struct MemoryCredentialStore {
    values: Arc<Mutex<BTreeMap<String, BTreeMap<String, Zeroizing<Vec<u8>>>>>>,
}

impl PlatformCredentialStorePort for MemoryCredentialStore {
    fn put(
        &self,
        credential: &CredentialRef,
        secret: CredentialSecretV1,
    ) -> Result<(), SecretError> {
        let copied = secret
            .fields
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        self.values
            .lock()
            .map_err(|_| SecretError::StoreUnavailable)?
            .insert(credential.0.as_str().to_owned(), copied);
        Ok(())
    }

    fn retrieve_for_lease(
        &self,
        credential: &CredentialRef,
        _authorization: &CredentialReadAuthorizationV1,
    ) -> Result<CredentialSecretV1, SecretError> {
        let values = self
            .values
            .lock()
            .map_err(|_| SecretError::StoreUnavailable)?;
        let fields = values
            .get(credential.0.as_str())
            .cloned()
            .ok_or(SecretError::UnknownCredential)?;
        Ok(CredentialSecretV1 { fields })
    }

    fn delete(&self, credential: &CredentialRef) -> Result<(), SecretError> {
        self.values
            .lock()
            .map_err(|_| SecretError::StoreUnavailable)?
            .remove(credential.0.as_str())
            .ok_or(SecretError::UnknownCredential)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecretLease {
    pub lease_id: StableId,
    pub credential: CredentialRef,
    pub audience_generation: ProcessGeneration,
    decision_id: Option<StableId>,
    invocation_id: Option<StableId>,
    run_id: Option<StableId>,
    permitted_fields: BTreeSet<String>,
    expires_at: Instant,
    remaining_uses: u32,
    identity_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopedLeaseRequestV1 {
    pub lease_id: StableId,
    pub credential: CredentialRef,
    pub decision_id: StableId,
    pub invocation_id: StableId,
    pub run_id: StableId,
    pub audience_generation: ProcessGeneration,
    pub permitted_fields: BTreeSet<String>,
    pub ttl: Duration,
    pub maximum_uses: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RedeemLeaseRequestV1 {
    pub lease_id: StableId,
    pub decision_id: StableId,
    pub invocation_id: StableId,
    pub audience_generation: ProcessGeneration,
    pub requested_fields: BTreeSet<String>,
}

/// Plaintext delivery is invocation-local, non-serializable, and zeroized on drop.
pub struct SecretDeliveryV1 {
    fields: BTreeMap<String, Zeroizing<Vec<u8>>>,
}

impl SecretDeliveryV1 {
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&[u8]> {
        self.fields.get(name).map(AsRef::as_ref)
    }

    #[must_use]
    pub fn into_fields(mut self) -> BTreeMap<String, Zeroizing<Vec<u8>>> {
        std::mem::take(&mut self.fields)
    }
}

impl Drop for SecretDeliveryV1 {
    fn drop(&mut self) {
        for value in self.fields.values_mut() {
            value.zeroize();
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SecretLeaseAuditKindV1 {
    Issued,
    Redeemed,
    Revoked,
    Expired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecretLeaseAuditV1 {
    pub lease_id: StableId,
    pub kind: SecretLeaseAuditKindV1,
}

pub struct SecretBroker {
    store: Arc<dyn PlatformCredentialStorePort>,
    metadata: BTreeMap<String, CredentialMetadataV1>,
    leases: BTreeMap<String, SecretLease>,
    audit: Vec<SecretLeaseAuditV1>,
}

impl Default for SecretBroker {
    fn default() -> Self {
        Self::native()
    }
}

impl SecretBroker {
    #[must_use]
    pub fn with_store(store: Arc<dyn PlatformCredentialStorePort>) -> Self {
        Self {
            store,
            metadata: BTreeMap::new(),
            leases: BTreeMap::new(),
            audit: Vec::new(),
        }
    }

    /// Constructs the production broker over the current user's native OS
    /// credential store. Store lock or initialization failures remain explicit.
    #[must_use]
    pub fn native() -> Self {
        Self::with_store(Arc::new(NativeCredentialStore::new()))
    }

    #[must_use]
    pub fn describe_credential(&self, credential: &CredentialRef) -> Option<&CredentialMetadataV1> {
        self.metadata.get(credential.0.as_str())
    }

    pub fn put_credential(
        &mut self,
        credential: CredentialRef,
        fields: BTreeMap<String, Vec<u8>>,
    ) -> Result<CredentialMetadataV1, SecretError> {
        validate_fields(fields.keys().cloned().collect())?;
        if fields.len() > 64
            || fields
                .values()
                .any(|value| value.is_empty() || value.len() > 64 * 1024)
            || fields.values().map(Vec::len).sum::<usize>() > 256 * 1024
        {
            return Err(SecretError::SecretTooLarge);
        }
        let next_revision = self
            .metadata
            .get(credential.0.as_str())
            .map_or(1, |metadata| metadata.revision.saturating_add(1));
        let field_names = fields.keys().cloned().collect();
        self.store
            .put(&credential, CredentialSecretV1::new(fields))?;
        let revoked: Vec<_> = self
            .leases
            .values()
            .filter(|lease| lease.credential == credential)
            .map(|lease| lease.lease_id.clone())
            .collect();
        for lease_id in revoked {
            self.revoke(&lease_id);
        }
        let metadata = CredentialMetadataV1 {
            credential: credential.clone(),
            field_names,
            revision: next_revision,
        };
        self.metadata
            .insert(credential.0.as_str().to_owned(), metadata.clone());
        Ok(metadata)
    }

    /// Creates an opaque random reference before placing the value in the OS
    /// credential store. Plaintext never becomes part of the returned DTO.
    pub fn create_credential(
        &mut self,
        fields: BTreeMap<String, Vec<u8>>,
    ) -> Result<CredentialMetadataV1, SecretError> {
        let mut random = [0_u8; 24];
        getrandom::fill(&mut random).map_err(|_| SecretError::StoreUnavailable)?;
        let opaque = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        random.zeroize();
        let credential = CredentialRef(
            StableId::parse(format!("credential.{opaque}"))
                .map_err(|_| SecretError::StoreUnavailable)?,
        );
        self.put_credential(credential, fields)
    }

    pub fn delete_credential(&mut self, credential: &CredentialRef) -> Result<(), SecretError> {
        self.store.delete(credential)?;
        self.metadata.remove(credential.0.as_str());
        let revoked: Vec<_> = self
            .leases
            .values()
            .filter(|lease| &lease.credential == credential)
            .map(|lease| lease.lease_id.clone())
            .collect();
        for lease_id in revoked {
            self.revoke(&lease_id);
        }
        Ok(())
    }

    /// Compatibility lease is still audience-bound, expiring, and one-use.
    pub fn issue(
        &mut self,
        lease_id: StableId,
        credential: CredentialRef,
        audience_generation: ProcessGeneration,
        ttl: Duration,
    ) -> Result<SecretLease, SecretError> {
        let expires_at = Instant::now()
            .checked_add(ttl)
            .ok_or(SecretError::Expired)?;
        if ttl.is_zero() || ttl > MAX_LEASE_TTL {
            return Err(SecretError::Expired);
        }
        let identity_hash = lease_identity(
            &lease_id,
            &credential,
            None,
            None,
            None,
            audience_generation,
            &BTreeSet::new(),
            ttl,
            1,
        );
        let lease = SecretLease {
            lease_id: lease_id.clone(),
            credential,
            audience_generation,
            decision_id: None,
            invocation_id: None,
            run_id: None,
            permitted_fields: BTreeSet::new(),
            expires_at,
            remaining_uses: 1,
            identity_hash,
        };
        self.insert_lease(lease)
    }

    pub fn issue_scoped(
        &mut self,
        request: ScopedLeaseRequestV1,
    ) -> Result<SecretLease, SecretError> {
        if request.ttl.is_zero()
            || request.ttl > MAX_LEASE_TTL
            || request.maximum_uses == 0
            || request.maximum_uses > MAX_LEASE_USES
        {
            return Err(SecretError::Expired);
        }
        validate_fields(request.permitted_fields.clone())?;
        let metadata = self
            .metadata
            .get(request.credential.0.as_str())
            .ok_or(SecretError::UnknownCredential)?;
        if !request.permitted_fields.is_subset(&metadata.field_names) {
            return Err(SecretError::FieldDenied);
        }
        let expires_at = Instant::now()
            .checked_add(request.ttl)
            .ok_or(SecretError::Expired)?;
        let identity_hash = lease_identity(
            &request.lease_id,
            &request.credential,
            Some(&request.decision_id),
            Some(&request.invocation_id),
            Some(&request.run_id),
            request.audience_generation,
            &request.permitted_fields,
            request.ttl,
            request.maximum_uses,
        );
        self.insert_lease(SecretLease {
            lease_id: request.lease_id,
            credential: request.credential,
            audience_generation: request.audience_generation,
            decision_id: Some(request.decision_id),
            invocation_id: Some(request.invocation_id),
            run_id: Some(request.run_id),
            permitted_fields: request.permitted_fields,
            expires_at,
            remaining_uses: request.maximum_uses,
            identity_hash,
        })
    }

    fn insert_lease(&mut self, lease: SecretLease) -> Result<SecretLease, SecretError> {
        if let Some(existing) = self.leases.get(lease.lease_id.as_str()) {
            return if existing.identity_hash == lease.identity_hash {
                Ok(existing.clone())
            } else {
                Err(SecretError::IdentityConflict)
            };
        }
        self.audit.push(SecretLeaseAuditV1 {
            lease_id: lease.lease_id.clone(),
            kind: SecretLeaseAuditKindV1::Issued,
        });
        self.leases
            .insert(lease.lease_id.as_str().to_owned(), lease.clone());
        Ok(lease)
    }

    pub fn redeem(
        &mut self,
        lease_id: &StableId,
        generation: ProcessGeneration,
    ) -> Result<CredentialRef, SecretError> {
        let lease = self.validate_lease(lease_id, generation, None, None, &BTreeSet::new())?;
        Ok(lease.credential.clone())
    }

    pub fn redeem_scoped(
        &mut self,
        request: &RedeemLeaseRequestV1,
    ) -> Result<SecretDeliveryV1, SecretError> {
        let lease = self.validate_lease(
            &request.lease_id,
            request.audience_generation,
            Some(&request.decision_id),
            Some(&request.invocation_id),
            &request.requested_fields,
        )?;
        let secret = self.store.retrieve_for_lease(
            &lease.credential,
            &CredentialReadAuthorizationV1 { _core_only: () },
        )?;
        Ok(SecretDeliveryV1 {
            fields: secret.select(&request.requested_fields)?,
        })
    }

    fn validate_lease(
        &mut self,
        lease_id: &StableId,
        generation: ProcessGeneration,
        decision_id: Option<&StableId>,
        invocation_id: Option<&StableId>,
        fields: &BTreeSet<String>,
    ) -> Result<SecretLease, SecretError> {
        let lease = self
            .leases
            .get_mut(lease_id.as_str())
            .ok_or(SecretError::Unknown)?;
        if Instant::now() >= lease.expires_at {
            self.audit.push(SecretLeaseAuditV1 {
                lease_id: lease_id.clone(),
                kind: SecretLeaseAuditKindV1::Expired,
            });
            return Err(SecretError::Expired);
        }
        if lease.remaining_uses == 0 {
            return Err(SecretError::Used);
        }
        if lease.audience_generation != generation {
            return Err(SecretError::Audience);
        }
        if lease.decision_id.as_ref() != decision_id
            || lease.invocation_id.as_ref() != invocation_id
        {
            return Err(SecretError::InvocationMismatch);
        }
        if !fields.is_subset(&lease.permitted_fields) {
            return Err(SecretError::FieldDenied);
        }
        lease.remaining_uses -= 1;
        self.audit.push(SecretLeaseAuditV1 {
            lease_id: lease_id.clone(),
            kind: SecretLeaseAuditKindV1::Redeemed,
        });
        Ok(lease.clone())
    }

    pub fn revoke(&mut self, lease_id: &StableId) {
        if self.leases.remove(lease_id.as_str()).is_some() {
            self.audit.push(SecretLeaseAuditV1 {
                lease_id: lease_id.clone(),
                kind: SecretLeaseAuditKindV1::Revoked,
            });
        }
    }

    pub fn revoke_generation(&mut self, generation: ProcessGeneration) {
        let ids: Vec<_> = self
            .leases
            .values()
            .filter(|lease| lease.audience_generation == generation)
            .map(|lease| lease.lease_id.clone())
            .collect();
        for id in ids {
            self.revoke(&id);
        }
    }

    pub fn revoke_run(&mut self, run_id: &StableId) {
        let ids: Vec<_> = self
            .leases
            .values()
            .filter(|lease| lease.run_id.as_ref() == Some(run_id))
            .map(|lease| lease.lease_id.clone())
            .collect();
        for id in ids {
            self.revoke(&id);
        }
    }

    #[must_use]
    pub fn audit(&self) -> &[SecretLeaseAuditV1] {
        &self.audit
    }
}

fn validate_fields(fields: BTreeSet<String>) -> Result<(), SecretError> {
    if fields.is_empty()
        || fields.iter().any(|field| {
            field.is_empty()
                || field.len() > 128
                || !field
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        })
    {
        Err(SecretError::FieldDenied)
    } else {
        Ok(())
    }
}

fn lease_identity(
    lease_id: &StableId,
    credential: &CredentialRef,
    decision_id: Option<&StableId>,
    invocation_id: Option<&StableId>,
    run_id: Option<&StableId>,
    generation: ProcessGeneration,
    fields: &BTreeSet<String>,
    ttl: Duration,
    maximum_uses: u32,
) -> String {
    let bytes = serde_json::to_vec(&(
        lease_id.as_str(),
        credential.0.as_str(),
        decision_id.map(StableId::as_str),
        invocation_id.map(StableId::as_str),
        run_id.map(StableId::as_str),
        generation.0,
        fields,
        ttl.as_nanos(),
        maximum_uses,
    ))
    .expect("stable secret lease identity is serializable");
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum SecretError {
    #[error("unknown lease")]
    Unknown,
    #[error("unknown credential reference")]
    UnknownCredential,
    #[error("lease already redeemed")]
    Used,
    #[error("lease audience mismatch")]
    Audience,
    #[error("lease invocation or decision mismatch")]
    InvocationMismatch,
    #[error("secret field was not permitted")]
    FieldDenied,
    #[error("lease expired")]
    Expired,
    #[error("lease ID was reused with different content")]
    IdentityConflict,
    #[error("credential store is unavailable")]
    StoreUnavailable,
    #[error("credential store is locked or user interaction was denied")]
    StoreLocked,
    #[error("credential store access-control guarantees could not be validated")]
    StoreAccessControlInvalid,
    #[error("credential secret material exceeds its invocation-safe bound")]
    SecretTooLarge,
}
