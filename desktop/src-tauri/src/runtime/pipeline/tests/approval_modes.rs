//! Behavioral checks cross the real broker, host, persistence and provider port.
use super::*;
use crate::runtime::approvals::{
    ApprovalChoice, ApprovalContext, ApprovalMode, ApprovalResolution,
};

fn scoped_request(
    pipeline: &WorkflowExecutionPipeline,
    metadata: CredentialMetadataV1,
    project: &Path,
    mode: ApprovalMode,
) -> WorkflowExecutionRequestV1 {
    let mut request = edit_approval_request(pipeline, metadata, project);
    request.approvals = ApprovalContext {
        mode,
        chat_id: request.chat_id.to_string(),
        project_key: Some(crate::runtime::approvals::digest(
            request.workspace.as_ref().unwrap(),
        )),
        project_name: Some("Approval fixture".into()),
    };
    request
}

#[test]
fn full_access_executes_once_without_a_reviewer_or_prompt() {
    let root = TempDir::new().unwrap();
    let project = edit_approval_project(&root);
    let (pipeline, _, metadata, calls, _) = setup_tool_pipeline(&root, ToolScriptV1::Edit);
    let request = scoped_request(&pipeline, metadata, &project, ApprovalMode::FullAccess);
    let result = pipeline.execute(request.clone()).unwrap();
    assert_eq!(
        result.status,
        WorkflowExecutionStatusV1::Succeeded,
        "{:?}",
        result.error
    );
    assert_eq!(
        fs::read_to_string(project.join("notes.txt")).unwrap(),
        "beta"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(pipeline.execute(request).unwrap().replayed);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "replay calls neither reviewer nor tool"
    );
}

#[test]
fn automatic_review_approves_denies_or_falls_back_to_a_person() {
    for (script, expected, content, count) in [
        (
            ToolScriptV1::ReviewApprove,
            WorkflowExecutionStatusV1::Succeeded,
            "beta",
            3,
        ),
        (
            ToolScriptV1::ReviewDeny,
            WorkflowExecutionStatusV1::Succeeded,
            "alpha",
            3,
        ),
        (
            ToolScriptV1::ReviewUnavailable,
            WorkflowExecutionStatusV1::AwaitingApproval,
            "alpha",
            2,
        ),
    ] {
        let root = TempDir::new().unwrap();
        let project = edit_approval_project(&root);
        let (pipeline, _, metadata, calls, results) = setup_tool_pipeline(&root, script);
        let request = scoped_request(&pipeline, metadata, &project, ApprovalMode::ApproveForMe);
        let result = pipeline.execute(request.clone()).unwrap();
        assert_eq!(result.status, expected, "{:?}", result.error);
        assert_eq!(
            fs::read_to_string(project.join("notes.txt")).unwrap(),
            content
        );
        assert_eq!(calls.load(Ordering::SeqCst), count);
        if matches!(script, ToolScriptV1::ReviewDeny) {
            let results = results.lock().unwrap();
            assert!(
                results[0]["detail"]
                    .as_str()
                    .unwrap()
                    .contains("preserve the original file")
            );
            assert!(
                results[0]["detail"]
                    .as_str()
                    .unwrap()
                    .contains("Do not retry")
            );
        }
        if let Some(approval) = result.approval {
            assert!(
                approval
                    .message
                    .contains("Automatic review could not complete")
            );
            let resumed = pipeline
                .resume_approval(&approval.decision_id, true)
                .unwrap();
            assert_eq!(resumed.status, WorkflowExecutionStatusV1::Succeeded);
            assert_eq!(
                calls.load(Ordering::SeqCst),
                count + 1,
                "human approval does not run reviewer again"
            );
        } else {
            assert!(pipeline.execute(request).unwrap().replayed);
            assert_eq!(calls.load(Ordering::SeqCst), count);
        }
    }
}

#[test]
fn saved_project_grant_survives_reopen_is_isolated_and_can_be_revoked() {
    let root = TempDir::new().unwrap();
    let project = edit_approval_project(&root);
    let (pipeline, _, metadata, _, _) = setup_tool_pipeline(&root, ToolScriptV1::Edit);
    let request = scoped_request(
        &pipeline,
        metadata.clone(),
        &project,
        ApprovalMode::AskForApproval,
    );
    let result = pipeline.execute(request).unwrap();
    let approval = result.approval.unwrap();
    assert_eq!(
        approval.project_scope.as_deref(),
        Some("Files in this project")
    );
    let resolution = ApprovalResolution {
        choice: ApprovalChoice::AlwaysApproveInProject,
        reason: None,
    };
    assert_eq!(
        pipeline
            .resume_approval_choice(&approval.decision_id, &resolution)
            .unwrap()
            .status,
        WorkflowExecutionStatusV1::Succeeded
    );
    let grants = pipeline.file_tool_authority.approvals.grants().unwrap();
    assert_eq!(grants.len(), 1);
    drop(pipeline);

    let (pipeline, _, metadata, _, _) = setup_tool_pipeline(&root, ToolScriptV1::Edit);
    fs::write(project.join("notes.txt"), "alpha").unwrap();
    let mut next = scoped_request(
        &pipeline,
        metadata.clone(),
        &project,
        ApprovalMode::AskForApproval,
    );
    next.request_id = stable("command.saved-project-grant").unwrap();
    next.run_id = stable("run.saved-project-grant").unwrap();
    next.chat_id = stable("chat.saved-project-grant").unwrap();
    assert_eq!(
        pipeline.execute(next).unwrap().status,
        WorkflowExecutionStatusV1::Succeeded
    );

    let mut other = scoped_request(
        &pipeline,
        metadata.clone(),
        &project,
        ApprovalMode::AskForApproval,
    );
    other.request_id = stable("command.other-project-grant").unwrap();
    other.run_id = stable("run.other-project-grant").unwrap();
    other.chat_id = stable("chat.other-project-grant").unwrap();
    other.approvals.project_key = Some("different-project".into());
    assert_eq!(
        pipeline.execute(other).unwrap().status,
        WorkflowExecutionStatusV1::AwaitingApproval
    );

    pipeline
        .file_tool_authority
        .approvals
        .revoke(&grants[0].id)
        .unwrap();
    let mut revoked = scoped_request(&pipeline, metadata, &project, ApprovalMode::AskForApproval);
    revoked.request_id = stable("command.revoked-project-grant").unwrap();
    revoked.run_id = stable("run.revoked-project-grant").unwrap();
    revoked.chat_id = stable("chat.revoked-project-grant").unwrap();
    assert_eq!(
        pipeline.execute(revoked).unwrap().status,
        WorkflowExecutionStatusV1::AwaitingApproval
    );
}

#[test]
fn denial_reason_is_model_visible_and_durable() {
    let root = TempDir::new().unwrap();
    let project = edit_approval_project(&root);
    let (pipeline, _, metadata, _, results) = setup_tool_pipeline(&root, ToolScriptV1::Edit);
    let request = scoped_request(&pipeline, metadata, &project, ApprovalMode::AskForApproval);
    let approval = pipeline.execute(request).unwrap().approval.unwrap();
    let reason = "Keep notes.txt unchanged; explain the edit instead.";
    let resolution = ApprovalResolution {
        choice: ApprovalChoice::Deny,
        reason: Some(reason.into()),
    };
    assert_eq!(
        pipeline
            .resume_approval_choice(&approval.decision_id, &resolution)
            .unwrap()
            .status,
        WorkflowExecutionStatusV1::Succeeded
    );
    assert_eq!(
        fs::read_to_string(project.join("notes.txt")).unwrap(),
        "alpha"
    );
    assert!(
        results.lock().unwrap()[0]["detail"]
            .as_str()
            .unwrap()
            .contains(reason)
    );
    assert_eq!(
        pipeline
            .file_tool_authority
            .approvals
            .resolution(&approval.decision_id)
            .unwrap(),
        Some(resolution)
    );
    assert!(
        pipeline
            .resume_approval(&approval.decision_id, true)
            .is_err()
    );
}

#[test]
fn lost_approval_receipt_recovers_without_repeating_provider_or_tool_effects() {
    let root = TempDir::new().unwrap();
    let project = edit_approval_project(&root);
    let (pipeline, _, metadata, calls, _) = setup_tool_pipeline(&root, ToolScriptV1::Edit);
    let request = scoped_request(&pipeline, metadata, &project, ApprovalMode::AskForApproval);
    let approval = pipeline.execute(request).unwrap().approval.unwrap();
    let resolution = ApprovalResolution::once(true);
    let completed = pipeline
        .resume_approval_choice(&approval.decision_id, &resolution)
        .unwrap();
    assert_eq!(completed.status, WorkflowExecutionStatusV1::Succeeded);
    let count = calls.load(Ordering::SeqCst);
    // Simulate the crash window after the broker outcome and before saving the
    // UI-facing result. Recovery must rebuild it from committed evidence only.
    rusqlite::Connection::open(root.path().join("history/aworkit-invocations.sqlite3"))
        .unwrap()
        .execute(
            "DELETE FROM approval_results WHERE decision_id=?1",
            [&approval.decision_id],
        )
        .unwrap();
    let recovered = pipeline
        .resume_approval_choice(&approval.decision_id, &resolution)
        .unwrap();
    assert!(recovered.replayed);
    assert_eq!(recovered.assistant_text, completed.assistant_text);
    assert_eq!(calls.load(Ordering::SeqCst), count);
    assert_eq!(
        fs::read_to_string(project.join("notes.txt")).unwrap(),
        "beta"
    );
}

#[test]
fn projectless_and_workflow_approvals_cannot_create_project_grants() {
    let root = TempDir::new().unwrap();
    let project = edit_approval_project(&root);
    let (pipeline, _, metadata, _, _) = setup_tool_pipeline(&root, ToolScriptV1::Edit);
    let request = edit_approval_request(&pipeline, metadata, &project);
    let approval = pipeline.execute(request).unwrap().approval.unwrap();
    assert!(approval.project_scope.is_none());
    assert!(
        pipeline
            .validate_approval_target(&approval.decision_id, "chat.wrong")
            .is_err()
    );
    assert!(
        pipeline
            .resume_approval_choice(
                &approval.decision_id,
                &ApprovalResolution {
                    choice: ApprovalChoice::AlwaysApproveInProject,
                    reason: None
                }
            )
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(project.join("notes.txt")).unwrap(),
        "alpha"
    );
    assert!(
        pipeline
            .file_tool_authority
            .approvals
            .grants()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn mcp_asks_for_approval_before_entering_the_server() {
    let root = TempDir::new().unwrap();
    let (pipeline, metadata, peer_calls, _, _) =
        setup_mcp_pipeline(&root, ScriptedMcpBehavior::Echo);
    let mut request = mcp_graph_request(&pipeline, metadata);
    request.approvals.mode = ApprovalMode::AskForApproval;
    let approval = pipeline.execute(request).unwrap().approval.unwrap();
    assert_eq!(peer_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        pipeline
            .resume_approval(&approval.decision_id, true)
            .unwrap()
            .status,
        WorkflowExecutionStatusV1::Succeeded
    );
    assert_eq!(peer_calls.load(Ordering::SeqCst), 1);
}
