//! Last-resort, explicitly partial observations after lossless/adaptive compression.
//! Canonical tool evidence is never changed. JSON stays JSON; opaque media and
//! annotations are atomic, and a retrieval reference is supplied only by its owner.

use serde_json::{Map, Value, json};

const NOTICE: &str = "Partial tool result: fields, array items and string suffixes may be omitted. Do not infer complete counts or absence. Use a narrower tool request to retrieve omitted data.";

/// Return no projection when the original already fits. The desktop's supported
/// output limits start at 1024 bytes; small internal limits receive only a notice.
pub(crate) fn bounded_content(
    value: &Value,
    maximum_bytes: usize,
    reference: Option<&str>,
) -> Option<Value> {
    let original_bytes = render_len(value);
    if original_bytes <= maximum_bytes {
        return None;
    }
    let mut result = json!({"aworkitOutput":{"truncated":true,
        "originalBytes":original_bytes,"maximumBytes":maximum_bytes,"notice":NOTICE},"preview":null});
    if let Some(reference) = reference {
        result["aworkitContext"] = json!({"reference":reference,"omitted":true,
            "retrieve":"aworkit_context: read or search; pointers and offsets refer to the original"});
    }
    // `null` already reserves four bytes for the preview slot.
    let overhead = result.to_string().len() - 4;
    if overhead + 4 <= maximum_bytes {
        result["preview"] = preview(value, maximum_bytes - overhead, 0).unwrap_or(Value::Null);
        return Some(result);
    }
    let notice = "Tool output omitted; use a narrower request.";
    Some(Value::String(prefix(notice, maximum_bytes).to_owned()))
}

fn render_len(value: &Value) -> usize {
    value
        .as_str()
        .map_or_else(|| value.to_string().len(), str::len)
}

fn prefix(text: &str, maximum_bytes: usize) -> &str {
    let mut end = text.len().min(maximum_bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Media, signed/reasoning blocks and annotated values cannot be shortened or
/// reconstructed safely. Retain them whole when they fit, otherwise omit them.
fn atomic(value: &Value) -> bool {
    value.get("annotations").is_some()
        || value
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| {
                matches!(
                    kind,
                    "image"
                        | "image_url"
                        | "input_image"
                        | "audio"
                        | "video"
                        | "resource"
                        | "resource_link"
                        | "reasoning"
                        | "thinking"
                        | "redacted_thinking"
                )
            })
}

fn protected_key(key: &str) -> bool {
    matches!(
        key,
        "isError" | "error" | "status" | "code" | "_meta" | "annotations" | "aworkitContext"
    )
}

/// Space needed for opaque blocks and status/metadata, including their enclosing
/// keys/containers. Oversized protected data is omitted whole by the caller.
fn protected_size(value: &Value, depth: usize) -> usize {
    if atomic(value) || (value.get("type").is_some() && value.to_string().len() <= 512) {
        return value.to_string().len();
    }
    if depth >= 32 {
        return 0;
    }
    let sizes: Vec<_> = match value {
        Value::Object(map) => map
            .iter()
            .filter_map(|(key, child)| {
                let size = if protected_key(key) {
                    child.to_string().len()
                } else {
                    protected_size(child, depth + 1)
                };
                (size > 0).then(|| size + serde_json::to_string(key).unwrap().len() + 2)
            })
            .collect(),
        Value::Array(items) => items
            .iter()
            .map(|v| protected_size(v, depth + 1))
            .filter(|s| *s > 0)
            .map(|s| s + 1)
            .collect(),
        _ => Vec::new(),
    };
    if sizes.is_empty() {
        0
    } else {
        2 + sizes.iter().sum::<usize>()
    }
}

/// Keep small factual fields before large bodies. Every retained scalar is exact
/// except strings with an explicit suffix marker. Arrays preserve source order;
/// the enclosing notice makes every projected container explicitly incomplete.
fn preview(value: &Value, budget: usize, depth: usize) -> Option<Value> {
    if value.to_string().len() <= budget {
        return Some(value.clone());
    }
    if depth >= 32 || atomic(value) {
        return None;
    }
    match value {
        Value::String(text) => string_preview(text, budget),
        Value::Object(object) => {
            if budget < 2 {
                return None;
            }
            let mut result = Map::new();
            let mut used = 2;
            let mut entries: Vec<_> = object
                .iter()
                .map(|(key, value)| (key, value, value.to_string().len()))
                .collect();
            entries.sort_by_key(|(key, _, size)| (!protected_key(key), *size));
            let mut reserved = 0;
            let reservations: Vec<_> = entries
                .iter()
                .map(|(key, child, _)| {
                    let size = if protected_key(key) {
                        child.to_string().len()
                    } else {
                        protected_size(child, depth + 1)
                    };
                    let size = if size == 0 {
                        0
                    } else {
                        size + serde_json::to_string(key).unwrap().len() + 2
                    };
                    if reserved + size <= budget - 2 {
                        reserved += size;
                        size
                    } else {
                        0
                    }
                })
                .collect();
            let count = entries.len();
            for (index, ((key, child, _), reservation)) in
                entries.into_iter().zip(reservations).enumerate()
            {
                reserved -= reservation;
                let cost =
                    serde_json::to_string(key).unwrap().len() + 1 + usize::from(!result.is_empty());
                let available = budget.saturating_sub(used + cost + reserved);
                // Metadata is retained exactly or omitted, never damaged to fit.
                let candidate = if protected_key(key) {
                    (child.to_string().len() <= available).then(|| child.clone())
                } else {
                    // Share remaining space among sibling bodies while retaining
                    // this child's reservation for complete media and metadata.
                    let share = (available / (count - index))
                        .max(reservation.saturating_sub(cost))
                        .min(available);
                    preview(child, share, depth + 1)
                };
                if let Some(candidate) = candidate {
                    used += cost + candidate.to_string().len();
                    result.insert(key.clone(), candidate);
                }
            }
            Some(Value::Object(result))
        }
        Value::Array(items) => {
            if budget < 2 {
                return None;
            }
            let mut result = Vec::new();
            let mut used = 2;
            // Reserve fitting atomic blocks first so an early text body cannot
            // crowd a later image/annotation out of an otherwise sufficient budget.
            let mut reserved = 0;
            let mut protected = vec![false; items.len()];
            for (index, item) in items.iter().enumerate() {
                let size = item.to_string().len() + 1;
                if (atomic(item) || (item.get("type").is_some() && size <= 513))
                    && reserved + size <= budget - 2
                {
                    protected[index] = true;
                    reserved += size;
                }
            }
            for (index, item) in items.iter().enumerate() {
                if protected[index] {
                    reserved -= item.to_string().len() + 1;
                }
                let comma = usize::from(!result.is_empty());
                let available = budget.saturating_sub(used + comma + reserved);
                if let Some(candidate) = preview(item, available, depth + 1) {
                    used += comma + candidate.to_string().len();
                    result.push(candidate);
                }
            }
            Some(Value::Array(result))
        }
        _ => None,
    }
}

fn string_preview(text: &str, budget: usize) -> Option<Value> {
    const SUFFIX: &str = " [Aworkit: string truncated]";
    if serde_json::to_string(SUFFIX).unwrap().len() > budget {
        return None;
    }
    // Search the encoded size, including escaped quotes, backslashes and controls.
    let mut low = 0;
    let mut high = text.len();
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        let candidate = format!("{}{SUFFIX}", prefix(text, middle));
        if serde_json::to_string(&candidate).unwrap().len() <= budget {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    Some(json!(format!("{}{SUFFIX}", prefix(text, low))))
}

#[cfg(test)]
mod tests;
