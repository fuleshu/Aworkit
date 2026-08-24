use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use aworkit_protocol::{ProcessGeneration, StableId};
use aworkit_trusted_core::{
    CredentialMetadataV1, CredentialRef, PlatformCredentialStorePort, RedeemLeaseRequestV1,
    ScopedLeaseRequestV1, SecretBroker, SecretError,
};
use zeroize::Zeroizing;

use super::settings_v2::CredentialMetadataConfigurationV2;

const API_KEY_FIELD: &str = "api_key";

pub(crate) struct CredentialVault {
    broker: SecretBroker,
    lease_ordinal: u64,
}

impl CredentialVault {
    pub(crate) fn with_store(
        store: Arc<dyn PlatformCredentialStorePort>,
        credentials: &[CredentialMetadataConfigurationV2],
    ) -> Result<Self, String> {
        Self::from_broker(SecretBroker::with_store(store), credentials)
    }

    fn from_broker(
        mut broker: SecretBroker,
        credentials: &[CredentialMetadataConfigurationV2],
    ) -> Result<Self, String> {
        for metadata in credentials {
            let credential = parse_ref(&metadata.credential_ref)?;
            let field_names = metadata
                .field_names
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>();
            broker
                .restore_credential_metadata(CredentialMetadataV1 {
                    credential,
                    field_names,
                    revision: metadata.revision,
                })
                .map_err(|error| format!("cannot restore credential metadata: {error}"))?;
        }
        Ok(Self {
            broker,
            lease_ordinal: 0,
        })
    }

    /// Places an API key at a caller-preallocated opaque reference. The caller
    /// durably journals that reference before invoking this method.
    pub(crate) fn put_api_key_at(
        &mut self,
        reference: &str,
        api_key: &str,
    ) -> Result<CredentialMetadataV1, String> {
        if api_key.trim().is_empty() {
            return Err("API key cannot be empty when Replace is selected".into());
        }
        self.broker
            .put_credential(
                parse_ref(reference)?,
                BTreeMap::from([(API_KEY_FIELD.into(), api_key.as_bytes().to_vec())]),
            )
            .map_err(|error| {
                format!("cannot store API key in the operating-system credential store: {error}")
            })
    }

    /// Stores a new opaque multi-field credential at a journaled reference
    /// without returning plaintext.
    pub(crate) fn put_fields_at(
        &mut self,
        reference: &str,
        fields: BTreeMap<String, Zeroizing<String>>,
    ) -> Result<CredentialMetadataV1, String> {
        if fields.is_empty() {
            return Err("credential requires at least one secret field".into());
        }
        let fields = fields
            .into_iter()
            .map(|(name, value)| {
                if name.trim().is_empty() || value.is_empty() {
                    return Err(
                        "credential field names and write-only values cannot be empty".to_owned(),
                    );
                }
                Ok((name, value.as_bytes().to_vec()))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        self.broker
            .put_credential(parse_ref(reference)?, fields)
            .map_err(|error| {
                format!("cannot store credential in the operating-system credential store: {error}")
            })
    }

    pub(crate) fn clear(&mut self, reference: Option<&str>) -> Result<(), String> {
        let Some(reference) = reference else {
            return Ok(());
        };
        match self.broker.delete_credential(&parse_ref(reference)?) {
            Ok(()) | Err(SecretError::UnknownCredential) => Ok(()),
            Err(error) => Err(format!(
                "cannot remove API key from the operating-system credential store: {error}"
            )),
        }
    }

    pub(crate) fn resolve(&mut self, reference: Option<&str>) -> Result<Option<String>, String> {
        let Some(reference) = reference else {
            return Ok(None);
        };
        let mut delivery =
            self.resolve_fields(reference, BTreeSet::from([API_KEY_FIELD.to_owned()]))?;
        let bytes = delivery
            .remove(API_KEY_FIELD)
            .ok_or_else(|| "stored credential has no API-key field".to_owned())?;
        String::from_utf8(bytes.as_slice().to_vec())
            .map(Some)
            .map_err(|_| "stored API key is not valid UTF-8".to_owned())
    }

    /// Redeems exactly the requested fields from one credential into
    /// invocation-local, zeroizing memory. Callers must not serialize or log
    /// the returned values.
    pub(crate) fn resolve_fields(
        &mut self,
        reference: &str,
        requested_fields: BTreeSet<String>,
    ) -> Result<BTreeMap<String, Zeroizing<Vec<u8>>>, String> {
        if requested_fields.is_empty() {
            return Err("credential field resolution requires at least one field".into());
        }
        self.lease_ordinal = self.lease_ordinal.saturating_add(1);
        let suffix = self.lease_ordinal;
        let lease_id = stable(&format!("lease.desktop.{suffix}"))?;
        let decision_id = stable(&format!("decision.desktop.{suffix}"))?;
        let invocation_id = stable(&format!("invocation.desktop.{suffix}"))?;
        self.broker
            .issue_scoped(ScopedLeaseRequestV1 {
                lease_id: lease_id.clone(),
                credential: parse_ref(reference)?,
                decision_id: decision_id.clone(),
                invocation_id: invocation_id.clone(),
                run_id: stable("run.local")?,
                audience_generation: ProcessGeneration(1),
                permitted_fields: requested_fields.clone(),
                ttl: Duration::from_secs(30),
                maximum_uses: 1,
            })
            .map_err(|error| format!("cannot issue scoped credential lease: {error}"))?;
        let delivery = self
            .broker
            .redeem_scoped(&RedeemLeaseRequestV1 {
                lease_id,
                decision_id,
                invocation_id,
                audience_generation: ProcessGeneration(1),
                requested_fields: requested_fields.clone(),
            })
            .map_err(|error| {
                format!(
                    "cannot read scoped fields from the operating-system credential store: {error}"
                )
            })?;
        let fields = delivery.into_fields();
        if fields.keys().cloned().collect::<BTreeSet<_>>() != requested_fields {
            return Err("credential store returned fields outside the scoped request".into());
        }
        Ok(fields)
    }
}

fn parse_ref(value: &str) -> Result<CredentialRef, String> {
    Ok(CredentialRef(stable(value)?))
}

fn stable(value: &str) -> Result<StableId, String> {
    StableId::parse(value.to_owned()).map_err(|error| error.to_string())
}
