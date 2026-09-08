//! Automatic Agent contribution. Preparation is serialized and admitted through
//! the existing core history store. No listeners, background tasks or text cache
//! survive disposal; restart reconstructs everything from immutable records.

use super::*;
use aworkit_capability_host::{ModelToolContextV1, ModelToolRequestV1,
    workspace_instructions::{self as library, Configuration, Event, Owner, Preparation, ProjectInstructionFiles, Selection}};
use crate::runtime::model_tool_loop::AgentContextV1;

const KIND: &str = "pipeline.workspace-instructions";
const ID: &str = "tool.workspace_instructions";

#[cfg(test)]
mod tests;

pub(crate) fn configuration(value: &Value) -> Result<Configuration, String> {
    let config: Configuration = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    config.resolve(home.as_deref())
}

/// Positions reference the original message once, instead of archiving copies.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Position {
    id: String,
    after_exchanges: usize,
    after_input_messages: Option<usize>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Step {
    owner: Owner,
    outer: StableId,
    after_exchanges: usize,
    revision: String,
    /// Number of ordinary conversation messages when this observation occurred.
    after_conversation: usize,
    event: Option<Event>,
    positions: Vec<Position>,
    diagnostics: Vec<String>,
}

impl BoundFileToolAuthorityV1 {
    /// Inherit this contribution only from the Agent that selected it, never
    /// from an unrelated node in the Run's combined frozen binding catalog.
    pub(super) fn child_instruction_context(&self, invocation: &StableId) -> Result<AgentContextV1, String> {
        let invocations: Vec<ToolInvocationRecordV1> = self.runtime.records.events("pipeline.tool-invocation-prepared")
            .map_err(|e| e.to_string())?.into_iter().map(serde_json::from_value).collect::<Result<_, _>>().map_err(|e| e.to_string())?;
        let mut parent = None;
        for record in &invocations {
            if self.runtime.ledger.invocation_for_proposal(&record.proposal.proposal_id).map_err(|e| e.to_string())?.as_ref() == Some(invocation) { parent = Some(record); break; }
        }
        let steps: Vec<Step> = self.runtime.records.events(KIND).map_err(|e| e.to_string())?.into_iter()
            .map(serde_json::from_value).collect::<Result<_, _>>().map_err(|e| e.to_string())?;
        let selected = parent.as_ref().and_then(|parent| steps.iter().rev().find(|s| s.outer == parent.outer_invocation_id));
        Ok(AgentContextV1 { node_id: selected.map_or_else(|| self.context.node_id.to_string(), |s| s.owner.node.clone()),
            tool_ids: self.context.bindings.iter().filter(|b| SUBAGENT_CHILD_TOOL_IDS.contains(&b.capability_id.as_str())
                && (b.is_callable() || selected.is_some())).map(|b| b.capability_id.clone()).collect(),
            child: Some(invocation.to_string()) })
    }

    /// Called after context selection, for text-only and tool-calling Agents.
    pub(super) fn workspace_context(&self, outer: &StableId, after_exchanges: usize,
        agent: &AgentContextV1, request: &mut ModelToolRequestV1, cancellation: &CancellationToken) -> Result<(), String>
    {
        if !agent.tool_ids.iter().any(|id| id == ID) { return Ok(()); }
        let binding = self.context.bindings.iter().find(|b| b.capability_id == ID).ok_or("missing frozen Workspace Instructions binding")?;
        let StoredFileToolLimitV1::WorkspaceInstructions { configuration } = &binding.limit else { return Err("invalid automatic context binding".into()); };
        if cancellation.is_cancelled() || self.context.cancellation.is_cancelled() { return Err("instruction preparation cancelled".into()); }
        let owner = Owner { chat: self.context.approvals.chat_id.clone(), node: agent.node_id.clone(),
            context: format!("{}:{}", self.context.project_branch.as_deref().unwrap_or(""), agent.child.as_deref().unwrap_or("")) };
        let revision = if agent.child.is_some() { "ordinary".into() } else { self.run_events.instruction_context_revision(&agent.node_id)? };
        let _guard = self.runtime.records.instruction_lock.lock().map_err(|_| "instruction preparation lock poisoned")?;
        let steps: Vec<Step> = self.runtime.records.events(KIND).map_err(|e| e.to_string())?.into_iter()
            .map(serde_json::from_value).collect::<Result<Vec<Step>, _>>().map_err(|e| e.to_string())?
            .into_iter().filter(|s| s.owner == owner).collect();
        let history: Vec<Event> = steps.iter().filter_map(|s| s.event.clone()).collect();
        if let Some(step) = steps.iter().find(|s| s.outer == *outer && s.after_exchanges == after_exchanges && s.revision == revision) {
            return project(request, &step.positions, &history);
        }
        let mut positions = Vec::new();
        // An edited context carries its actual surviving references. Validate both
        // identity and exact bytes; copied headings or altered bodies are not proof.
        for message in &request.context_messages {
            if let Some(id) = &message.instruction_event_id {
                if history.iter().any(|e| e.id == *id && e.text == message.content && message.role.as_deref().is_none_or(|r| r == "user") && message.images.is_empty()) {
                    positions.push(Position { id: id.clone(), after_exchanges: message.after_exchanges, after_input_messages: message.after_input_messages });
                }
            }
        }
        let input = request.input["messages"].as_array().ok_or("Agent context requires input.messages")?;
        let system_count = input.iter().take_while(|m| m["role"] == "system").count();
        let after_conversation = self.context.review_messages.iter().filter(|m| m.role != "system").count();
        for step in &steps {
            let Some(event) = &step.event else { continue; };
            if positions.iter().any(|p| p.id == event.id) { continue; }
            if revision != "ordinary" && step.revision != revision { continue; }
            let same_input = step.outer == *outer;
            let position = if same_input {
                Position { id: event.id.clone(), after_exchanges: step.after_exchanges + request.exchanges.len().saturating_sub(after_exchanges), after_input_messages: None }
            } else if revision == "ordinary" {
                Position { id: event.id.clone(), after_exchanges: 0, after_input_messages: Some((system_count + step.after_conversation).min(input.len())) }
            } else {
                Position { id: event.id.clone(), after_exchanges: request.exchanges.len().saturating_sub(after_exchanges), after_input_messages: None }
            };
            positions.push(position);
        }
        let selection = Selection { revision: revision.clone(), event_ids: positions.iter().map(|p| p.id.clone()).collect() };
        let workspace = self.context.approvals.project_key.as_ref().map(|_| self.context.workspace.root.as_path());
        let validation = self.runtime.projects.revalidate_workspace_v1(&self.context.workspace).map_err(|e| e.to_string());
        let mut roots = vec![configuration.aworkit_home.clone()];
        if let Some(workspace) = workspace { roots.push(workspace.to_owned()); }
        let files = ProjectInstructionFiles::new(roots);
        let settled_touches = self.settled_instruction_touches(outer, after_exchanges)?;
        let event_id = digest_id("record.workspace-instructions", &format!("{}:{}:{outer}:{after_exchanges}:{revision}", owner.node, owner.context)).map_err(|e| e.to_string())?;
        let mut plan = library::prepare(Preparation { configuration, owner: &owner, workspace, cwd: workspace,
            files: validation.is_ok().then_some(&files as &dyn library::InstructionFiles), history: &history,
            selection: &selection, event_id: event_id.as_str(), settled_touches: &settled_touches,
            refresh: after_exchanges == 0, cancellation })?;
        if let Err(error) = validation { plan.diagnostics.push(error); }
        if let Some(event) = &plan.event {
            positions.push(Position { id: event.id.clone(), after_exchanges: request.exchanges.len(), after_input_messages: None });
        }
        if cancellation.is_cancelled() || self.context.cancellation.is_cancelled() { return Err("instruction preparation cancelled".into()); }
        let step = Step { owner, outer: outer.clone(), after_exchanges, revision, after_conversation,
            event: plan.event, positions, diagnostics: plan.diagnostics };
        let value = serde_json::to_value(&step).map_err(|e| e.to_string())?;
        self.runtime.records.append(KIND, &event_id, value).map_err(|e| e.to_string())?;
        self.run_events.publish_instruction_context(&agent.node_id, &serde_json::to_value(&step.event).map_err(|e| e.to_string())?, &step.diagnostics)?;
        let mut admitted = history;
        if let Some(event) = step.event.clone() { admitted.push(event); }
        project(request, &step.positions, &admitted)
    }

    /// Read only committed exchanges and canonical outcomes. Descendant successes
    /// are retained even if their composite later fails, but not before it settles.
    fn settled_instruction_touches(&self, outer: &StableId, through: usize) -> Result<Vec<PathBuf>, String> {
        if through == 0 { return Ok(Vec::new()); }
        let exchanges = self.runtime.records.events("pipeline.model-tool-exchange").map_err(|e| e.to_string())?;
        let calls: BTreeSet<String> = exchanges.iter().filter(|e| e["outerInvocationId"] == outer.as_str())
            .filter(|e| e["turn"].as_u64().is_some_and(|t| (t as usize) == through))
            .filter_map(|e| e["exchange"]["assistantContent"].as_array()).flatten()
            .filter_map(|part| part["call"]["callId"].as_str().map(str::to_owned)).collect();
        let invocations: Vec<ToolInvocationRecordV1> = self.runtime.records.events("pipeline.tool-invocation-prepared")
            .map_err(|e| e.to_string())?.into_iter().map(serde_json::from_value).collect::<Result<_, _>>().map_err(|e| e.to_string())?;
        let outcomes: Vec<ToolOutcomeRecordV1> = self.runtime.records.events("pipeline.tool-outcome")
            .map_err(|e| e.to_string())?.into_iter().map(serde_json::from_value).collect::<Result<_, _>>().map_err(|e| e.to_string())?;
        let mut identities = Vec::new();
        for invocation in &invocations {
            if let Some(id) = self.runtime.ledger.invocation_for_proposal(&invocation.proposal.proposal_id).map_err(|e| e.to_string())? {
                identities.push((id, invocation));
            }
        }
        let mut accepted: BTreeSet<_> = identities.iter().filter(|(_, i)| i.outer_invocation_id == *outer && calls.contains(&i.call.call_id)).map(|(id, _)| id.clone()).collect();
        loop {
            let before = accepted.len();
            for (id, invocation) in &identities {
                if accepted.contains(&invocation.outer_invocation_id) { accepted.insert(id.clone()); }
            }
            if accepted.len() == before { break; }
        }
        Ok(identities.iter().filter(|(id, i)| accepted.contains(id)
            && matches!(i.binding.limit, StoredFileToolLimitV1::Read { .. } | StoredFileToolLimitV1::Write { .. } | StoredFileToolLimitV1::Edit { .. }))
            .filter(|(id, _)| outcomes.iter().any(|o| o.invocation_id == *id && !o.is_error))
            .filter_map(|(_, i)| i.call.arguments["path"].as_str().map(PathBuf::from)).collect())
    }
}

fn project(request: &mut ModelToolRequestV1, positions: &[Position], history: &[Event]) -> Result<(), String> {
    for message in &mut request.context_messages {
        if message.instruction_event_id.as_ref().is_some_and(|id| !history.iter().any(|e| e.id == *id && e.text == message.content)) {
            message.instruction_event_id = None;
        }
    }
    request.context_messages.retain(|m| m.instruction_event_id.is_none());
    let mut seen = BTreeSet::new();
    for position in positions {
        if !seen.insert(&position.id) { continue; }
        let event = history.iter().find(|e| e.id == position.id).ok_or("instruction context references an uncommitted event")?;
        if event.text.is_empty() { continue; }
        request.context_messages.push(ModelToolContextV1 { after_exchanges: position.after_exchanges,
            after_input_messages: position.after_input_messages, instruction_event_id: Some(event.id.clone()),
            content: event.text.clone(), ..Default::default() });
    }
    Ok(())
}
