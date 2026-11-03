//! Model routes and their adapter authority for each prepared workflow pass.
use super::*;

impl StoredProviderBindingV1 {
    /// Captures all dispatch settings and the credential reference as one route.
    pub(super) fn from_request(
        request: &WorkflowExecutionRequestV1,
    ) -> Result<(Self, Option<StoredSecretBindingV1>), WorkflowPipelineError> {
        let provider = &request.provider;
        let secret = provider
            .credential
            .as_ref()
            .map(StoredSecretBindingV1::from_metadata)
            .transpose()?;
        Ok((
            Self {
                kind: provider.kind.clone(),
                base_url: provider.base_url.clone(),
                model: provider.model.clone(),
                parameters: request.model_parameters.clone(),
                model_context: request.model_context.clone(),
                request_timeout_seconds: provider.request_timeout_seconds,
                maximum_tool_output_bytes: provider.maximum_tool_output_bytes,
            },
            secret,
        ))
    }

    /// Model capabilities for the acting route and any auxiliary summary route.
    pub(super) fn capability_bindings(
        &self,
        primary: ProviderProtocolV1,
        descriptors: &BTreeMap<ProviderProtocolV1, CapabilityDescriptor>,
    ) -> Result<Vec<CapabilityBindingV1>, WorkflowPipelineError> {
        let metadata: crate::runtime::compaction::Metadata =
            serde_json::from_value(self.model_context.clone()).map_err(json_error)?;
        let mut protocols = vec![primary];
        if let Some(target) = metadata.summary_target {
            let summary = ProviderProtocolV1::parse(&target.provider.kind)?;
            if summary != primary {
                protocols.push(summary);
            }
        }
        protocols
            .into_iter()
            .map(|protocol| {
                let descriptor = descriptors
                    .get(&protocol)
                    .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
                Ok(CapabilityBindingV1 {
                    capability_id: stable(protocol.capability_id())?,
                    adapter_id: stable(protocol.adapter_id())?,
                    adapter_version: descriptor.version.clone(),
                    descriptor_hash: descriptor.version_hash.clone(),
                    extension: None,
                    required_isolation_profile: descriptor.required_isolation.clone(),
                    enabled: true,
                    compatible: true,
                    approval: ApprovalRequirement::Never,
                    allowed_node_types: vec!["agent".to_owned(), "model_call".to_owned()],
                })
            })
            .collect()
    }
}

/// Validates the transport for a fresh pass independently of the Run's budget.
pub(super) fn validate_provider_binding(
    provider: &WorkflowProviderBindingV1,
    protocol: ProviderProtocolV1,
    descriptor: &CapabilityDescriptor,
) -> Result<(), WorkflowPipelineError> {
    if provider.request_timeout_seconds == 0
        || provider.request_timeout_seconds > MAXIMUM_PROVIDER_REQUEST_TIMEOUT_SECONDS_V1
        || !(1024..=512 * 1024).contains(&provider.maximum_tool_output_bytes)
    {
        return Err(WorkflowPipelineError::InvalidInput(
            "workflow provider limits do not match the supported transport bounds".to_owned(),
        ));
    }
    if descriptor.capability_id != protocol.capability_id() {
        return Err(WorkflowPipelineError::IncompleteEvidence);
    }
    match protocol {
        ProviderProtocolV1::OpenAiCompatible | ProviderProtocolV1::XiaomiMiMo => {
            OpenAiCompatibleProviderConfig::new(
                protocol.capability_id(),
                descriptor.version_hash.clone(),
                &provider.base_url,
                provider.model.clone(),
                None,
                OpenAiCompatibleLimitsV1::default(),
            )
            .map_err(|error| WorkflowPipelineError::InvalidInput(error.to_string()))?;
        }
        ProviderProtocolV1::Anthropic => {
            AnthropicMessagesProviderConfig::new(
                protocol.capability_id(),
                descriptor.version_hash.clone(),
                &provider.base_url,
                provider.model.clone(),
                None,
                AnthropicMessagesLimitsV1::default(),
            )
            .map_err(|error| WorkflowPipelineError::InvalidInput(error.to_string()))?;
        }
        ProviderProtocolV1::Gemini => {
            GoogleGeminiProviderConfig::new(
                protocol.capability_id(),
                descriptor.version_hash.clone(),
                &provider.base_url,
                provider.model.clone(),
                None,
                GoogleGeminiLimitsV1::default(),
            )
            .map_err(|error| WorkflowPipelineError::InvalidInput(error.to_string()))?;
        }
    }
    Ok(())
}

/// Updates the per-pass route and broker authority while retaining the original
/// Run snapshot as evidence. The same request ID replays its prepared record.
pub(super) fn adopt_current_model(
    saved: &mut PreparedExecutionRecordV1,
    request: &WorkflowExecutionRequestV1,
    descriptors: &BTreeMap<ProviderProtocolV1, CapabilityDescriptor>,
) -> Result<(), WorkflowPipelineError> {
    let protocol = ProviderProtocolV1::parse(&request.provider.kind)?;
    let descriptor = descriptors
        .get(&protocol)
        .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
    validate_provider_binding(&request.provider, protocol, descriptor)?;
    let (provider, secret) = StoredProviderBindingV1::from_request(request)?;
    let model_bindings = provider.capability_bindings(protocol, descriptors)?;
    let capability_id = stable(protocol.capability_id())?;

    let manifest = &mut saved.manifest;
    manifest.capability_bindings.retain(|binding| {
        !ProviderProtocolV1::ALL
            .iter()
            .any(|protocol| binding.capability_id.as_str() == protocol.capability_id())
    });
    manifest.capability_bindings.extend(model_bindings);
    manifest.capability_bindings.sort_by(|left, right| {
        left.capability_id
            .as_str()
            .cmp(right.capability_id.as_str())
    });
    manifest.manifest_hash = canonical_digest(&manifest.capability_bindings)?;
    manifest.manifest_id = stable(&format!("manifest.{}", &manifest.manifest_hash[..32]))?;
    manifest.summary = format!(
        "{} exact capability binding(s); {} require per-invocation approval",
        manifest.capability_bindings.len(),
        manifest
            .capability_bindings
            .iter()
            .filter(|binding| binding.approval == ApprovalRequirement::PerInvocation)
            .count()
    );

    saved.provider = provider;
    saved.secret = secret;
    saved.worker_proposal.capability_ref = capability_id.clone();
    saved.worker_proposal.authority_manifest_ref = manifest.manifest_id.clone();
    saved.broker_proposal.capability_id = capability_id;
    Ok(())
}
