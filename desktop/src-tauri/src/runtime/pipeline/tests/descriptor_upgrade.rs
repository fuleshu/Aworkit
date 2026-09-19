//! Continuation must preserve the old manifest all the way through host dispatch.
use super::*;
use crate::runtime::tool_loop::subagent::compatibility;

#[test]
fn continuation_uses_saved_configuration_without_catalog_or_compiler_resolution() {
    let root = TempDir::new().unwrap();
    let (mut pipeline, _, metadata, calls, _) = setup(&root, ScriptedBehavior::Succeed);
    let first = request(metadata);
    let completed = pipeline.execute(first.clone()).unwrap();
    assert_eq!(completed.status, WorkflowExecutionStatusV1::Succeeded);
    let original = pipeline
        .records
        .execution(&first.request_id)
        .unwrap()
        .unwrap();

    pipeline.file_tool_descriptors.clear();
    let mut followup = first.clone();
    followup.request_id = stable("command.catalog-independent-continuation").unwrap();
    followup.messages.push(WorkflowMessageV1 {
        role: "user".into(),
        content: "Continue with the saved configuration".into(),
        images: vec![],
    });
    // These echoes would fail parsing, compilation and freezing if the
    // continuation path still tried to rebuild the existing Chat.
    followup.provider.kind = "removed-from-catalog".into();
    followup.workflow_snapshot = json!({"obsoleteEditorFormat": true});
    followup.frozen_context_hash = "obsolete-context-hash".into();
    followup.budget.turns = 0;
    let (prepared, replay) = pipeline.validated_prepared(&followup).unwrap();
    assert!(!replay);
    assert!(prepared.same_frozen_run(&original));
    assert_ne!(
        prepared.worker_proposal.invocation_id,
        original.worker_proposal.invocation_id
    );
    assert_eq!(
        prepared.worker_proposal.payload["context"]["messages"],
        json!(followup.messages)
    );
    assert_eq!(
        prepared.worker_proposal.payload["config"],
        original.worker_proposal.payload["config"]
    );
    let continued = pipeline.execute(followup.clone()).unwrap();
    assert_eq!(continued.status, WorkflowExecutionStatusV1::Succeeded);
    assert_eq!(continued.snapshot_hash, completed.snapshot_hash);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(pipeline.execute(followup.clone()).unwrap().replayed);
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // Deduplication still distinguishes actual input and Chat ownership.
    let mut different_input = followup.clone();
    different_input.messages.last_mut().unwrap().content = "A different command".into();
    assert!(pipeline.preflight(&different_input).is_err());
    let mut another_chat = followup;
    another_chat.chat_id = stable("chat.another").unwrap();
    assert!(pipeline.preflight(&another_chat).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn subagent_descriptor_upgrade_preserves_continuation_approval_dispatch_and_replay() {
    // Both pre-upgrade Chats and Chats already created by the updated app work.
    for legacy in [true, false] {
        let root = TempDir::new().unwrap();
        let project = subagent_project(&root);
        let (mut old_pipeline, credentials, metadata, calls, observed) =
            setup_tool_pipeline(&root, ToolScriptV1::Subagent);
        if legacy {
            let old = compatibility::legacy_descriptor(
                &old_pipeline.file_tool_descriptors[SUBAGENT_CAPABILITY_ID],
            )
            .unwrap();
            // Real persisted fingerprint from the affected Chat, not a fresh
            // snapshot compiled by the same code under test.
            assert_eq!(
                old.version_hash,
                "sha256:5f8aeb73a7575cf592313774883b222c556ab582d137ab2991ddb25d6d343e7e"
            );
            old_pipeline
                .file_tool_descriptors
                .insert(SUBAGENT_CAPABILITY_ID.into(), old);
        }
        let mut first = subagent_request(
            &old_pipeline,
            metadata,
            &project,
            &[
                FILE_READ_CAPABILITY_ID,
                FILE_SEARCH_CAPABILITY_ID,
                SUBAGENT_CAPABILITY_ID,
            ],
        );
        first.project_branch = Some("main".into());
        let protocol = ProviderProtocolV1::parse(&first.provider.kind).unwrap();
        let original = old_pipeline
            .prepare(&first, protocol, &old_pipeline.descriptors[&protocol], None)
            .unwrap();
        old_pipeline.records.record_execution(&original).unwrap();
        drop(old_pipeline);

        let pipeline = WorkflowExecutionPipeline::compose(
            root.path(),
            credentials,
            Arc::new(ToolProviderFactoryV1 {
                calls: calls.clone(),
                script: ToolScriptV1::Subagent,
                observed_results: observed.clone(),
            }),
        )
        .unwrap();
        let mut followup = first.clone();
        followup.request_id = stable("command.continue-old-subagent").unwrap();
        followup.messages.push(WorkflowMessageV1 {
            role: "user".into(),
            content: "Continue".into(),
            images: vec![],
        });
        let (prepared, _) = pipeline
            .validated_prepared(&followup)
            .expect("existing Chat continues after upgrade");
        assert!(prepared.same_frozen_run(&original));
        assert_eq!(calls.load(Ordering::SeqCst), 0, "preflight has no effects");

        let suspended = pipeline.execute(followup.clone()).unwrap();
        assert_eq!(
            suspended.status,
            WorkflowExecutionStatusV1::AwaitingApproval,
            "{:?}",
            suspended.error
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "child still requires the original approval"
        );
        let approval = suspended.approval.unwrap();
        let done = pipeline
            .resume_approval(&approval.decision_id, true)
            .unwrap();
        assert_eq!(
            done.status,
            WorkflowExecutionStatusV1::Succeeded,
            "{:?}",
            done.error
        );
        assert_eq!(
            done.tool_activity[0].status, "completed",
            "{:?}",
            done.tool_activity
        );
        assert_eq!(observed.lock().unwrap()[0]["content"], "alpha beta alpha");
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        let saved = pipeline
            .records
            .execution(&followup.request_id)
            .unwrap()
            .unwrap();
        assert!(
            saved.same_frozen_run(&original),
            "execution retains the original manifest and tool contract"
        );
        assert!(pipeline.execute(followup.clone()).unwrap().replayed);
        assert_eq!(calls.load(Ordering::SeqCst), 4, "no repeated child effects");

        // Echoed configuration cannot expand permission or block continuation.
        for reuse_command in [true, false] {
            let mut changed = followup.clone();
            if !reuse_command {
                changed.request_id = stable("command.changed-subagent-mode").unwrap();
            }
            changed
                .tools
                .iter_mut()
                .find(|t| t.capability_id == SUBAGENT_CAPABILITY_ID)
                .unwrap()
                .configuration["inheritParentTools"] = json!(true);
            let (kept, _) = pipeline.validated_prepared(&changed).unwrap();
            assert!(
                kept.same_frozen_run(&original),
                "must not expand old child authority"
            );
        }
    }
}

#[test]
fn subagent_descriptor_compatibility_does_not_accept_unknown_hashes_or_other_contracts() {
    let root = TempDir::new().unwrap();
    let project = subagent_project(&root);
    let (pipeline, _, metadata, _, _) = setup_tool_pipeline(&root, ToolScriptV1::Subagent);
    let request = subagent_request(&pipeline, metadata, &project, &[SUBAGENT_CAPABILITY_ID]);
    let mut binding = freeze_file_tool_bindings(&request.tools).unwrap().remove(0);
    let current = &pipeline.file_tool_descriptors[SUBAGENT_CAPABILITY_ID];
    let legacy = compatibility::legacy_descriptor(current).unwrap();
    assert!(compatibility::accepts_legacy(
        &binding,
        current,
        &legacy.version_hash
    ));
    assert!(!compatibility::accepts_legacy(
        &binding,
        current,
        &format!("sha256:{}", "f".repeat(64))
    ));
    let mut changed_executor = current.clone();
    changed_executor.max_output_bytes += 1;
    changed_executor.rehash().unwrap();
    assert!(!compatibility::accepts_legacy(
        &binding,
        &changed_executor,
        &legacy.version_hash
    ));
    binding.limit = crate::runtime::tool_loop::StoredFileToolLimitV1::Subagent {
        inherit_parent_tools: true,
        legacy_maximum_turns: None,
    };
    assert!(!compatibility::accepts_legacy(
        &binding,
        current,
        &legacy.version_hash
    ));
}
