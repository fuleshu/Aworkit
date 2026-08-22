//! Deterministic evaluation of frozen route predicates.

use aworkit_protocol::{StableId, WorkerRouteRuleV1};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

const MAX_PREDICATE_DEPTH: usize = 32;
const MAX_PREDICATE_TERMS: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteTraceOutcome {
    Matched,
    NotMatched,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteTraceEntry {
    pub route_id: StableId,
    pub priority: i32,
    pub outcome: RouteTraceOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoutingDecisionV1 {
    pub route_id: StableId,
    pub transition_id: StableId,
    pub trace: Vec<RouteTraceEntry>,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum RoutingError {
    #[error("no declared route matched node {0}")]
    NoMatch(String),
    #[error("route predicate is malformed: {0}")]
    InvalidPredicate(String),
    #[error("route predicate exceeded its bounded complexity")]
    PredicateTooComplex,
}

/// Evaluates only the frozen Aworkit predicate language. Rules are selected in
/// ascending `(priority, route_id)` order and a fallback must be explicit via
/// `{ "always": true }`; there is no implicit truthiness or hidden default.
pub fn choose_route(
    node_id: &StableId,
    rules: &[WorkerRouteRuleV1],
    facts: &Value,
) -> Result<RoutingDecisionV1, RoutingError> {
    let mut ordered = rules.to_vec();
    ordered.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| left.route_id.as_str().cmp(right.route_id.as_str()))
    });
    let mut trace = Vec::with_capacity(ordered.len());
    for rule in ordered {
        let matched = evaluate_predicate(&rule.predicate, facts)?;
        trace.push(RouteTraceEntry {
            route_id: rule.route_id.clone(),
            priority: rule.priority,
            outcome: if matched {
                RouteTraceOutcome::Matched
            } else {
                RouteTraceOutcome::NotMatched
            },
        });
        if matched {
            return Ok(RoutingDecisionV1 {
                route_id: rule.route_id,
                transition_id: rule.destination_transition,
                trace,
            });
        }
    }
    Err(RoutingError::NoMatch(node_id.to_string()))
}

pub fn evaluate_predicate(predicate: &Value, facts: &Value) -> Result<bool, RoutingError> {
    let mut terms = 0;
    evaluate(predicate, facts, 0, &mut terms)
}

fn evaluate(
    predicate: &Value,
    facts: &Value,
    depth: usize,
    terms: &mut usize,
) -> Result<bool, RoutingError> {
    if depth > MAX_PREDICATE_DEPTH || *terms >= MAX_PREDICATE_TERMS {
        return Err(RoutingError::PredicateTooComplex);
    }
    *terms += 1;
    let object = predicate
        .as_object()
        .ok_or_else(|| RoutingError::InvalidPredicate("predicate must be an object".to_owned()))?;
    if object.len() != 1 {
        return Err(RoutingError::InvalidPredicate(
            "predicate must contain exactly one operator".to_owned(),
        ));
    }
    let (operator, operand) = object.iter().next().expect("length checked");
    match operator.as_str() {
        "always" => operand
            .as_bool()
            .ok_or_else(|| RoutingError::InvalidPredicate("always requires a boolean".to_owned())),
        "exists" => {
            let path = operand.as_str().ok_or_else(|| {
                RoutingError::InvalidPredicate("exists requires a path".to_owned())
            })?;
            Ok(resolve_path(facts, path).is_some())
        }
        "eq" | "neq" => {
            let comparison = operand.as_object().ok_or_else(|| {
                RoutingError::InvalidPredicate(format!("{operator} requires an object"))
            })?;
            if comparison.len() != 2
                || !comparison.contains_key("path")
                || !comparison.contains_key("value")
            {
                return Err(RoutingError::InvalidPredicate(format!(
                    "{operator} requires only path and value"
                )));
            }
            let path = comparison["path"].as_str().ok_or_else(|| {
                RoutingError::InvalidPredicate(format!("{operator}.path must be text"))
            })?;
            let equal = resolve_path(facts, path) == comparison.get("value");
            Ok(if operator == "eq" { equal } else { !equal })
        }
        "and" | "or" => {
            let children = operand.as_array().ok_or_else(|| {
                RoutingError::InvalidPredicate(format!("{operator} requires an array"))
            })?;
            if children.is_empty() || children.len() > MAX_PREDICATE_TERMS {
                return Err(RoutingError::InvalidPredicate(format!(
                    "{operator} requires a non-empty bounded array"
                )));
            }
            if operator == "and" {
                for child in children {
                    if !evaluate(child, facts, depth + 1, terms)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            } else {
                for child in children {
                    if evaluate(child, facts, depth + 1, terms)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
        }
        "not" => Ok(!evaluate(operand, facts, depth + 1, terms)?),
        other => Err(RoutingError::InvalidPredicate(format!(
            "unknown operator {other}"
        ))),
    }
}

fn resolve_path<'a>(facts: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() || path.len() > 512 {
        return None;
    }
    path.split('.').try_fold(facts, |value, segment| {
        if segment.is_empty() {
            None
        } else if let Ok(index) = segment.parse::<usize>() {
            value.as_array()?.get(index)
        } else {
            value.as_object()?.get(segment)
        }
    })
}
