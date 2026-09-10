//! Text-only graph nodes and bounded correction of structured Plan responses.

use super::*;
use crate::runtime::plan_contract::{parse_plan_output_v1, plan_output_instructions_v1};

impl PassMachine<'_> {
    /// Keep both model attempts in the normal evidence, cancellation and budget path.
    /// Only validated Plan JSON is allowed to reach downstream nodes.
    pub(super) fn run_model_call(
        &mut self,
        node: &CompiledGraphNodeV1,
        cancellation: &CancellationToken,
    ) -> Result<Value, String> {
        let instructions = node
            .configuration
            .get("instructions")
            .and_then(Value::as_str)
            .unwrap_or("");
        let is_plan = node
            .configuration
            .get("outputContract")
            .and_then(Value::as_str)
            == Some("plan");
        let mut messages = Vec::new();
        if !instructions.trim().is_empty() {
            messages.push(WorkflowMessageV1 {
                images: Vec::new(),
                role: "system".into(),
                content: instructions.to_owned(),
            });
        }
        if let Some(project) = self.tool_authority.project_context() {
            messages.push(context::project_message(project));
        }
        if is_plan {
            messages.push(WorkflowMessageV1 {
                images: Vec::new(),
                role: "system".into(),
                content: context::planning_tools(self.compiled, &node.id),
            });
            messages.push(WorkflowMessageV1 {
                images: Vec::new(),
                role: "system".into(),
                content: plan_output_instructions_v1(),
            });
        }
        messages.push(WorkflowMessageV1 {
            images: self
                .conversation
                .iter()
                .flat_map(|message| message.images.clone())
                .collect(),
            role: "user".into(),
            content: value_text(&self.incoming_value(&node.id)),
        });
        let messages = context::merge_system_messages(messages);
        let plan = ModelResolutionPlanV1 {
            candidates: vec![ModelCandidateV1 {
                binding_id: self.model_binding_id.to_owned(),
                version_hash: self.model_version_hash.to_owned(),
            }],
            maximum_input_bytes: MAXIMUM_MODEL_CALL_INPUT_BYTES,
            maximum_output_bytes: MAXIMUM_NODE_OUTPUT_BYTES,
        };
        let parameters = node_model_parameters(&node.configuration);
        let mut correction_notice = None;
        loop {
            let evidence = self
                .execute_text_turn(
                    &plan,
                    ModelRequestV1 {
                        input: json!({"messages": messages}),
                        parameters: parameters.clone(),
                    },
                    None,
                    correction_notice.as_deref(),
                    cancellation,
                )
                .map_err(|error| format!("model_call node '{}' failed: {error}", node.id))?;
            let turn = project_model_events(&evidence.events);
            let text = turn.assistant_text;
            self.input_units = self.input_units.saturating_add(turn.input_tokens);
            self.output_units = self.output_units.saturating_add(turn.output_tokens);
            if text.trim().is_empty() {
                return Err(format!(
                    "model_call node '{}' returned no assistant text",
                    node.id
                ));
            }
            if !is_plan {
                return Ok(Value::String(text));
            }
            match parse_plan_output_v1(&text) {
                Ok(output) => return Ok(output),
                Err(error) if correction_notice.is_none() => {
                    // Retry notices survive restoration of the first attempt's
                    // context checkpoint; appended input messages do not.
                    correction_notice = Some(format!(
                        "Your previous Plan response failed validation: {error}. \
                         Generate the plan for the original request again in the required format. {}",
                        plan_output_instructions_v1()
                    ));
                }
                Err(error) => {
                    return Err(format!(
                        "model_call node '{}' violated its plan output contract: {error}",
                        node.id
                    ));
                }
            }
        }
    }
}
