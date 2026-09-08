//! Durable fresh-result projection and Context tool executor. Nested under
//! tool_loop to reuse the exact broker and immutable Chat authority.
use super::*;
use crate::runtime::{compaction, model_tool_loop::AgentContextV1};
use aworkit_capability_host::context_compression::{self as compression, Mode};
#[path = "archive_search.rs"]
mod archive_search;

pub(super) fn schema() -> Value {
    crate::runtime::tool_registry::native_tool("tool.context")
        .expect("bundled context tool")
        .input_schema
        .clone()
}

fn owner_key(context: &FrozenFileToolAuthorityContextV1) -> String {
    compaction::hash(&json!({"chat":context.chat_id,"branch":context.project_branch}))
}

fn same_owner(a: &Value, b: &Value) -> bool {
    a["ownerKey"] == b["ownerKey"] && a["nodeId"] == b["nodeId"] && a["child"] == b["child"]
}

impl BoundFileToolAuthorityV1 {
    /// The model cannot select a retrieval owner. The provider request's core
    /// scope is committed before any proposal from that request can execute.
    pub(super) fn register_compression_scope(
        &self,
        owner: &AgentContextV1,
        outer: &StableId,
        request: &aworkit_capability_host::ModelToolRequestV1,
    ) -> Result<(), String> {
        if self.context.model_context["policy"]
            .get("compression")
            .is_none()
        {
            return Ok(());
        }
        let query = request.input["messages"]
            .as_array()
            .and_then(|messages| messages.iter().rev().find(|m| m["role"] == "user"))
            .map(|m| compression::render(&m["content"]))
            .unwrap_or_default()
            .chars()
            .take(8192)
            .collect::<String>();
        let scope = json!({"ownerKey":owner_key(&self.context),"nodeId":owner.node_id,"child":owner.child,"outer":outer,"query":query,
            "retrieval":request.tools.iter().any(|t|t.capability_id=="tool.context")});
        let events = self.run_events.context_events()?;
        if let Some(prior) = events
            .iter()
            .find(|e| e.kind == "context.compression-scope" && e.payload["outer"] == json!(outer))
        {
            if !same_owner(&scope, &prior.payload)
                || scope["retrieval"] != prior.payload["retrieval"]
            {
                return Err("Compression invocation scope changed".into());
            }
        } else {
            self.run_events
                .context_event("context.compression-scope", scope)?;
        }
        Ok(())
    }

    pub(super) fn compress_settlement(
        &self,
        outer: &StableId,
        call: &ModelToolCallV1,
        mut settled: SettledModelToolCallV1,
        cancellation: &CancellationToken,
    ) -> Result<SettledModelToolCallV1, WorkflowPipelineError> {
        let value = &self.context.model_context["policy"]["compression"];
        if value.is_null()
            || settled.result.is_error
            || matches!(
                call.capability_id.as_str(),
                "tool.context"
                    | "tool.skill"
                    | "tool.workspace_instructions"
                    | "tool.todo"
                    | "tool.files.edit"
                    | "tool.files.write"
            )
        {
            return Ok(settled);
        }
        let mut policy: compression::Policy =
            serde_json::from_value(value.clone()).map_err(|e| invalid_tool(&e.to_string()))?;
        policy.validate().map_err(|e| invalid_tool(&e))?;
        if policy.mode == Mode::Off || policy.excluded_tools.contains(&call.capability_id) {
            return Ok(settled);
        }
        let events = self
            .run_events
            .context_events()
            .map_err(|e| invalid_tool(&e))?;
        let Some(scope) = events.iter().find(|e| {
            e.kind == "context.compression-scope"
                && e.payload["outer"] == json!(outer)
                && e.payload["ownerKey"] == owner_key(&self.context)
        }) else {
            return Ok(settled);
        };
        let key=compaction::hash(&json!({"scope":scope.payload,"invocation":settled.activity.invocation_id,"original":settled.result.content,"policy":policy})).trim_start_matches("sha256:").to_owned();
        if let Some(prior) = events
            .iter()
            .find(|e| e.kind == "context.compression" && e.payload["reference"] == key)
        {
            settled.result.content = prior.payload["projection"].clone();
            return Ok(settled);
        }
        if events
            .iter()
            .any(|e| e.kind == "context.compression-skipped" && e.payload["reference"] == key)
        {
            return Ok(settled);
        }
        let retrieval = scope.payload["retrieval"] == true;
        // Repeated retrieval is evidence of overcompression for this owner and
        // capability, not permission to relearn other Chats' preferences.
        let archives: Vec<_> = events
            .iter()
            .filter(|e| {
                e.kind == "context.compression"
                    && same_owner(&scope.payload, &e.payload)
                    && e.payload["capabilityId"] == call.capability_id
            })
            .collect();
        let requested: BTreeSet<_> = events
            .iter()
            .filter(|e| e.kind == "context.retrieved" && same_owner(&scope.payload, &e.payload))
            .filter_map(|e| e.payload["reference"].as_str())
            .collect();
        let retrieved = archives
            .iter()
            .filter(|a| {
                a.payload["reference"]
                    .as_str()
                    .is_some_and(|r| requested.contains(r))
            })
            .count();
        let backoff = archives.len() >= 3 && retrieved * 3 >= archives.len();
        if backoff {
            policy.mode = Mode::Lossless;
        }
        let question = scope.payload["query"].as_str().unwrap_or("");
        let query = format!("{} {}", question, call.arguments);
        let path = call.arguments["path"].as_str().unwrap_or("");
        if cancellation.is_cancelled() {
            return Err(invalid_tool("Compression cancelled"));
        }
        let compressed = compression::compress(
            &settled.result.content,
            &query,
            path,
            &policy,
            retrieval.then_some(key.as_str()),
            self.context.maximum_tool_output_bytes,
        );
        if cancellation.is_cancelled() {
            return Err(invalid_tool("Compression cancelled"));
        }
        if let Some(result) = compressed {
            let payload = json!({"ownerKey":scope.payload["ownerKey"],"nodeId":scope.payload["nodeId"],"child":scope.payload["child"],"reference":key,"outer":outer,
                "capabilityId":call.capability_id,"invocationId":settled.activity.invocation_id,"callId":call.call_id,
                "original":settled.result.content,"originalHash":compaction::hash(&settled.result.content),"projection":result.content,"metrics":result.metrics,"backoff":backoff,"retrievable":retrieval});
            self.run_events
                .context_event("context.compression", payload)
                .map_err(|e| invalid_tool(&e))?;
            settled.result.content = result.content;
        } else {
            self.run_events.context_event("context.compression-skipped",json!({"ownerKey":scope.payload["ownerKey"],"nodeId":scope.payload["nodeId"],"child":scope.payload["child"],"reference":key,"capabilityId":call.capability_id,"reason":"No eligible representation met the size and token savings gates within the output bound.","backoff":backoff,"adaptiveAvailable":retrieval})).map_err(|e|invalid_tool(&e))?;
        }
        Ok(settled)
    }
}

impl FileToolDispatcherV1 {
    pub(super) fn retrieve_context(
        &self,
        maximum_bytes: usize,
        cancellation: &CancellationToken,
    ) -> Result<(Value, String), String> {
        let request = compression::retrieval::Request::parse(&self.record.call.arguments)?;
        if cancellation.is_cancelled() {
            return Err("Context retrieval cancelled".into());
        }
        let events = self.run_events.context_events()?;
        let scope = events
            .iter()
            .find(|e| {
                e.kind == "context.compression-scope"
                    && e.payload["outer"] == json!(self.record.outer_invocation_id)
                    && e.payload["ownerKey"] == owner_key(&self.context)
                    && e.payload["retrieval"] == true
            })
            .ok_or("No admitted context retrieval scope")?;
        let archives: Vec<_> = events
            .iter()
            .filter(|e| {
                e.kind == "context.compression"
                    && same_owner(&scope.payload, &e.payload)
                    && e.payload["retrievable"] == true
            })
            .collect();
        if request.operation == "search" && request.reference.is_none() {
            let result = archive_search::search(
                &archives,
                &request,
                maximum_bytes.min(self.context.maximum_tool_output_bytes),
            )?;
            if cancellation.is_cancelled() {
                return Err("Context retrieval cancelled".into());
            }
            self.run_events.context_event("context.retrieved",json!({"ownerKey":scope.payload["ownerKey"],"nodeId":scope.payload["nodeId"],"child":scope.payload["child"],"operation":"search","returnedBytes":result.to_string().len(),"invocationId":self.record.proposal.proposal_id}))?;
            return Ok((result, "Searched original context archive.".into()));
        }
        if request.operation == "stats" {
            let before: u64 = archives
                .iter()
                .filter_map(|e| e.payload["metrics"]["beforeBytes"].as_u64())
                .sum();
            let after: u64 = archives
                .iter()
                .filter_map(|e| e.payload["metrics"]["afterBytes"].as_u64())
                .sum();
            let reads = events
                .iter()
                .filter(|e| e.kind == "context.retrieved" && same_owner(&scope.payload, &e.payload))
                .count();
            let result = json!({"compressedResults":archives.len(),"originalBytes":before,"projectedBytes":after,"bytesSaved":before.saturating_sub(after),"retrievalCalls":reads,"basis":"local representations; not billing"});
            if result.to_string().len() > maximum_bytes.min(self.context.maximum_tool_output_bytes)
            {
                return Err("Compression statistics exceed the configured result size".into());
            }
            return Ok((result, "Read context compression statistics.".into()));
        }
        let archive = archives
            .into_iter()
            .find(|e| e.payload["reference"] == json!(request.reference))
            .ok_or("Context reference is unavailable in this node and child scope")?;
        if archive.payload["originalHash"] != compaction::hash(&archive.payload["original"]) {
            return Err("Context original integrity check failed".into());
        }
        let result = compression::retrieval::retrieve(
            &archive.payload["original"],
            &request,
            maximum_bytes.min(self.context.maximum_tool_output_bytes),
        )?;
        if cancellation.is_cancelled() {
            return Err("Context retrieval cancelled".into());
        }
        self.run_events.context_event("context.retrieved",json!({"ownerKey":scope.payload["ownerKey"],"nodeId":scope.payload["nodeId"],"child":scope.payload["child"],"reference":request.reference,"operation":request.operation,"returnedBytes":result.to_string().len(),"invocationId":self.record.proposal.proposal_id}))?;
        Ok((result, "Retrieved original context evidence.".into()))
    }
}
