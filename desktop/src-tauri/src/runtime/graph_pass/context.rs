//! Model-visible facts derived only from the frozen graph and selected bindings.

use super::*;

/// Planning cannot invoke tools. Describe the first reachable Agents' real
/// tool sets so it can plan their work without inventing discovery functions.
/// Stop at Agent and turn boundaries; unrelated branches are not advertised.
pub(super) fn planning_tools(graph: &CompiledGraphPassV1, node_id: &str) -> String {
    let agents = next_agents(graph, node_id);
    let inventory: Vec<_> = agents
        .into_iter()
        .map(|agent| {
            let tools: Vec<_> = agent
                .tool_bindings
                .iter()
                .filter(|binding| binding.is_callable())
                .map(|binding| {
                    let definition = binding.definition();
                    json!({"name":definition.name, "capabilityId":definition.capability_id,
                "description":truncate_utf8(definition.description, 240)})
                })
                .collect();
            json!({"agentNode":agent.id,"tools":tools})
        })
        .collect();
    format!(
        "Tools registered for the next Agent nodes in this workflow:\n{}\n\
        Each Agent can call only its own listed tools; conditional routes may select different Agents. \
        This planning step cannot call tools. Plan using the listed names and purposes. \
        Tool visibility is already established by this inventory and needs no filesystem, database, \
        shell or tool-discovery search. A connection test is a separate tool invocation, if the user requests one.",
        json!(inventory)
    )
}

fn next_agents<'a>(graph: &'a CompiledGraphPassV1, node_id: &str) -> Vec<&'a CompiledGraphNodeV1> {
    let mut visited = BTreeSet::from([node_id]);
    let mut pending = vec![node_id];
    let mut agents = BTreeMap::new();
    while let Some(id) = pending.pop() {
        for edge in graph.edges.iter().filter(|edge| edge.source == id) {
            if !visited.insert(&edge.target) {
                continue;
            }
            let Some(node) = graph.nodes.iter().find(|node| node.id == edge.target) else {
                continue;
            };
            match node.node_type.as_str() {
                "agent" => {
                    agents.insert(node.id.as_str(), node);
                }
                "wait" | "completion" => {}
                _ => pending.push(&node.id),
            }
        }
    }
    agents.into_values().collect()
}

/// Compose the same Agent context for initial execution and approval recovery.
/// Callable aliases are joined to exact capability identities, including full
/// MCP operation names that may be shortened in provider aliases.
pub(super) fn agent_messages(
    node: &CompiledGraphNodeV1,
    upstream: String,
    conversation: &[WorkflowMessageV1],
    project: Option<Value>,
) -> Vec<WorkflowMessageV1> {
    let mut sections = Vec::new();
    if let Some(instructions) = node
        .configuration
        .get("instructions")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        sections.push(instructions.to_owned());
    }
    if !upstream.trim().is_empty() {
        sections.push(format!(
            "Additional context from earlier graph steps:\n{}",
            truncate_utf8(upstream, MAXIMUM_AGENT_CONTEXT_BYTES)
        ));
    }
    if node.tool_bindings.iter().any(|binding| binding.is_callable()) {
        sections.push(
            "The supplied tool definitions are the tools registered for this Agent. \
            Use their exact callable names. Earlier plans do not add tools. \
            Answer tool-visibility questions from these definitions; searching local files, \
            configuration or databases is unnecessary to establish visibility."
                .into(),
        );
    }
    for binding in node.tool_bindings.iter().filter(|binding| binding.is_callable()) {
        let mut section = format!(
            "Tool {} ({}):",
            binding.provider_name, binding.capability_id
        );
        if let Some(instructions) = binding
            .options
            .instructions
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            section.push('\n');
            section.push_str(instructions);
        }
        sections.push(section);
    }
    let mut messages = Vec::new();
    if !sections.is_empty() {
        messages.push(WorkflowMessageV1 {
            role: "system".into(),
            content: sections.join("\n\n"),
            images: Vec::new(),
        });
    }
    if let Some(project) = project {
        messages.push(project_message(project));
    }
    messages.extend_from_slice(conversation);
    merge_system_messages(messages)
}

pub(super) fn project_message(project: Value) -> WorkflowMessageV1 {
    WorkflowMessageV1 {
        role: "system".into(),
        content: format!("Current project selected for this Chat:\n{project}"),
        images: Vec::new(),
    }
}

/// Some OpenAI-compatible chat templates accept only one initial system
/// message. Preserve all leading system text and images in that single slot.
pub(super) fn merge_system_messages(
    mut messages: Vec<WorkflowMessageV1>,
) -> Vec<WorkflowMessageV1> {
    let count = messages
        .iter()
        .take_while(|message| message.role == "system")
        .count();
    if count > 1 {
        let mut parts = Vec::new();
        let mut images = Vec::new();
        for message in messages.drain(..count) {
            parts.push(message.content);
            images.extend(message.images);
        }
        messages.insert(
            0,
            WorkflowMessageV1 {
                role: "system".into(),
                content: parts.join("\n\n"),
                images,
            },
        );
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_and_workflow_context_use_one_initial_system_message() {
        let messages = vec![
            WorkflowMessageV1 {
                role: "system".into(),
                content: "persona".into(),
                images: Vec::new(),
            },
            WorkflowMessageV1 {
                role: "system".into(),
                content: "project context".into(),
                images: Vec::new(),
            },
            WorkflowMessageV1 {
                role: "user".into(),
                content: "question".into(),
                images: Vec::new(),
            },
        ];
        let combined = merge_system_messages(messages.clone());
        assert_eq!(combined.len(), 2);
        assert_eq!(combined[0].content, "persona\n\nproject context");
        assert_eq!(combined[1], messages[2]);
    }

    #[test]
    fn inventory_stops_at_agents_and_turn_boundaries_and_excludes_unrelated_nodes() {
        let node = |id: &str, kind: &str| CompiledGraphNodeV1 {
            id: id.into(),
            node_type: kind.into(),
            label: id.into(),
            configuration: json!({}),
            tool_bindings: Vec::new(),
        };
        let edge = |source: &str, target: &str| CompiledGraphEdgeV1 {
            source: source.into(),
            target: target.into(),
            route: None,
        };
        let graph = CompiledGraphPassV1 {
            nodes: vec![
                node("plan", "model_call"),
                node("route", "condition"),
                node("a", "agent"),
                node("b", "agent"),
                node("later", "agent"),
                node("other", "agent"),
                node("wait", "wait"),
            ],
            edges: vec![
                edge("plan", "route"),
                edge("route", "a"),
                edge("route", "b"),
                edge("a", "later"),
                edge("plan", "wait"),
                edge("wait", "other"),
            ],
            entry_node_id: "plan".into(),
            topological_order: Vec::new(),
        };
        assert_eq!(
            next_agents(&graph, "plan")
                .iter()
                .map(|n| n.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }
}
