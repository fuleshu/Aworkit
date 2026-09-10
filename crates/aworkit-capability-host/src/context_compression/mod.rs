//! Local observation compression. Only this module selects representations;
//! the desktop owns durable evidence, scoping and provider admission.
mod code;
mod extract;
mod lossless;
mod policy;
mod relevance;
pub mod retrieval;
pub use policy::{Mode, Policy, Tokenizer, count};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metrics {
    pub before_bytes: usize,
    pub after_bytes: usize,
    pub before_tokens: usize,
    pub after_tokens: usize,
    pub tokenizer: Tokenizer,
    pub strategies: Vec<String>,
    pub lossy: bool,
    pub elapsed_micros: u64,
}
pub struct Compression {
    pub content: Value,
    pub metrics: Metrics,
}

pub fn render(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

/// Attempt lossless representations first. Adaptive extraction is available
/// only with a usable reference owned and committed by the caller.
pub fn compress(
    value: &Value,
    query: &str,
    path: &str,
    policy: &Policy,
    reference: Option<&str>,
    maximum_bytes: usize,
) -> Option<Compression> {
    if policy.validate().is_err() || policy.mode == Mode::Off {
        return None;
    }
    let started = std::time::Instant::now();
    let original = render(value);
    if original.len() < policy.minimum_bytes || original.len() > 524288 {
        return None;
    }
    let mut strategies = Vec::new();
    let mut lossy = false;
    let transformed = transform(
        value,
        query,
        path,
        policy,
        reference.is_some(),
        &mut strategies,
        &mut lossy,
        0,
    );
    if strategies.is_empty() {
        return None;
    }
    let rich = contains_rich(value);
    let content = if let Some(reference) = reference {
        if rich {
            let mut result = transformed;
            let metadata = json!({"reference":reference,"omitted":lossy,"retrieve":"Read or search with Context retrieval; offsets refer to the original"});
            if let Some(blocks) = result.as_array_mut() {
                blocks.push(json!({"type":"text","text":format!("Aworkit context: {metadata}")}));
            } else if result.get("aworkitContext").is_none() {
                result["aworkitContext"] = metadata;
            } else {
                return None;
            }
            result
        } else {
            json!({"aworkitContext":{"reference":reference,"omitted":lossy,"retrieve":"Use Context retrieval before exact counts, quotes or edits"},"value":transformed})
        }
    } else {
        transformed
    };
    let rendered = render(&content);
    let before = count(&original, policy.tokenizer);
    let after = count(&rendered, policy.tokenizer);
    if rendered.len() > maximum_bytes
        || rendered.len() as f64 > original.len() as f64 * (1.0 - policy.minimum_savings)
        || after as f64 > before as f64 * (1.0 - policy.minimum_savings)
    {
        // A byte-smaller extraction can tokenize worse than a reversible table.
        // Keep the lossless candidate available when that extraction fails.
        if lossy {
            let fallback = Policy {
                mode: Mode::Lossless,
                ..policy.clone()
            };
            return compress(value, query, path, &fallback, reference, maximum_bytes);
        }
        return None;
    }
    Some(Compression {
        content,
        metrics: Metrics {
            before_bytes: original.len(),
            after_bytes: rendered.len(),
            before_tokens: before,
            after_tokens: after,
            tokenizer: policy.tokenizer,
            strategies,
            lossy,
            elapsed_micros: started.elapsed().as_micros().min(u64::MAX as u128) as u64,
        },
    })
}

fn is_rich(value: &Value) -> bool {
    value
        .as_array()
        .or_else(|| value.get("content").and_then(Value::as_array))
        .is_some_and(|blocks| {
            !blocks.is_empty()
                && blocks
                    .iter()
                    .all(|b| b.get("type").and_then(Value::as_str).is_some())
        })
}
fn contains_rich(value: &Value) -> bool {
    is_rich(value)
        || protected_block(value)
        || match value {
            Value::Object(map) => map.values().any(contains_rich),
            Value::Array(values) => values.iter().any(contains_rich),
            _ => false,
        }
}

fn protected_block(value: &Value) -> bool {
    value
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| {
            matches!(
                kind,
                "image"
                    | "image_url"
                    | "input_image"
                    | "resource"
                    | "resource_link"
                    | "reasoning"
                    | "thinking"
                    | "redacted_thinking"
                    | "audio"
                    | "video"
            )
        })
}

fn transform(
    value: &Value,
    query: &str,
    path: &str,
    policy: &Policy,
    retrievable: bool,
    strategies: &mut Vec<String>,
    lossy: &mut bool,
    depth: usize,
) -> Value {
    if depth > 16 || protected_block(value) || render(value).len() < policy.minimum_bytes {
        return value.clone();
    }
    // MCP envelopes must keep nontext blocks and metadata in their exact order.
    if is_rich(value) {
        let mut result = value.clone();
        let blocks = if result.is_array() {
            result.as_array_mut().unwrap()
        } else {
            result["content"].as_array_mut().unwrap()
        };
        for block in blocks {
            if block["type"] == "text" {
                if let Some(text) = block["text"].as_str() {
                    let transformed = transform(
                        &json!(text),
                        query,
                        path,
                        policy,
                        retrievable,
                        strategies,
                        lossy,
                        depth + 1,
                    );
                    block["text"] = json!(render(&transformed));
                }
            }
        }
        if let Some(structured) = result.get("structuredContent").cloned() {
            result["structuredContent"] = transform(
                &structured,
                query,
                path,
                policy,
                retrievable,
                strategies,
                lossy,
                depth + 1,
            );
        }
        return result;
    }
    let original = render(value);
    let packed = if value.is_string() {
        let lines: Vec<_> = original.lines().collect();
        let log = lines
            .iter()
            .filter(|s| {
                ["INFO", "DEBUG", "TRACE", "WARN", "ERROR"]
                    .iter()
                    .any(|w| s.contains(w))
            })
            .count()
            * 4
            >= lines.len();
        if log && !code::supported(path) {
            lossless::templates(&original)
        } else {
            None
        }
    } else if contains_rich(value) {
        None
    } else {
        lossless::table(value)
    };
    let mut best = value.clone();
    let mut strategy = None;
    if let Some(packed) = packed {
        if render(&packed).len() < original.len() {
            best = packed;
            strategy = Some("lossless".to_owned());
        }
    }
    let adaptive = policy.mode == Mode::Adaptive && retrievable;
    if adaptive && render(&best).len() as f64 > original.len() as f64 * policy.target_ratio {
        let candidate = if let Some(text) = value.as_str() {
            // Never use prose heuristics on source with a known grammar, even
            // if parsing fails or the user disabled code extraction.
            if code::supported(path) {
                if policy.extract_code {
                    code::outline(text, path, query, policy)
                } else {
                    None
                }
            } else if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                extract::rows(&parsed, query, policy)
            } else if prose_path(path) {
                extract::text(text, query, policy)
            } else {
                None
            }
        } else {
            extract::rows(value, query, policy)
        };
        if let Some(candidate) = candidate {
            if render(&candidate).len() < render(&best).len() {
                best = candidate;
                strategy = Some("extract".into());
                *lossy = true;
            }
        }
    }
    if let Some(strategy) = strategy {
        strategies.push(strategy);
        return best;
    }
    // Mixed structured results: compress recognized content leaves and arrays,
    // preserving surrounding metadata (URLs, hashes, statuses and offsets).
    if let Some(map) = value.as_object() {
        let mut result = map.clone();
        for (key, child) in map {
            if child.is_array()
                || child.is_object()
                || matches!(
                    key.as_str(),
                    "content" | "text" | "stdout" | "stderr" | "markdown" | "snippet" | "body"
                )
            {
                result.insert(
                    key.clone(),
                    transform(
                        child,
                        query,
                        path,
                        policy,
                        retrievable,
                        strategies,
                        lossy,
                        depth + 1,
                    ),
                );
            }
        }
        return Value::Object(result);
    }
    value.clone()
}

/// An unknown source grammar must not silently fall through to prose deletion.
fn prose_path(path: &str) -> bool {
    path.is_empty()
        || std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| {
                matches!(
                    e.to_ascii_lowercase().as_str(),
                    "txt"
                        | "log"
                        | "md"
                        | "markdown"
                        | "rst"
                        | "csv"
                        | "tsv"
                        | "json"
                        | "jsonl"
                        | "ndjson"
                )
            })
}

#[cfg(test)]
mod tests;
