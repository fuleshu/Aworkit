//! Strict structured output contract for workflow Plan model nodes.

use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAXIMUM_PLAN_ITEMS: usize = 16;
const MAXIMUM_PLAN_TEXT_BYTES: usize = 8 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PlanOutputV1 {
    goal: String,
    open_questions: Vec<String>,
    evidence_needed: Vec<String>,
    tool_order: Vec<String>,
}

pub(crate) fn parse_plan_output_v1(text: &str) -> Result<Value, String> {
    let json = strip_json_fence(text.trim());
    let plan: PlanOutputV1 = serde_json::from_str(json).map_err(|_| {
        "expected only a JSON object with goal, openQuestions, evidenceNeeded, and toolOrder"
            .to_owned()
    })?;
    validate_text("goal", &plan.goal)?;
    if plan.tool_order.is_empty() {
        return Err("toolOrder must contain at least one intended action".to_owned());
    }
    for (label, items) in [
        ("openQuestions", &plan.open_questions),
        ("evidenceNeeded", &plan.evidence_needed),
        ("toolOrder", &plan.tool_order),
    ] {
        if items.len() > MAXIMUM_PLAN_ITEMS {
            return Err(format!(
                "{label} exceeds the {MAXIMUM_PLAN_ITEMS}-item bound"
            ));
        }
        for item in items {
            validate_text(label, item)?;
        }
    }
    serde_json::to_value(plan).map_err(|_| "validated plan could not be encoded".to_owned())
}

fn strip_json_fence(text: &str) -> &str {
    text.strip_prefix("```json")
        .and_then(|body| body.strip_suffix("```"))
        .map_or(text, str::trim)
}

fn validate_text(label: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > MAXIMUM_PLAN_TEXT_BYTES || value.contains('\0') {
        return Err(format!(
            "{label} entries must be non-empty and at most {} KiB",
            MAXIMUM_PLAN_TEXT_BYTES / 1024
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_exact_plan_contract_and_normalizes_fences() {
        let output = parse_plan_output_v1(
            r#"```json
            {
              "goal":"Repair the failing agent run",
              "openQuestions":[],
              "evidenceNeeded":["Durable event trace"],
              "toolOrder":["Read the trace", "Patch the runtime", "Run tests"]
            }
            ```"#,
        )
        .expect("valid plan");
        assert_eq!(output["goal"], "Repair the failing agent run");
        assert_eq!(output["toolOrder"].as_array().map(Vec::len), Some(3));
    }

    #[test]
    fn rejects_a_generic_user_facing_answer() {
        assert!(
            parse_plan_output_v1("I inspected the project and here is the final answer.").is_err()
        );
    }
}
