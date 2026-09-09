//! MCP policy cases execute through the real broker and persistence boundary.
use super::*;
use crate::runtime::approvals::ApprovalMode;

#[test]
fn mcp_approval_requires_both_explicit_safe_hints_unless_auto_approved() {
    for annotations in [
        None,
        Some(json!({})),
        Some(json!({"readOnlyHint": true})),
        Some(json!({"destructiveHint": false})),
        Some(json!({"readOnlyHint": false, "destructiveHint": false})),
        Some(json!({"readOnlyHint": false, "destructiveHint": true})),
        Some(json!({"readOnlyHint": true, "destructiveHint": true})),
        Some(json!({"readOnlyHint": true, "destructiveHint": false})),
    ] {
        for auto_approve in [false, true] {
            let root = TempDir::new().unwrap();
            let (pipeline, metadata, calls, _, _) =
                setup_mcp_pipeline(&root, ScriptedMcpBehavior::Echo);
            let mut request = mcp_graph_request(&pipeline, metadata);
            request.approvals.mode = ApprovalMode::AskForApproval;
            request.tools[0].options.approval_mode = Some(ApprovalMode::AskForApproval);
            request.tools[0].options.auto_approve = auto_approve;
            if let Some(hints) = &annotations {
                request.tools[0].configuration["annotations"] = hints.clone();
            }
            let free = auto_approve
                || annotations == Some(json!({"readOnlyHint": true, "destructiveHint": false}));
            let result = pipeline.execute(request.clone()).unwrap();
            assert_eq!(
                result.status,
                if free {
                    WorkflowExecutionStatusV1::Succeeded
                } else {
                    WorkflowExecutionStatusV1::AwaitingApproval
                },
                "auto={auto_approve}, hints={annotations:?}: {:?}",
                result.error
            );
            assert_eq!(calls.load(Ordering::SeqCst), usize::from(free));
            if let Some(approval) = result.approval {
                let resumed = pipeline
                    .resume_approval(&approval.decision_id, true)
                    .unwrap();
                assert_eq!(
                    resumed.status,
                    WorkflowExecutionStatusV1::Succeeded,
                    "{:?}",
                    resumed.error
                );
                assert_eq!(calls.load(Ordering::SeqCst), 1);
            } else {
                assert!(pipeline.execute(request).unwrap().replayed);
                assert_eq!(
                    calls.load(Ordering::SeqCst),
                    1,
                    "approved MCP calls never replay effects"
                );
            }
        }
    }
}

#[test]
fn mcp_automatic_paths_skip_the_model_reviewer_and_unsafe_hints_use_standard_policy() {
    for (mode, auto_approve, safe, expected) in [
        (
            ApprovalMode::ApproveForMe,
            true,
            false,
            WorkflowExecutionStatusV1::Succeeded,
        ),
        (
            ApprovalMode::ApproveForMe,
            false,
            true,
            WorkflowExecutionStatusV1::Succeeded,
        ),
        (
            ApprovalMode::FullAccess,
            false,
            false,
            WorkflowExecutionStatusV1::Succeeded,
        ),
        // This fixture has no reviewer response; standard policy must fall back to a person.
        (
            ApprovalMode::ApproveForMe,
            false,
            false,
            WorkflowExecutionStatusV1::AwaitingApproval,
        ),
    ] {
        let root = TempDir::new().unwrap();
        let (pipeline, metadata, calls, _, _) =
            setup_mcp_pipeline(&root, ScriptedMcpBehavior::Echo);
        let mut request = mcp_graph_request(&pipeline, metadata);
        request.approvals.mode = mode;
        request.tools[0].options.auto_approve = auto_approve;
        if safe {
            request.tools[0].configuration["annotations"] =
                json!({"readOnlyHint": true, "destructiveHint": false});
        }
        let result = pipeline.execute(request).unwrap();
        assert_eq!(result.status, expected, "{:?}", result.error);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            usize::from(expected == WorkflowExecutionStatusV1::Succeeded)
        );
    }
}

#[test]
fn malformed_frozen_mcp_hints_cannot_bypass_approval() {
    let root = TempDir::new().unwrap();
    let (pipeline, metadata, calls, _, _) = setup_mcp_pipeline(&root, ScriptedMcpBehavior::Echo);
    for malformed in [
        json!("read-only"),
        json!({"readOnlyHint": "true", "destructiveHint": false}),
        json!({"readOnlyHint": true, "destructiveHint": false, "extra": true}),
    ] {
        let mut request = mcp_graph_request(&pipeline, metadata.clone());
        request.tools[0].configuration["annotations"] = malformed;
        assert!(freeze_file_tool_bindings(&request.tools).is_err());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
