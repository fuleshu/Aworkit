//! Authenticated, generation-fenced approved-invocation admission.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
};

use aworkit_protocol::{ProcessGeneration, SchemaVersion, StableId};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::{
    AdapterRegistry, CapabilityDescriptor, CapabilityKind, FrozenAdapterRegistry,
    registry::RegistryError,
};

type HmacSha256 = Hmac<Sha256>;
const LEGACY_CORE_KEY: &[u8] = b"aworkit-hermetic-core-host-key-v1";

/// Compatibility envelope retained for early in-process callers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovedInvocation {
    pub invocation_id: StableId,
    pub host_generation: ProcessGeneration,
    pub capability_id: String,
    pub adapter_version: String,
    pub kind: CapabilityKind,
    pub payload: Value,
}

/// Fully bound core-issued envelope admitted by the production gateway.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovedInvocationEnvelopeV1 {
    pub schema_version: SchemaVersion,
    pub invocation_id: StableId,
    pub decision_id: StableId,
    pub host_generation: ProcessGeneration,
    pub capability_id: String,
    pub adapter_version: String,
    pub binding_hash: String,
    pub kind: CapabilityKind,
    pub enforced_scopes: Vec<String>,
    pub deadline_epoch_millis: u64,
    pub cancellation_token: StableId,
    pub lease_handles: Vec<StableId>,
    pub max_output_bytes: usize,
    pub payload: Value,
    pub core_authentication_tag: String,
}

impl ApprovedInvocationEnvelopeV1 {
    /// Computes the authenticated tag over every execution-relevant field.
    pub fn sign(&mut self, key: &[u8]) -> Result<(), HostError> {
        self.core_authentication_tag = authentication_tag(self, key)?;
        Ok(())
    }
}

/// Admission result gives the executor the exact frozen descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmissionReceipt {
    pub invocation_id: StableId,
    pub request_hash: String,
    pub duplicate: bool,
    pub disposition: AdmissionDispositionV1,
    pub descriptor: CapabilityDescriptor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionDispositionV1 {
    Execute,
    AlreadyActive,
    AlreadyCompleted,
}

impl AdmissionReceipt {
    #[must_use]
    pub fn should_execute(&self) -> bool {
        self.disposition == AdmissionDispositionV1::Execute
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InvocationResult {
    pub invocation_id: StableId,
    pub succeeded: bool,
    pub side_effect_known_safe: bool,
    pub payload: Value,
}

#[derive(Clone, Debug)]
struct AdmissionState {
    request_hash: String,
    cancelled: bool,
}

/// Sole core-facing admission gate for one immutable host generation.
pub struct CapabilityHost {
    generation: ProcessGeneration,
    registry: FrozenAdapterRegistry,
    core_key: Zeroizing<Vec<u8>>,
    maximum_active: usize,
    admissions: Mutex<BTreeMap<String, AdmissionState>>,
    completed: Mutex<BTreeMap<String, String>>,
}

impl CapabilityHost {
    #[must_use]
    pub fn new(generation: ProcessGeneration, registry: AdapterRegistry) -> Self {
        Self::new_authenticated(generation, registry, LEGACY_CORE_KEY.to_vec(), 128)
    }

    #[must_use]
    pub fn new_authenticated(
        generation: ProcessGeneration,
        registry: AdapterRegistry,
        core_key: Vec<u8>,
        maximum_active: usize,
    ) -> Self {
        Self {
            generation,
            registry: registry.freeze(generation),
            core_key: Zeroizing::new(core_key),
            maximum_active: maximum_active.max(1),
            admissions: Mutex::new(BTreeMap::new()),
            completed: Mutex::new(BTreeMap::new()),
        }
    }

    /// Compatibility admission still enforces generation, exact version, kind, and input size.
    pub fn admit(&self, envelope: &ApprovedInvocation) -> Result<(), HostError> {
        if envelope.host_generation != self.generation {
            return Err(HostError::StaleGeneration);
        }
        let descriptor = self
            .registry
            .resolve_version(&envelope.capability_id, &envelope.adapter_version)?;
        if descriptor.kind != envelope.kind {
            return Err(HostError::KindMismatch);
        }
        let encoded = serde_json::to_vec(&envelope.payload)?;
        if encoded.len() > descriptor.max_input_bytes {
            return Err(HostError::PayloadTooLarge);
        }
        Ok(())
    }

    /// Validates all authority, generation, deadline, size, and deduplication fences.
    pub fn admit_v1(
        &self,
        envelope: &ApprovedInvocationEnvelopeV1,
        now_epoch_millis: u64,
    ) -> Result<AdmissionReceipt, HostError> {
        if envelope.schema_version != SchemaVersion::V1 {
            return Err(HostError::UnsupportedSchema);
        }
        if envelope.host_generation != self.generation
            || self.registry.generation() != self.generation
        {
            return Err(HostError::StaleGeneration);
        }
        verify_authentication(envelope, &self.core_key)?;
        if envelope.deadline_epoch_millis <= now_epoch_millis {
            return Err(HostError::DeadlineElapsed);
        }
        let descriptor = self.registry.resolve_exact(
            &envelope.capability_id,
            &envelope.adapter_version,
            &envelope.binding_hash,
        )?;
        if descriptor.kind != envelope.kind {
            return Err(HostError::KindMismatch);
        }
        if envelope.max_output_bytes == 0 || envelope.max_output_bytes > descriptor.max_output_bytes
        {
            return Err(HostError::OutputLimitBroadened);
        }
        if !strictly_sorted_unique(&envelope.enforced_scopes)
            || !envelope
                .lease_handles
                .windows(2)
                .all(|pair| pair[0].as_str() < pair[1].as_str())
        {
            return Err(HostError::NonCanonicalAuthority);
        }
        let requested: BTreeSet<_> = envelope.enforced_scopes.iter().collect();
        let permitted: BTreeSet<_> = descriptor.allowed_scopes.iter().collect();
        if !requested.is_subset(&permitted) {
            return Err(HostError::ScopeBroadened);
        }
        if envelope.lease_handles.len() > descriptor.secret_slots.len() {
            return Err(HostError::LeaseCountBroadened);
        }
        let encoded = serde_json::to_vec(&envelope.payload)?;
        if encoded.len() > descriptor.max_input_bytes {
            return Err(HostError::PayloadTooLarge);
        }
        let request_hash = request_hash(envelope)?;
        if let Some(existing) = self
            .completed
            .lock()
            .map_err(|_| HostError::Poisoned)?
            .get(envelope.invocation_id.as_str())
        {
            if existing != &request_hash {
                return Err(HostError::InvocationIdentityConflict);
            }
            return Ok(AdmissionReceipt {
                invocation_id: envelope.invocation_id.clone(),
                request_hash,
                duplicate: true,
                disposition: AdmissionDispositionV1::AlreadyCompleted,
                descriptor: descriptor.clone(),
            });
        }
        let mut admissions = self.admissions.lock().map_err(|_| HostError::Poisoned)?;
        if let Some(existing) = admissions.get(envelope.invocation_id.as_str()) {
            if existing.request_hash != request_hash {
                return Err(HostError::InvocationIdentityConflict);
            }
            return Ok(AdmissionReceipt {
                invocation_id: envelope.invocation_id.clone(),
                request_hash,
                duplicate: true,
                disposition: AdmissionDispositionV1::AlreadyActive,
                descriptor: descriptor.clone(),
            });
        }
        if admissions.len() >= self.maximum_active {
            return Err(HostError::Backpressure);
        }
        admissions.insert(
            envelope.invocation_id.as_str().to_owned(),
            AdmissionState {
                request_hash: request_hash.clone(),
                cancelled: false,
            },
        );
        Ok(AdmissionReceipt {
            invocation_id: envelope.invocation_id.clone(),
            request_hash,
            duplicate: false,
            disposition: AdmissionDispositionV1::Execute,
            descriptor: descriptor.clone(),
        })
    }

    /// Reserved control-path cancellation remains available while admissions are full.
    pub fn cancel(&self, invocation_id: &StableId) -> Result<(), HostError> {
        let mut admissions = self.admissions.lock().map_err(|_| HostError::Poisoned)?;
        let state = admissions
            .get_mut(invocation_id.as_str())
            .ok_or(HostError::UnknownInvocation)?;
        state.cancelled = true;
        Ok(())
    }

    #[must_use]
    pub fn is_cancelled(&self, invocation_id: &StableId) -> bool {
        self.admissions
            .lock()
            .ok()
            .and_then(|values| {
                values
                    .get(invocation_id.as_str())
                    .map(|value| value.cancelled)
            })
            .unwrap_or(true)
    }

    pub fn complete(&self, invocation_id: &StableId) -> Result<(), HostError> {
        let state = self
            .admissions
            .lock()
            .map_err(|_| HostError::Poisoned)?
            .remove(invocation_id.as_str())
            .ok_or(HostError::UnknownInvocation)?;
        self.completed
            .lock()
            .map_err(|_| HostError::Poisoned)?
            .insert(invocation_id.as_str().to_owned(), state.request_hash);
        Ok(())
    }
}

fn strictly_sorted_unique<T: Ord>(values: &[T]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

fn authentication_bytes(envelope: &ApprovedInvocationEnvelopeV1) -> Result<Vec<u8>, HostError> {
    Ok(serde_json::to_vec(&(
        envelope.schema_version,
        &envelope.invocation_id,
        &envelope.decision_id,
        envelope.host_generation,
        &envelope.capability_id,
        &envelope.adapter_version,
        &envelope.binding_hash,
        envelope.kind,
        &envelope.enforced_scopes,
        envelope.deadline_epoch_millis,
        &envelope.cancellation_token,
        &envelope.lease_handles,
        envelope.max_output_bytes,
        &envelope.payload,
    ))?)
}

fn authentication_tag(
    envelope: &ApprovedInvocationEnvelopeV1,
    key: &[u8],
) -> Result<String, HostError> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|_| HostError::Authentication)?;
    mac.update(&authentication_bytes(envelope)?);
    Ok(format!("hmac-sha256:{:x}", mac.finalize().into_bytes()))
}

fn verify_authentication(
    envelope: &ApprovedInvocationEnvelopeV1,
    key: &[u8],
) -> Result<(), HostError> {
    let supplied = envelope
        .core_authentication_tag
        .strip_prefix("hmac-sha256:")
        .ok_or(HostError::Authentication)?;
    let supplied = decode_hex(supplied).ok_or(HostError::Authentication)?;
    let mut mac = HmacSha256::new_from_slice(key).map_err(|_| HostError::Authentication)?;
    mac.update(&authentication_bytes(envelope)?);
    mac.verify_slice(&supplied)
        .map_err(|_| HostError::Authentication)
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16)?;
            let low = (pair[1] as char).to_digit(16)?;
            Some(((high << 4) | low) as u8)
        })
        .collect()
}

fn request_hash(envelope: &ApprovedInvocationEnvelopeV1) -> Result<String, HostError> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(authentication_bytes(envelope)?)
    ))
}

#[derive(Debug, Error)]
pub enum HostError {
    #[error("unsupported invocation schema")]
    UnsupportedSchema,
    #[error("stale host generation")]
    StaleGeneration,
    #[error("core authentication failed")]
    Authentication,
    #[error("invocation deadline elapsed")]
    DeadlineElapsed,
    #[error("adapter kind differs from its frozen descriptor")]
    KindMismatch,
    #[error("invocation scope broadens the frozen descriptor")]
    ScopeBroadened,
    #[error("invocation output limit broadens the frozen descriptor")]
    OutputLimitBroadened,
    #[error("invocation contains too many secret leases")]
    LeaseCountBroadened,
    #[error("invocation authority lists must be sorted and duplicate-free")]
    NonCanonicalAuthority,
    #[error("invocation payload exceeds the descriptor bound")]
    PayloadTooLarge,
    #[error("the same invocation ID was reused with different content")]
    InvocationIdentityConflict,
    #[error("host admission queue is full")]
    Backpressure,
    #[error("unknown active invocation")]
    UnknownInvocation,
    #[error("host admission state is unavailable")]
    Poisoned,
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
