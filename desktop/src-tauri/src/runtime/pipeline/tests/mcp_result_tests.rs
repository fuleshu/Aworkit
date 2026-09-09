//! Full authority/provider regressions for structured MCP continuation results.
use super::*;

#[test]
fn duplicate_structured_result_reaches_provider_once_and_respects_configured_limit() {
    for maximum_bytes in [512 * 1024, 4096] {
        let root = TempDir::new().unwrap();
        let (pipeline, metadata, calls, _, results) =
            setup_mcp_pipeline(&root, ScriptedMcpBehavior::DuplicatedResult);
        let mut request = mcp_graph_request(&pipeline, metadata);
        request.provider.maximum_tool_output_bytes = maximum_bytes;
        let result = pipeline.execute(request).unwrap();
        assert_eq!(
            result.status,
            WorkflowExecutionStatusV1::Succeeded,
            "{:?}",
            result.error
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(result.tool_activity[0].status, "completed");
        let results = results.lock().unwrap();
        if maximum_bytes > 270_000 {
            assert_eq!(
                results[0]["result"]["structuredContent"]["body"]
                    .as_str()
                    .unwrap()
                    .len(),
                270_000
            );
            assert_eq!(
                results[0]["result"]["structuredContent"]["tail"],
                "complete"
            );
            assert_eq!(results[0]["result"]["content"], json!([]));
        } else {
            assert!(results[0].to_string().len() <= maximum_bytes);
            assert_eq!(results[0]["aworkitOutput"]["truncated"], true);
            assert_eq!(results[0]["preview"]["result"]["structuredContent"]["tail"], "complete");
            assert_eq!(results[0]["preview"]["result"]["content"], json!([]));
        }
    }
}

#[test]
fn server_error_remains_a_failed_tool_call_after_projection() {
    let root = TempDir::new().unwrap();
    let (pipeline, metadata, calls, _, results) =
        setup_mcp_pipeline(&root, ScriptedMcpBehavior::ToolError);
    let result = pipeline
        .execute(mcp_graph_request(&pipeline, metadata))
        .unwrap();
    assert_eq!(result.status, WorkflowExecutionStatusV1::Succeeded);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(result.tool_activity[0].status, "failed");
    assert_eq!(results.lock().unwrap()[0]["result"]["isError"], true);
}
