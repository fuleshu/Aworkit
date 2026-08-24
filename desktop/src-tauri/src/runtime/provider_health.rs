//! Durable, secret-free health projections for exact saved provider profiles.
//!
//! Health is keyed by provider ID and a canonical fingerprint of the complete
//! saved provider configuration. A stale connection test or a frozen Chat can
//! therefore never badge a different provider revision as ready.

use std::collections::{BTreeMap, BTreeSet};

use aworkit_local_store::{
    DocumentAccessMode, DocumentKind, DocumentRepository, JsonDocument, RepositoryRoot,
};
use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::settings_v2::ProviderConfigurationV2;

const PROVIDER_HEALTH_DOCUMENT_ID: &str = "provider-health.desktop";
const PROVIDER_HEALTH_SCHEMA_VERSION: u16 = 1;
const MAXIMUM_HEALTH_DETAIL_BYTES: usize = 16 * 1024;
const ALLOWED_HEALTH_STATES: [&str; 5] =
    ["unconfigured", "configured", "ready", "error", "disabled"];

/// User-facing health for one exact provider revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProviderHealth {
    pub state: String,
    pub detail: Option<String>,
}

impl ProviderHealth {
    pub(crate) fn ready(detail: impl Into<String>) -> Self {
        Self::new("ready", Some(detail.into()))
    }

    pub(crate) fn error(detail: impl Into<String>) -> Self {
        Self::new("error", Some(detail.into()))
    }

    pub(crate) fn legacy_unconfigured() -> Self {
        Self::new(
            "unconfigured",
            Some("Enter an OpenAI-compatible base URL and model, then test it.".into()),
        )
    }

    fn for_provider(provider: &ProviderConfigurationV2) -> Self {
        if provider.enabled {
            Self::new(
                "configured",
                Some("Saved locally. Run Test connection to verify the endpoint.".into()),
            )
        } else {
            Self::new("disabled", Some("Provider is disabled.".into()))
        }
    }

    fn new(state: &str, detail: Option<String>) -> Self {
        Self {
            state: state.into(),
            detail: detail.map(bounded_detail),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderHealthRecordV1 {
    provider_id: String,
    provider_fingerprint: String,
    state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

impl ProviderHealthRecordV1 {
    fn baseline(provider: &ProviderConfigurationV2) -> Result<Self, String> {
        let health = ProviderHealth::for_provider(provider);
        Ok(Self {
            provider_id: provider.id.clone(),
            provider_fingerprint: provider_fingerprint(provider)?,
            state: health.state,
            detail: health.detail,
        })
    }

    fn health(&self) -> ProviderHealth {
        ProviderHealth {
            state: self.state.clone(),
            detail: self.detail.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderHealthDocumentV1 {
    schema_version: u16,
    providers: Vec<ProviderHealthRecordV1>,
}

/// Versioned native projection store for provider health.
pub(crate) struct ProviderHealthRegistry {
    repository: RepositoryRoot,
    version: u64,
    records: BTreeMap<String, ProviderHealthRecordV1>,
}

impl ProviderHealthRegistry {
    /// Opens the projection and reconciles it against current canonical
    /// Settings. Added or changed providers receive a fresh baseline; removed
    /// providers are discarded.
    pub(crate) fn open(
        data_root: &std::path::Path,
        providers: &[ProviderConfigurationV2],
    ) -> Result<Self, String> {
        let repository = RepositoryRoot::open(data_root.join("documents"))
            .map_err(|error| format!("cannot open provider-health repository: {error}"))?;
        let mut registry = match repository
            .load(DocumentKind::Configuration, PROVIDER_HEALTH_DOCUMENT_ID)
            .map_err(|error| format!("cannot load provider health: {error}"))?
        {
            Some(stored) => {
                if stored.access != DocumentAccessMode::Editable {
                    return Err("provider-health schema is not editable".into());
                }
                let document: ProviderHealthDocumentV1 =
                    serde_json::from_slice(stored.document.raw_json())
                        .map_err(|error| format!("provider-health document is invalid: {error}"))?;
                validate_document(&document)?;
                Self {
                    repository,
                    version: stored.version,
                    records: document
                        .providers
                        .into_iter()
                        .map(|record| (record.provider_id.clone(), record))
                        .collect(),
                }
            }
            None => {
                let document = ProviderHealthDocumentV1 {
                    schema_version: PROVIDER_HEALTH_SCHEMA_VERSION,
                    providers: providers
                        .iter()
                        .map(ProviderHealthRecordV1::baseline)
                        .collect::<Result<Vec<_>, _>>()?,
                };
                let stored = repository
                    .save(
                        DocumentKind::Configuration,
                        PROVIDER_HEALTH_DOCUMENT_ID,
                        None,
                        &encode(&document)?,
                    )
                    .map_err(|error| format!("cannot create provider health: {error}"))?;
                Self {
                    repository,
                    version: stored.version,
                    records: document
                        .providers
                        .into_iter()
                        .map(|record| (record.provider_id.clone(), record))
                        .collect(),
                }
            }
        };
        registry.reconcile(providers)?;
        Ok(registry)
    }

    /// Returns health only when its persisted fingerprint still identifies the
    /// exact supplied provider. Any drift is projected as untested immediately.
    pub(crate) fn health(&self, provider: &ProviderConfigurationV2) -> ProviderHealth {
        if !provider.enabled {
            return ProviderHealth::for_provider(provider);
        }
        let fingerprint = provider_fingerprint(provider).ok();
        self.records
            .get(&provider.id)
            .filter(|record| Some(record.provider_fingerprint.as_str()) == fingerprint.as_deref())
            .map_or_else(
                || ProviderHealth::for_provider(provider),
                |record| record.health(),
            )
    }

    /// Persists health only if the tested/executed provider is byte-for-byte
    /// equivalent (under canonical JSON) to a currently saved provider.
    pub(crate) fn set_exact(
        &mut self,
        saved_providers: &[ProviderConfigurationV2],
        exact_provider: &ProviderConfigurationV2,
        health: ProviderHealth,
    ) -> Result<bool, String> {
        let Some(saved) = saved_providers
            .iter()
            .find(|provider| *provider == exact_provider)
        else {
            return Ok(false);
        };
        if !saved.enabled {
            return Ok(false);
        }
        let fingerprint = provider_fingerprint(saved)?;
        let next_record = ProviderHealthRecordV1 {
            provider_id: saved.id.clone(),
            provider_fingerprint: fingerprint,
            state: health.state,
            detail: health.detail,
        };
        if self.records.get(&saved.id) == Some(&next_record) {
            return Ok(true);
        }
        let mut next = self.records.clone();
        next.insert(saved.id.clone(), next_record);
        self.save(next)?;
        Ok(true)
    }

    /// Reconciles add/remove/enable/edit operations after a Settings commit.
    /// Unchanged exact records keep their last result across process restarts.
    pub(crate) fn reconcile(
        &mut self,
        providers: &[ProviderConfigurationV2],
    ) -> Result<(), String> {
        let mut next = BTreeMap::new();
        for provider in providers {
            let fingerprint = provider_fingerprint(provider)?;
            let record = if provider.enabled {
                self.records
                    .get(&provider.id)
                    .filter(|record| record.provider_fingerprint == fingerprint)
                    .cloned()
                    .unwrap_or(ProviderHealthRecordV1::baseline(provider)?)
            } else {
                ProviderHealthRecordV1::baseline(provider)?
            };
            next.insert(provider.id.clone(), record);
        }
        if next != self.records {
            self.save(next)?;
        }
        Ok(())
    }

    fn save(&mut self, next: BTreeMap<String, ProviderHealthRecordV1>) -> Result<(), String> {
        let document = ProviderHealthDocumentV1 {
            schema_version: PROVIDER_HEALTH_SCHEMA_VERSION,
            providers: next.values().cloned().collect(),
        };
        validate_document(&document)?;
        let stored = self
            .repository
            .save(
                DocumentKind::Configuration,
                PROVIDER_HEALTH_DOCUMENT_ID,
                Some(self.version),
                &encode(&document)?,
            )
            .map_err(|error| format!("cannot commit provider health: {error}"))?;
        self.version = stored.version;
        self.records = next;
        Ok(())
    }
}

fn provider_fingerprint(provider: &ProviderConfigurationV2) -> Result<String, String> {
    let bytes = serde_jcs::to_vec(provider).map_err(|error| {
        format!(
            "cannot fingerprint saved provider '{}': {error}",
            provider.id
        )
    })?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn encode(document: &ProviderHealthDocumentV1) -> Result<JsonDocument, String> {
    let bytes = serde_jcs::to_vec(document)
        .map_err(|error| format!("cannot encode provider health: {error}"))?;
    JsonDocument::parse(bytes).map_err(|error| format!("cannot encode provider health: {error}"))
}

fn validate_document(document: &ProviderHealthDocumentV1) -> Result<(), String> {
    if document.schema_version != PROVIDER_HEALTH_SCHEMA_VERSION {
        return Err(format!(
            "unsupported provider-health schema {}",
            document.schema_version
        ));
    }
    let mut provider_ids = BTreeSet::new();
    for record in &document.providers {
        StableId::parse(record.provider_id.clone())
            .map_err(|error| format!("provider-health ID is invalid: {error}"))?;
        if !provider_ids.insert(record.provider_id.as_str()) {
            return Err("provider-health document contains duplicate provider IDs".into());
        }
        if record.provider_fingerprint.len() != 71
            || !record.provider_fingerprint.starts_with("sha256:")
            || !record.provider_fingerprint[7..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(format!(
                "provider-health record '{}' has an invalid fingerprint",
                record.provider_id
            ));
        }
        if !ALLOWED_HEALTH_STATES.contains(&record.state.as_str()) {
            return Err(format!(
                "provider-health record '{}' has unsupported state '{}'",
                record.provider_id, record.state
            ));
        }
        if record.detail.as_ref().is_some_and(|detail| {
            detail.len() > MAXIMUM_HEALTH_DETAIL_BYTES || detail.contains('\0')
        }) {
            return Err(format!(
                "provider-health record '{}' has invalid detail",
                record.provider_id
            ));
        }
    }
    Ok(())
}

fn bounded_detail(value: String) -> String {
    let sanitized = value.replace('\0', "�");
    if sanitized.len() <= MAXIMUM_HEALTH_DETAIL_BYTES {
        return sanitized;
    }
    let suffix = "…";
    let mut end = MAXIMUM_HEALTH_DETAIL_BYTES - suffix.len();
    while !sanitized.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &sanitized[..end], suffix)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tempfile::TempDir;

    use super::*;
    use crate::runtime::ModelConfigurationV2;

    fn provider(id: &str, enabled: bool) -> ProviderConfigurationV2 {
        ProviderConfigurationV2 {
            id: id.into(),
            name: id.into(),
            kind: "openai_compatible".into(),
            base_url: format!("https://{id}.example/v1"),
            enabled,
            credential_ref: None,
            models: enabled
                .then(|| ModelConfigurationV2 {
                    id: "model.main".into(),
                    name: "Main".into(),
                    remote_id: "remote-main".into(),
                    enabled: true,
                    context_window: None,
                    max_output_tokens: None,
                    capabilities: vec!["text".into()],
                    parameters: BTreeMap::new(),
                })
                .into_iter()
                .collect(),
            configuration: BTreeMap::new(),
        }
    }

    #[test]
    fn reconciliation_preserves_only_unchanged_exact_saved_providers() {
        let root = TempDir::new().unwrap();
        let first = provider("provider.first", true);
        let removed = provider("provider.removed", true);
        let mut registry =
            ProviderHealthRegistry::open(root.path(), &[first.clone(), removed.clone()]).unwrap();
        registry
            .set_exact(
                &[first.clone(), removed.clone()],
                &first,
                ProviderHealth::ready("verified"),
            )
            .unwrap();

        let disabled = provider("provider.first", false);
        let added = provider("provider.added", true);
        registry
            .reconcile(&[disabled.clone(), added.clone()])
            .unwrap();
        assert!(
            !registry
                .set_exact(
                    &[disabled.clone(), added.clone()],
                    &disabled,
                    ProviderHealth::ready("must stay disabled"),
                )
                .unwrap()
        );
        assert_eq!(registry.health(&disabled).state, "disabled");
        assert_eq!(registry.health(&added).state, "configured");
        assert!(!registry.records.contains_key("provider.removed"));

        drop(registry);
        let reopened =
            ProviderHealthRegistry::open(root.path(), &[disabled.clone(), added]).unwrap();
        assert_eq!(reopened.health(&disabled).state, "disabled");
        assert!(!reopened.records.contains_key("provider.removed"));
    }
}
