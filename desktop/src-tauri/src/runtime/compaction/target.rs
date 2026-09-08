use super::*;

pub(crate) fn freeze_summary_target(
    settings: &super::super::settings_v2::SettingsConfigurationV2,
    model: &super::super::settings_v2::ModelConfigurationV2,
    tools: bool,
) -> Result<Option<FrozenSummaryTarget>, String> {
    let policy: Policy =
        serde_json::from_value(model.compaction.clone().unwrap_or_else(|| json!({})))
            .map_err(|e| e.to_string())?;
    policy.validate(model.context_window)?;
    let Some(provider_id) = policy
        .summarization_provider
        .as_deref()
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    let provider = settings
        .providers
        .iter()
        .find(|p| p.id == provider_id)
        .ok_or("Summary provider is not configured")?;
    let needs_vision = model.capabilities.iter().any(|c| c == "vision");
    let model = provider
        .models
        .iter()
        .find(|m| Some(m.id.as_str()) == policy.summarization_model.as_deref())
        .ok_or("Summary model is not configured")?;
    if !provider.enabled
        || !model.enabled
        || !model.capabilities.iter().any(|c| c == "text")
        || (tools && !model.capabilities.iter().any(|c| c == "tools"))
    {
        return Err("Summary provider/model must be enabled and support text plus the acting model's tools.".into());
    }
    if needs_vision && !model.capabilities.iter().any(|c| c == "vision") {
        return Err(
            "Summary model must support vision when the acting model accepts images.".into(),
        );
    }
    let credential = provider
        .credential_ref
        .as_ref()
        .map(|reference| {
            let metadata = settings
                .credential(reference)
                .ok_or("Summary credential metadata is unavailable")?;
            Ok::<_, String>(super::super::history::FrozenCredentialBindingV1 {
                credential_ref: aworkit_protocol::StableId::parse(reference.clone())
                    .map_err(|e| e.to_string())?,
                field_names: metadata.field_names.iter().cloned().collect(),
                revision: metadata.revision,
            })
        })
        .transpose()?;
    Ok(Some(FrozenSummaryTarget {
        provider: provider.clone(),
        model: model.clone(),
        credential,
    }))
}
