//! Regression for Chats frozen before MCP aliases obeyed provider name limits.
use super::*;

#[test]
fn previously_invalid_frozen_name_can_execute_with_unchanged_authority() {
    let root = TempDir::new().expect("temporary directory");
    let (pipeline, metadata, peer_calls, arguments, results) =
        setup_mcp_pipeline(&root, ScriptedMcpBehavior::Echo);
    let mut request = mcp_graph_request(&pipeline, metadata);
    let invalid_name = "mcp__mcp_166dddff4b6840dba8aed1edbbb9e427__adashi_get_rule_injections";
    request.tools[0].definition.as_mut().unwrap().name = invalid_name.into();
    let frozen = request.tools[0].clone();
    let result = pipeline.execute(request).expect("restored MCP Chat");
    assert_eq!(
        result.status,
        WorkflowExecutionStatusV1::Succeeded,
        "{:?}",
        result.error
    );
    assert_eq!(peer_calls.load(Ordering::SeqCst), 1);
    assert_eq!(arguments.lock().unwrap()[0]["text"], "hello");
    assert_eq!(results.lock().unwrap()[0]["result"]["echo"], "hello");
    assert_eq!(result.tool_activity[0].capability_id, frozen.capability_id);
    assert_eq!(frozen.definition.unwrap().name, invalid_name);
}
