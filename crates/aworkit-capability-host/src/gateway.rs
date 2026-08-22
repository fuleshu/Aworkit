//! Authenticated, generation-fenced approved-invocation admission.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
};

use aworkit_protocol::{ExtensionRuntimeBindingV1, ProcessGeneration, SchemaVersion, StableId};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::{
    CancellationToken, CapabilityDescriptor, CapabilityKind, FrozenAdapterRegistry,
    registry::RegistryError,
};

type HmacSha256 = Hmac<Sha256>;

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
    /// Present only for an extension contribution and exact-matched against
    /// the immutable core-attested registry provenance.
    pub extension: Option<ExtensionRuntimeBindingV1>,
    /// Exact isolation profile pinned into the Run. `None` is valid only when
    /// the descriptor also requires no isolation profile.
    pub required_isolation_profile: Option<String>,
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

/// Generation-fenced control operations accepted on the gateway's reserved
/// capacity path.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostControlKindV1 {
    Cancel,
}

/// Core-authenticated control for one already-admitted invocation. The
/// invocation-scoped cancellation token prevents a valid control for another
/// invocation from being retargeted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostControlEnvelopeV1 {
    pub schema_version: SchemaVersion,
    pub control_id: StableId,
    pub invocation_id: StableId,
    pub host_generation: ProcessGeneration,
    pub cancellation_token: StableId,
    pub kind: HostControlKindV1,
    pub core_authentication_tag: String,
}

impl HostControlEnvelopeV1 {
    /// Computes the authenticated tag over every control-relevant field.
    pub fn sign(&mut self, key: &[u8]) -> Result<(), HostError> {
        self.core_authentication_tag = control_authentication_tag(self, key)?;
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

/// Adapter-routing seam that is invoked only after the sole gateway has
/// authenticated and exact-matched an approved envelope.
pub trait AdmittedInvocationDispatcherV1 {
    type Output;

    fn dispatch(
        &self,
        envelope: &ApprovedInvocationEnvelopeV1,
        admission: &AdmissionReceipt,
        cancellation: &CancellationToken,
    ) -> Self::Output;

    /// Long-lived session starts remain admitted so authenticated cancellation
    /// and later terminal settlement can still address them. One-shot adapters
    /// use the terminal default.
    fn lifecycle(&self, _output: &Self::Output) -> DispatchLifecycleV1 {
        DispatchLifecycleV1::Terminal
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchLifecycleV1 {
    Active,
    Terminal,
}

/// Result of one gateway dispatch attempt. Duplicate active or completed
/// requests retain their admission receipt and never call the dispatcher.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostDispatchReceiptV1<T> {
    pub admission: AdmissionReceipt,
    pub output: Option<T>,
    pub lifecycle: Option<DispatchLifecycleV1>,
}

#[derive(Clone, Debug)]
struct AdmissionState {
    request_hash: String,
    cancellation_token: StableId,
    cancellation: CancellationToken,
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
    /// Constructs the production gateway only from a registry materialized
    /// from a validated core-attested set. Even a built-in-only generation must
    /// carry an explicit (possibly empty) attested set and its set hash.
    pub fn from_attested_registry(
        registry: FrozenAdapterRegistry,
        core_key: Vec<u8>,
        maximum_active: usize,
    ) -> Result<Self, HostError> {
        if registry.attested_set_hash().is_none() {
            return Err(HostError::UnattestedRegistry);
        }
        if core_key.is_empty() {
            return Err(HostError::InvalidCoreKey);
        }
        Ok(Self {
            generation: registry.generation(),
            registry,
            core_key: Zeroizing::new(core_key),
            maximum_active: maximum_active.max(1),
            admissions: Mutex::new(BTreeMap::new()),
            completed: Mutex::new(BTreeMap::new()),
        })
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
        let descriptor = self.registry.resolve_for_admission(
            &envelope.capability_id,
            &envelope.adapter_version,
            &envelope.binding_hash,
            envelope.extension.as_ref(),
            envelope.required_isolation_profile.as_deref(),
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
                cancellation_token: envelope.cancellation_token.clone(),
                cancellation: CancellationToken::default(),
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

    /// Authenticates, exact-matches, and dispatches through one production
    /// entry point. The dispatcher is never called for stale, unattested,
    /// malformed, active-duplicate, or completed-duplicate envelopes.
    pub fn dispatch_v1<D: AdmittedInvocationDispatcherV1>(
        &self,
        envelope: &ApprovedInvocationEnvelopeV1,
        now_epoch_millis: u64,
        dispatcher: &D,
    ) -> Result<HostDispatchReceiptV1<D::Output>, HostError> {
        let admission = self.admit_v1(envelope, now_epoch_millis)?;
        if !admission.should_execute() {
            return Ok(HostDispatchReceiptV1 {
                admission,
                output: None,
                lifecycle: None,
            });
        }
        let cancellation = self.cancellation_handle(&envelope.invocation_id)?;
        let output = dispatcher.dispatch(envelope, &admission, &cancellation);
        let lifecycle = dispatcher.lifecycle(&output);
        if lifecycle == DispatchLifecycleV1::Terminal {
            self.complete(&envelope.invocation_id)?;
        }
        Ok(HostDispatchReceiptV1 {
            admission,
            output: Some(output),
            lifecycle: Some(lifecycle),
        })
    }

    /// Authenticated reserved control-path cancellation remains available while
    /// normal admissions are full.
    pub fn apply_control_v1(&self, control: &HostControlEnvelopeV1) -> Result<(), HostError> {
        if control.schema_version != SchemaVersion::V1 {
            return Err(HostError::UnsupportedSchema);
        }
        if control.host_generation != self.generation {
            return Err(HostError::StaleGeneration);
        }
        verify_control_authentication(control, &self.core_key)?;
        let mut admissions = self.admissions.lock().map_err(|_| HostError::Poisoned)?;
        let state = admissions
            .get_mut(control.invocation_id.as_str())
            .ok_or(HostError::UnknownInvocation)?;
        if state.cancellation_token != control.cancellation_token {
            return Err(HostError::CancellationTokenMismatch);
        }
        match control.kind {
            HostControlKindV1::Cancel => state.cancellation.cancel(),
        }
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
                    .map(|value| value.cancellation.is_cancelled())
            })
            .unwrap_or(true)
    }

    fn cancellation_handle(
        &self,
        invocation_id: &StableId,
    ) -> Result<CancellationToken, HostError> {
        self.admissions
            .lock()
            .map_err(|_| HostError::Poisoned)?
            .get(invocation_id.as_str())
            .map(|state| state.cancellation.clone())
            .ok_or(HostError::UnknownInvocation)
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InvocationAuthenticationView<'a> {
    domain: &'static str,
    schema_version: SchemaVersion,
    invocation_id: &'a StableId,
    decision_id: &'a StableId,
    host_generation: ProcessGeneration,
    capability_id: &'a str,
    adapter_version: &'a str,
    binding_hash: &'a str,
    extension: &'a Option<ExtensionRuntimeBindingV1>,
    required_isolation_profile: &'a Option<String>,
    kind: CapabilityKind,
    enforced_scopes: &'a [String],
    deadline_epoch_millis: u64,
    cancellation_token: &'a StableId,
    lease_handles: &'a [StableId],
    max_output_bytes: usize,
    payload: &'a Value,
}

fn authentication_bytes(envelope: &ApprovedInvocationEnvelopeV1) -> Result<Vec<u8>, HostError> {
    Ok(serde_json::to_vec(&InvocationAuthenticationView {
        domain: "aworkit.approved-invocation.v1",
        schema_version: envelope.schema_version,
        invocation_id: &envelope.invocation_id,
        decision_id: &envelope.decision_id,
        host_generation: envelope.host_generation,
        capability_id: &envelope.capability_id,
        adapter_version: &envelope.adapter_version,
        binding_hash: &envelope.binding_hash,
        extension: &envelope.extension,
        required_isolation_profile: &envelope.required_isolation_profile,
        kind: envelope.kind,
        enforced_scopes: &envelope.enforced_scopes,
        deadline_epoch_millis: envelope.deadline_epoch_millis,
        cancellation_token: &envelope.cancellation_token,
        lease_handles: &envelope.lease_handles,
        max_output_bytes: envelope.max_output_bytes,
        payload: &envelope.payload,
    })?)
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

fn control_authentication_bytes(control: &HostControlEnvelopeV1) -> Result<Vec<u8>, HostError> {
    Ok(serde_json::to_vec(&(
        "aworkit.host-control.v1",
        control.schema_version,
        &control.control_id,
        &control.invocation_id,
        control.host_generation,
        &control.cancellation_token,
        control.kind,
    ))?)
}

fn control_authentication_tag(
    control: &HostControlEnvelopeV1,
    key: &[u8],
) -> Result<String, HostError> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|_| HostError::Authentication)?;
    mac.update(&control_authentication_bytes(control)?);
    Ok(format!("hmac-sha256:{:x}", mac.finalize().into_bytes()))
}

fn verify_control_authentication(
    control: &HostControlEnvelopeV1,
    key: &[u8],
) -> Result<(), HostError> {
    let supplied = control
        .core_authentication_tag
        .strip_prefix("hmac-sha256:")
        .ok_or(HostError::Authentication)?;
    let supplied = decode_hex(supplied).ok_or(HostError::Authentication)?;
    let mut mac = HmacSha256::new_from_slice(key).map_err(|_| HostError::Authentication)?;
    mac.update(&control_authentication_bytes(control)?);
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
    #[error("capability host requires a core-attested frozen registry")]
    UnattestedRegistry,
    #[error("capability host requires a non-empty core authentication key")]
    InvalidCoreKey,
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
    #[error("host control cancellation token does not match the admitted invocation")]
    CancellationTokenMismatch,
    #[error("host admission state is unavailable")]
    Poisoned,
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
