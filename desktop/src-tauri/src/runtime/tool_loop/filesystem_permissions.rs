//! Bind reusable user permissions to the broker's frozen file target, never to
//! renderer-supplied paths or model-selected tool names.
use super::*;
use crate::runtime::approvals::{
    ApprovalContext, ApprovalResolution, FilesystemAccess, FilesystemGrant,
};

pub(super) fn required_access(capability: &str) -> FilesystemAccess {
    if matches!(
        capability,
        FILE_WRITE_CAPABILITY_ID | FILE_EDIT_CAPABILITY_ID
    ) {
        FilesystemAccess::Write
    } else {
        FilesystemAccess::Read
    }
}

impl FileToolAuthorityRuntimeV1 {
    pub(crate) fn filesystem_grant(
        &self,
        context: &ApprovalContext,
        decision_id: &str,
        resolution: &ApprovalResolution,
    ) -> Result<Option<FilesystemGrant>, WorkflowPipelineError> {
        let Some(selection) = &resolution.filesystem else {
            return Ok(None);
        };
        let proposal = self
            .ledger
            .events(&stable(decision_id)?)
            .map_err(broker_error)?
            .into_iter()
            .find_map(|event| match event {
                InvocationLedgerEventV1::Proposed { proposal, .. } => Some(proposal),
                _ => None,
            })
            .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
        let record = self
            .records
            .invocation(&proposal.proposal_id)?
            .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
        let access = record
            .file_access
            .as_ref()
            .and_then(|access| access.as_ref().ok())
            .filter(|access| access.outside_workspace)
            .ok_or_else(|| {
                WorkflowPipelineError::InvalidInput(
                    "Only an external file operation can create a filesystem permission.".into(),
                )
            })?;
        self.projects
            .revalidate_workspace_v1(&record.workspace)
            .and_then(|_| self.projects.revalidate_workspace_v1(&access.directory))
            .map_err(|e| WorkflowPipelineError::Authority(e.to_string()))?;
        FilesystemGrant::create(
            context,
            &resolution.choice,
            selection,
            required_access(&record.call.capability_id),
            &access.target(),
            &self.projects,
        )
        .map(Some)
        .map_err(WorkflowPipelineError::InvalidInput)
    }
}
