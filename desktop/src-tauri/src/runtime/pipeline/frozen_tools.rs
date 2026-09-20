//! A Chat keeps authority; this build owns every tool interface. Job control
//! rides with the capability that launches work, so a Chat is never left on a
//! second, older behaviour by a switch that happened to stay off.
use super::*;

/// The effective bindings for one pass: the capabilities the current workflow
/// document binds, plus the control tools implied by a bound host shell/Python
/// capability.
///
/// The document decides *which* capabilities the pass may call. A capability the
/// Chat already held keeps the configuration it was frozen with, because that
/// configuration — executable identity, approval mode, child inheritance — is its
/// authority contract, and the interface is re-derived from it by this build. A
/// capability the Chat never held is frozen from the current document through the
/// same path as a first-input freeze, so the approval class this build derives is
/// the one the broker settles at the point of use instead of silently granting it.
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
    if let Some(previous) = previous {
        for tool in &mut complete {
            if let Some(saved) = previous
                .iter()
                .find(|saved| saved.capability_id == tool.capability_id)
            {
                tool.configuration = saved.configuration.clone();
            }
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

/// Authority and saved configuration travel from the Chat; interface fields do
/// not. The executable identity, approval class, credential binding and
/// file-access contract a capability was frozen with stay exactly as the Chat
/// holds them, so an improvement to a tool reaches the model while an edit can
/// never widen what that tool is allowed to do.
fn restore_authority(binding: &mut StoredFileToolBindingV1, saved: &StoredFileToolBindingV1) {
    binding.options = saved.options.clone();
    binding.requires_approval = saved.requires_approval;
    binding.secret = saved.secret.clone();
    binding.file_access_version = saved.file_access_version;
    binding.internal_id = saved.internal_id.clone();
}
