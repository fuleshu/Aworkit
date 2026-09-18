//! Durable stopped-pass checkpoints scoped to their frozen Chat and Run.
use super::*;
use crate::runtime::graph_pass::StoppedGraphPassV1;

impl WorkflowExecutionPipeline {
    pub(crate) fn stopped_node(
        &self,
        request_id: &StableId,
    ) -> Result<Option<String>, WorkflowPipelineError> {
        Ok(self
            .records
            .stopped_pass(request_id)?
            .map(|state| state.node_id))
    }

    pub(super) fn validate_steering(
        &self,
        request: &WorkflowExecutionRequestV1,
    ) -> Result<(), WorkflowPipelineError> {
        let Some(source) = &request.steer_from_request_id else {
            return Ok(());
        };
        let previous = self
            .records
            .execution(source)?
            .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
        let old_config = &previous.worker_proposal.payload["config"]["workflow"];
        if source == &request.request_id
            || previous.snapshot.chat_id != request.chat_id
            || previous.snapshot.run_id != request.run_id
            || old_config != &request.workflow_snapshot
            || !previous
                .snapshot
                .nodes
                .iter()
                .any(|node| node.config["frozenContextHash"] == request.frozen_context_hash)
            || self.records.stopped_pass(source)?.is_none()
        {
            return Err(WorkflowPipelineError::InvalidInput(
                "Stopped continuation does not match this Chat's frozen workflow".into(),
            ));
        }
        Ok(())
    }
}

impl PipelineRecordStore {
    pub(super) fn store_stopped_pass(
        &self,
        request_id: &StableId,
        state: Option<&StoppedGraphPassV1>,
    ) -> Result<(), WorkflowPipelineError> {
        let Some(state) = state else {
            return Ok(());
        };
        self.append_record(
            "pipeline.stopped-pass",
            &digest_id("record.stopped-pass", request_id.as_str())?,
            json!({"requestId":request_id,"state":state}),
        )
    }

    pub(super) fn stopped_pass(
        &self,
        request_id: &StableId,
    ) -> Result<Option<StoppedGraphPassV1>, WorkflowPipelineError> {
        self.events_matching("pipeline.stopped-pass", |record| {
            record["requestId"] == request_id.as_str()
        })?
        .into_iter()
        .last()
        .map(|record| serde_json::from_value(record["state"].clone()).map_err(json_error))
        .transpose()
    }

    /// The proposal is core-authored and already validated before admission.
    /// Recheck ownership on delivery, including crash recovery of that proposal.
    pub(super) fn steering_for(
        &self,
        prepared: &PreparedExecutionRecordV1,
    ) -> Result<Option<StoppedGraphPassV1>, WorkflowPipelineError> {
        let Some(source) = prepared.worker_proposal.payload["steerFromRequestId"].as_str() else {
            return Ok(None);
        };
        let source = stable(source)?;
        let previous = self
            .execution(&source)?
            .ok_or(WorkflowPipelineError::IncompleteEvidence)?;
        if previous.snapshot.chat_id != prepared.snapshot.chat_id
            || previous.snapshot.run_id != prepared.snapshot.run_id
            || previous.worker_proposal.payload["config"]
                != prepared.worker_proposal.payload["config"]
        {
            return Err(WorkflowPipelineError::IncompleteEvidence);
        }
        self.stopped_pass(&source)?
            .map(Some)
            .ok_or(WorkflowPipelineError::IncompleteEvidence)
    }
}
