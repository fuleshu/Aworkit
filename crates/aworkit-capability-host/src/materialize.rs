//! Invocation-scoped secret lease redemption and least-field materialization.

use std::collections::{BTreeMap, BTreeSet};

use aworkit_protocol::{ProcessGeneration, StableId};
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

use crate::Redactor;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecretLeaseHandleV1 {
    pub lease_id: StableId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RedeemLeaseRequestV1 {
    pub lease_id: StableId,
    pub decision_id: StableId,
    pub invocation_id: StableId,
    pub host_generation: ProcessGeneration,
    pub requested_fields: BTreeSet<String>,
}

/// Plaintext deliberately cannot be serialized or formatted with `Debug`.
pub struct SecretDeliveryV1 {
    pub fields: BTreeMap<String, Zeroizing<Vec<u8>>>,
}

impl SecretDeliveryV1 {
    fn into_fields(mut self) -> BTreeMap<String, Zeroizing<Vec<u8>>> {
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

/// Authenticated core channel used by the host; implementations must not persist delivery.
pub trait SecretLeaseClientV1: Send + Sync {
    fn redeem(
        &self,
        request: &RedeemLeaseRequestV1,
    ) -> Result<SecretDeliveryV1, SecretMaterializationError>;

    fn revoke(&self, lease_id: &StableId) -> Result<(), SecretMaterializationError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InjectionTargetV1 {
    Header(String),
    Environment(String),
    Stdin,
    ProtectedFile,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecretFieldPlanV1 {
    pub field: String,
    pub target: InjectionTargetV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecretMaterializationPlanV1 {
    pub decision_id: StableId,
    pub invocation_id: StableId,
    pub host_generation: ProcessGeneration,
    pub lease: SecretLeaseHandleV1,
    pub fields: Vec<SecretFieldPlanV1>,
}

/// Materialized values remain in memory and are zeroized when the invocation ends.
pub struct SecretMaterializationV1 {
    values: BTreeMap<String, Zeroizing<Vec<u8>>>,
    targets: BTreeMap<String, InjectionTargetV1>,
    redactor: Redactor,
}

impl SecretMaterializationV1 {
    /// Returns the approved field names without exposing their materialized values.
    pub fn field_names(&self) -> impl ExactSizeIterator<Item = &str> {
        self.values.keys().map(String::as_str)
    }

    #[must_use]
    pub fn value(&self, field: &str) -> Option<&[u8]> {
        self.values.get(field).map(AsRef::as_ref)
    }

    #[must_use]
    pub fn target(&self, field: &str) -> Option<&InjectionTargetV1> {
        self.targets.get(field)
    }

    #[must_use]
    pub fn redactor(&self) -> &Redactor {
        &self.redactor
    }
}

impl Drop for SecretMaterializationV1 {
    fn drop(&mut self) {
        for value in self.values.values_mut() {
            value.zeroize();
        }
    }
}

pub struct SecretMaterializer<C> {
    client: C,
}

impl<C: SecretLeaseClientV1> SecretMaterializer<C> {
    #[must_use]
    pub fn new(client: C) -> Self {
        Self { client }
    }

    pub fn materialize(
        &self,
        plan: &SecretMaterializationPlanV1,
    ) -> Result<SecretMaterializationV1, SecretMaterializationError> {
        let mut requested_fields = BTreeSet::new();
        let mut targets = BTreeMap::new();
        for field in &plan.fields {
            if field.field.is_empty()
                || field.field.len() > 128
                || !field
                    .field
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
                || !requested_fields.insert(field.field.clone())
                || !valid_target(&field.target)
            {
                return Err(SecretMaterializationError::InvalidPlan);
            }
            targets.insert(field.field.clone(), field.target.clone());
        }
        let delivery = self.client.redeem(&RedeemLeaseRequestV1 {
            lease_id: plan.lease.lease_id.clone(),
            decision_id: plan.decision_id.clone(),
            invocation_id: plan.invocation_id.clone(),
            host_generation: plan.host_generation,
            requested_fields: requested_fields.clone(),
        })?;
        if delivery.fields.keys().cloned().collect::<BTreeSet<_>>() != requested_fields {
            let _ = self.client.revoke(&plan.lease.lease_id);
            return Err(SecretMaterializationError::FieldMismatch);
        }
        let redaction_values = delivery
            .fields
            .values()
            .filter_map(|value| std::str::from_utf8(value).ok().map(str::to_owned))
            .collect();
        Ok(SecretMaterializationV1 {
            values: delivery.into_fields(),
            targets,
            redactor: Redactor::new(redaction_values),
        })
    }

    pub fn revoke(&self, lease_id: &StableId) -> Result<(), SecretMaterializationError> {
        self.client.revoke(lease_id)
    }
}

fn valid_target(target: &InjectionTargetV1) -> bool {
    match target {
        InjectionTargetV1::Header(name) => {
            !name.is_empty()
                && name.len() <= 128
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        }
        InjectionTargetV1::Environment(name) => {
            !name.is_empty()
                && name.len() <= 128
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        }
        InjectionTargetV1::Stdin | InjectionTargetV1::ProtectedFile => true,
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum SecretMaterializationError {
    #[error("secret materialization plan is malformed")]
    InvalidPlan,
    #[error("secret broker returned fields outside the approved plan")]
    FieldMismatch,
    #[error("secret lease was denied")]
    LeaseDenied,
    #[error("authenticated secret channel is unavailable")]
    ChannelUnavailable,
}
