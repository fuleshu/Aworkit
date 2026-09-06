//! Migrate only exact shipped personas, preserving user-authored instructions.

use serde_json::Value;

const LEGACY: &[&str] = &[
    "You are Aworkit's standard agent. Keep the todo list current, inspect project evidence with the file tools, use web_search and web_fetch when current information is required, and produce a final answer with citations. Do not claim tool results you did not receive.",
    "You are Aworkit's standard agent. Keep the todo list current and inspect project evidence with the file tools. Use web_search for discovery. When its freshness evidence requires extraction—or before making any current price, availability, news, score, weather, schedule, or other live-data claim—call web_extract on the best candidate URLs and base the claim on the live extracted pages. Produce a final answer with citations and do not claim tool results you did not receive.",
];

pub(crate) fn migrate_persona(workflow: &mut Value) -> bool {
    let bundled: Value =
        serde_json::from_str(include_str!("../../../../workflows/default-workflows.json"))
            .expect("bundled workflows");
    let persona = bundled["workflows"]
        .as_array()
        .and_then(|templates| {
            templates
                .iter()
                .filter_map(|template| template.get("document"))
                .find(|document| document["id"] == "workflow.standard-agent")
        })
        .and_then(|document| document["nodes"].as_array())
        .and_then(|nodes| nodes.iter().find(|node| node["id"] == "agent.1"))
        .and_then(|node| node["configuration"]["instructions"].as_str())
        .expect("default agent persona");
    let mut changed = false;
    if let Some(nodes) = workflow["nodes"].as_array_mut() {
        for node in nodes {
            if node["type"] == "agent"
                && node["configuration"]["instructions"]
                    .as_str()
                    .is_some_and(|text| LEGACY.contains(&text))
            {
                node["configuration"]["instructions"] = persona.into();
                changed = true;
            }
        }
    }
    changed
}
