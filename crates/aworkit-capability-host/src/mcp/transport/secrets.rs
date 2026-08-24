//! Non-formattable, zeroizing secret material retained only by live transports.

use std::collections::BTreeSet;

use serde_json::Value;
use zeroize::Zeroizing;

use crate::{
    InjectionTargetV1, McpServerManifestV1, McpTransportKindV1, Redactor, SecretMaterializationV1,
};

use super::{
    McpStreamableHttpTransportConfigV1, McpTransportConfigurationError, fold_environment_name,
    valid_environment_name,
};

pub(super) struct MaterializedTransportSecrets {
    fields: Vec<MaterializedTransportSecret>,
    redactor: Redactor,
}

struct MaterializedTransportSecret {
    name: String,
    target: InjectionTargetV1,
    value: Zeroizing<Vec<u8>>,
}

impl Clone for MaterializedTransportSecret {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            target: self.target.clone(),
            value: Zeroizing::new(self.value.as_slice().to_vec()),
        }
    }
}

impl Clone for MaterializedTransportSecrets {
    fn clone(&self) -> Self {
        Self {
            fields: self.fields.clone(),
            redactor: self.redactor.clone(),
        }
    }
}

impl MaterializedTransportSecrets {
    pub(super) fn empty() -> Self {
        Self {
            fields: Vec::new(),
            redactor: Redactor::default(),
        }
    }

    pub(super) fn from_materialization(
        materialization: SecretMaterializationV1,
    ) -> Result<Self, McpTransportConfigurationError> {
        let mut fields = Vec::new();
        let mut redaction_values = Vec::new();
        for name in materialization.field_names() {
            let target = materialization
                .target(name)
                .cloned()
                .ok_or(McpTransportConfigurationError::InvalidSecretMaterial)?;
            let value = materialization
                .value(name)
                .ok_or(McpTransportConfigurationError::InvalidSecretMaterial)?;
            if value.is_empty() {
                return Err(McpTransportConfigurationError::InvalidSecretMaterial);
            }
            if let Ok(text) = std::str::from_utf8(value) {
                redaction_values.push(text.to_owned());
            }
            fields.push(MaterializedTransportSecret {
                name: name.to_owned(),
                target,
                value: Zeroizing::new(value.to_vec()),
            });
        }
        Ok(Self {
            fields,
            redactor: Redactor::new(redaction_values),
        })
    }

    pub(super) fn validate(
        &self,
        manifest: &McpServerManifestV1,
        http: Option<&McpStreamableHttpTransportConfigV1>,
    ) -> Result<(), McpTransportConfigurationError> {
        let names = self
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>();
        if names != manifest.secret_slots {
            return Err(McpTransportConfigurationError::InvalidSecretMaterial);
        }
        match manifest.transport {
            McpTransportKindV1::Stdio => self.validate_stdio(),
            McpTransportKindV1::StreamableHttp => self
                .validate_http(http.ok_or(McpTransportConfigurationError::InvalidSecretMaterial)?),
        }
    }

    fn validate_stdio(&self) -> Result<(), McpTransportConfigurationError> {
        let mut targets = BTreeSet::new();
        for field in &self.fields {
            let InjectionTargetV1::Environment(name) = &field.target else {
                return Err(McpTransportConfigurationError::InvalidSecretMaterial);
            };
            if !valid_environment_name(name)
                || std::str::from_utf8(field.value.as_slice()).is_err()
                || field.value.contains(&0)
                || !targets.insert(fold_environment_name(name))
            {
                return Err(McpTransportConfigurationError::InvalidSecretMaterial);
            }
        }
        Ok(())
    }

    fn validate_http(
        &self,
        config: &McpStreamableHttpTransportConfigV1,
    ) -> Result<(), McpTransportConfigurationError> {
        let mut targets = BTreeSet::new();
        let mut bearer_found = config.bearer_token_secret_slot.is_none();
        for field in &self.fields {
            let InjectionTargetV1::Header(name) = &field.target else {
                return Err(McpTransportConfigurationError::InvalidSecretMaterial);
            };
            let header = http::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| McpTransportConfigurationError::InvalidSecretMaterial)?;
            if reserved_header(&header)
                || http::HeaderValue::from_bytes(field.value.as_slice()).is_err()
                || !targets.insert(header.as_str().to_owned())
            {
                return Err(McpTransportConfigurationError::InvalidSecretMaterial);
            }
            if config.bearer_token_secret_slot.as_deref() == Some(field.name.as_str()) {
                if !header.as_str().eq_ignore_ascii_case("authorization") {
                    return Err(McpTransportConfigurationError::InvalidSecretMaterial);
                }
                bearer_found = true;
            }
        }
        if !bearer_found {
            return Err(McpTransportConfigurationError::InvalidSecretMaterial);
        }
        Ok(())
    }

    pub(super) fn environment(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.fields.iter().filter_map(|field| {
            let InjectionTargetV1::Environment(name) = &field.target else {
                return None;
            };
            Some((name.as_str(), field.value.as_slice()))
        })
    }

    pub(super) fn headers(&self) -> impl Iterator<Item = (&str, &str, &[u8])> {
        self.fields.iter().filter_map(|field| {
            let InjectionTargetV1::Header(name) = &field.target else {
                return None;
            };
            Some((field.name.as_str(), name.as_str(), field.value.as_slice()))
        })
    }

    pub(super) fn redact_json(&self, value: &mut Value) {
        match value {
            Value::String(text) => *text = self.redactor.redact(text),
            Value::Array(values) => {
                for value in values {
                    self.redact_json(value);
                }
            }
            Value::Object(values) => {
                let original = std::mem::take(values);
                for (name, mut value) in original {
                    self.redact_json(&mut value);
                    values.insert(self.redactor.redact(&name), value);
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }

    pub(super) fn redact_text(&self, value: &str) -> String {
        self.redactor.redact(value)
    }
}

fn reserved_header(name: &http::HeaderName) -> bool {
    let name = name.as_str();
    name.eq_ignore_ascii_case("accept")
        || name.eq_ignore_ascii_case("connection")
        || name.eq_ignore_ascii_case("content-length")
        || name.eq_ignore_ascii_case("content-type")
        || name.eq_ignore_ascii_case("expect")
        || name.eq_ignore_ascii_case("host")
        || name.eq_ignore_ascii_case("proxy-authorization")
        || name.eq_ignore_ascii_case("transfer-encoding")
        || name.eq_ignore_ascii_case("upgrade")
        || name.eq_ignore_ascii_case("mcp-session-id")
        || name.eq_ignore_ascii_case("mcp-protocol-version")
        || name.eq_ignore_ascii_case("last-event-id")
        || name.eq_ignore_ascii_case("mcp-method")
        || name.eq_ignore_ascii_case("mcp-name")
        || name.to_ascii_lowercase().starts_with("mcp-param-")
}
