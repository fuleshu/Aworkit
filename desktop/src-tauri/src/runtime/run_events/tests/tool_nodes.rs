//! Explicit tool nodes must settle without opening an unrelated Agent loop.
use super::*;

#[test]
fn explicit_tool_node_can_finish_the_run_directly_and_after_resume() {
    for restart in [false, true] {
        let committer = ephemeral_semantic_event_committer();
        let mut stream = RunEventStream::new(
            "request.tool".into(),
            "run.tool".into(),
            committer.clone(),
            CancellationToken::default(),
        );
        let mut activity = GraphNodeActivityV1 {
            node_id: "files.1".into(),
            node_type: "tool".into(),
            label: "Write file".into(),
            status: "started".into(),
            summary: "Writing".into(),
            input: None,
            output: None,
            loop_frame: None,
        };
        let call = aworkit_capability_host::ModelToolCallV1 {
            call_id: "files.1.tool".into(),
            provider_call_id: None,
            capability_id: "tool.files.write".into(),
            name: "write_file".into(),
            arguments: json!({"path":"notes.txt","content":"private"}),
            provider_context: None,
        };
        stream.publish_graph_activity(&activity);
        stream.publish_tool_started(&call);
        if restart {
            stream = RunEventStream::new(
                "request.tool".into(),
                "run.tool".into(),
                committer,
                CancellationToken::default(),
            );
        }
        stream.publish_tool_terminal(
            &call,
            "completed",
            "Written".into(),
            json!({"path":"notes.txt"}),
        );
        activity.status = "completed".into();
        stream.publish_graph_activity(&activity);
        stream.terminal_span(
            &stream.run_span_id(),
            "completed",
            "Done".into(),
            None,
            Value::Null,
        );
        stream.ensure_healthy().unwrap();
        let events = stream.events();
        assert!(
            !events
                .iter()
                .any(|event| event.payload["spanKind"] == "agent_loop")
        );
        let tool = events
            .iter()
            .find(|event| {
                event.kind == "span.started"
                    && event.span_id.as_deref() == Some("span.tool.files.1.tool")
            })
            .unwrap();
        assert_eq!(
            tool.payload["parentSpanId"],
            "span.node.run.tool.request.tool.files.1"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "span.started")
                .count(),
            3
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "span.completed")
                .count(),
            3
        );
    }
}

/// An external-agent node owns its delegation call directly, exactly like an
/// explicit tool node. It has no Agent-loop wrapper, so a loop opened for its
/// call could never be settled: the Run span would then be refused at commit
/// with "span 'span.run...' cannot terminate while child 'span.agent-loop...'
/// is open", and a finished delegation could never deliver its answer.
#[test]
fn external_agent_node_settles_without_opening_an_agent_loop() {
    for restart in [false, true] {
        let committer = ephemeral_semantic_event_committer();
        let mut stream = RunEventStream::new(
            "request.review".into(),
            "run.review".into(),
            committer.clone(),
            CancellationToken::default(),
        );
        let mut activity = GraphNodeActivityV1 {
            node_id: "review.1".into(),
            node_type: "external_agent".into(),
            label: "External review".into(),
            status: "started".into(),
            summary: "Delegating the review".into(),
            input: None,
            output: None,
            loop_frame: None,
        };
        let call = aworkit_capability_host::ModelToolCallV1 {
            call_id: "review.1.external".into(),
            provider_call_id: None,
            capability_id: "tool.subagent_codex".into(),
            name: "spawn_codex_subagent".into(),
            arguments: json!({"task":"Review the workspace."}),
            provider_context: None,
        };
        stream.publish_graph_activity(&activity);
        stream.publish_tool_started(&call);
        if restart {
            stream = RunEventStream::new(
                "request.review".into(),
                "run.review".into(),
                committer,
                CancellationToken::default(),
            );
        }
        stream.publish_tool_terminal(
            &call,
            "completed",
            "Reviewed".into(),
            json!({"childId":"child.external.1","answer":"no defects"}),
        );
        activity.status = "completed".into();
        stream.publish_graph_activity(&activity);
        stream.terminal_span(
            &stream.run_span_id(),
            "completed",
            "Done".into(),
            None,
            Value::Null,
        );
        stream.ensure_healthy().unwrap();
        let events = stream.events();
        assert!(
            !events
                .iter()
                .any(|event| event.payload["spanKind"] == "agent_loop"),
            "an external-agent node must not leave an orphan Agent loop"
        );
        let tool = events
            .iter()
            .find(|event| {
                event.kind == "span.started"
                    && event.span_id.as_deref() == Some("span.tool.review.1.external")
            })
            .unwrap();
        assert_eq!(
            tool.payload["parentSpanId"],
            "span.node.run.review.request.review.review.1",
            "the delegation belongs to its own node, not to an Agent loop"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "span.completed")
                .count(),
            3
        );
    }
}
