//! A Chat keeps authority; this build owns every tool interface. Job control
//! rides with the capability that launches work, so a Chat is never left on a
//! second, older behaviour by a switch that happened to stay off.
use super::*;

/// The effective bindings for one Chat: the requested capabilities plus the
/// control tools implied by a bound host shell/Python capability. A saved set
/// supplies authority only; frozen hashes, schemas and descriptions never
/// shadow an improved tool.
pub(super) fn effective(
    requested: &[WorkflowToolBindingV1],
    previous: Option<&[StoredFileToolBindingV1]>,
) -> Result<Vec<StoredFileToolBindingV1>, WorkflowPipelineError> {
    let bound: Vec<String> = requested
        .iter()
        .map(|tool| tool.capability_id.clone())
        .collect();
    let mut complete = requested.to_vec();
    for implied in implied_requests(&bound) {
        if !complete
            .iter()
            .any(|tool| tool.capability_id == implied.capability_id)
        {
            complete.push(implied);
        }
    }
    let mut bindings = freeze_file_tool_bindings(&complete)?;
    if let Some(previous) = previous {
        for binding in &mut bindings {
            if let Some(saved) = previous
                .iter()
                .find(|saved| saved.capability_id == binding.capability_id)
            {
                restore_authority(binding, saved);
            }
        }
    }
    Ok(bindings)
}

/// An existing Chat re-resolves its interface from this build, so a Chat frozen
/// before a tool improved adopts the improvement in its next pass - including the
/// control tools that serve a host shell/Python capability it already holds.
pub(super) fn complete_saved(
    saved: &[StoredFileToolBindingV1],
) -> Result<Vec<StoredFileToolBindingV1>, WorkflowPipelineError> {
    let requested: Vec<WorkflowToolBindingV1> = saved.iter().map(request_from_saved).collect();
    effective(&requested, Some(saved))
}

fn request_from_saved(saved: &StoredFileToolBindingV1) -> WorkflowToolBindingV1 {
    WorkflowToolBindingV1 {
        // Options are frozen authority (executable identity, approval mode) and
        // are restored after the refresh instead of being revalidated as new.
        options: Default::default(),
        capability_id: saved.capability_id.clone(),
        configuration: saved.configuration.clone(),
        credential_bindings: saved
            .secret
            .iter()
            .map(
                |secret| super::super::tool_loop::WorkflowToolCredentialBindingV1 {
                    name: secret.name.clone(),
                    credential_ref: secret.credential_ref.clone(),
                    field: secret.field.clone(),
                    field_names: secret.field_names.clone(),
                    revision: secret.revision,
                },
            )
            .collect(),
        definition: saved
            .capability_id
            .starts_with(MCP_CAPABILITY_PREFIX)
            .then(|| aworkit_capability_host::ModelToolDefinitionV1 {
                capability_id: saved.capability_id.clone(),
                name: saved.provider_name.clone(),
                description: saved.description.clone(),
                input_schema: saved.input_schema.clone(),
            }),
    }
}

fn implied_requests(bound: &[String]) -> Vec<WorkflowToolBindingV1> {
    super::super::tool_loop::implied_job_control_ids(bound)
        .into_iter()
        .map(|capability_id| WorkflowToolBindingV1 {
            options: Default::default(),
            capability_id: capability_id.to_owned(),
            configuration: json!({}),
            credential_bindings: Vec::new(),
            definition: None,
        })
        .collect()
}

/// Authority travels from the saved Chat; interface fields do not.
fn restore_authority(binding: &mut StoredFileToolBindingV1, saved: &StoredFileToolBindingV1) {
    binding.options = saved.options.clone();
    binding.requires_approval = saved.requires_approval;
    binding.secret = saved.secret.clone();
    binding.file_access_version = saved.file_access_version;
    binding.internal_id = saved.internal_id.clone();
}
