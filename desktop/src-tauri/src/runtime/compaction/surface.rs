use super::*;

/// Exchanges are indivisible closed units, including all parallel results and
/// opaque provider signatures. Every other unit is one positioned message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub(crate) enum Unit {
    Message(ModelToolContextV1),
    Exchange(ModelToolExchangeV1),
}
impl Unit {
    pub(crate) fn tokens(&self) -> u64 {
        match self {
            Self::Message(m) => {
                text_tokens(&m.content)
                    + 8
                    + m.images.iter().map(|v| json_tokens(v) + 4).sum::<u64>()
            }
            Self::Exchange(e) => {
                let assistant: u64 = e
                    .assistant_content
                    .iter()
                    .map(|p| match p {
                        ModelAssistantContentV1::Text { text } => text_tokens(text) + 4,
                        ModelAssistantContentV1::ToolCall { call } => {
                            text_tokens(&call.name) + json_tokens(&call.arguments) + 4
                        }
                    })
                    .sum();
                assistant
                    + 4
                    + e.results
                        .iter()
                        .map(|r| result_tokens(&r.content) + 8)
                        .sum::<u64>()
            }
        }
    }
}
fn result_tokens(value: &Value) -> u64 {
    if let Some(blocks) = rich_blocks(value) {
        return blocks
            .iter()
            .map(|block| match block["type"].as_str() {
                Some("text" | "reasoning") => {
                    text_tokens(block["text"].as_str().unwrap_or_default()) + 4
                }
                Some("tool-result") => result_tokens(&block["content"]) + 4,
                Some("tool-call") => {
                    text_tokens(block["name"].as_str().unwrap_or_default())
                        + json_tokens(&block["arguments"])
                        + 4
                }
                _ => json_tokens(block) + 4,
            })
            .sum();
    }
    match value {
        Value::String(s) => text_tokens(s) + 4,
        _ => json_tokens(value) + 4,
    }
}
fn rich_blocks(value: &Value) -> Option<&Vec<Value>> {
    value
        .as_array()
        .or_else(|| value.get("content").and_then(Value::as_array))
        .filter(|blocks| {
            blocks
                .iter()
                .all(|b| b.get("type").and_then(Value::as_str).is_some())
        })
}
pub(crate) fn units(request: &ModelToolRequestV1) -> Result<Vec<Unit>, String> {
    let input = request.input["messages"]
        .as_array()
        .ok_or("Context requires input.messages")?;
    let mut result = Vec::new();
    for i in 0..=input.len() {
        for c in request
            .context_messages
            .iter()
            .filter(|c| c.after_input_messages == Some(i))
        {
            result.push(Unit::Message(c.clone()));
        }
        if let Some(m) = input.get(i).filter(|m| m["role"] != "system") {
            result.push(Unit::Message(ModelToolContextV1 {
                content: m["content"]
                    .as_str()
                    .ok_or("Invalid context message")?
                    .into(),
                role: Some(m["role"].as_str().ok_or("Invalid context role")?.into()),
                images: m["images"].as_array().cloned().unwrap_or_default(),
                ..Default::default()
            }));
        }
    }
    for i in 0..=request.exchanges.len() {
        for c in request
            .context_messages
            .iter()
            .filter(|c| c.after_input_messages.is_none() && c.after_exchanges == i)
        {
            result.push(Unit::Message(c.clone()));
        }
        if let Some(e) = request.exchanges.get(i) {
            result.push(Unit::Exchange(e.clone()));
        }
    }
    Ok(result)
}

/// Rebase positions while retaining message boundaries and authentic metadata.
/// The base input ends at its first ordinary user message; later messages stay
/// in the positioned context so the shared provider validator sees a user tail.
pub(crate) fn replace_units(
    request: &mut ModelToolRequestV1,
    selected: &[Unit],
) -> Result<(), String> {
    let mut input: Vec<_> = request.input["messages"]
        .as_array()
        .ok_or("Context requires messages")?
        .iter()
        .take_while(|m| m["role"] == "system")
        .cloned()
        .collect();
    let mut contexts = Vec::new();
    let mut exchanges = Vec::new();
    let mut base = false;
    for unit in selected {
        match unit {
            Unit::Message(m)
                if !base
                    && m.instruction_event_id.is_none()
                    && m.role.as_deref().unwrap_or("user") == "user" =>
            {
                input.push(m.message());
                base = true;
                // Earlier context belongs before this base user message.
            }
            Unit::Message(m) => {
                let mut m = m.clone();
                m.after_exchanges = exchanges.len();
                m.after_input_messages = (!base).then_some(input.len());
                contexts.push(m);
            }
            Unit::Exchange(e) => {
                exchanges.push(e.clone());
            }
        }
    }
    if !base {
        return Err("Context selection has no ordinary user message".into());
    }
    request.input = json!({"messages": input});
    request.exchanges = exchanges;
    request.context_messages = contexts;
    Ok(())
}
pub(crate) fn estimate(request: &ModelToolRequestV1) -> Result<u64, String> {
    let system: u64 = request.input["messages"]
        .as_array()
        .ok_or("Context requires messages")?
        .iter()
        .filter(|m| m["role"] == "system")
        .map(|m| text_tokens(m["content"].as_str().unwrap_or_default()) + 4)
        .sum();
    Ok(system
        + if request.tools.is_empty() {
            0
        } else {
            json_tokens(&request.tools) + 4
        }
        + units(request)?.iter().map(Unit::tokens).sum::<u64>()
        + request
            .retry_notice
            .as_deref()
            .map_or(0, |n| text_tokens(n) + 8))
}
pub(crate) fn select_prefix(surface: &[Unit], retain_tokens: u64) -> Option<usize> {
    let mut tail = 0;
    for index in (0..surface.len()).rev() {
        tail += surface[index].tokens();
        if tail >= retain_tokens {
            return (index > 0).then_some(index);
        }
    }
    None
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Pruned {
    pub exchange: usize,
    pub call_id: String,
    pub before_hash: String,
    pub chars_before: usize,
    pub chars_after: usize,
}

/// Textual tool results are protocol-neutral JSON values. Native/MCP rich
/// blocks are retained in their original order; only their text fields shrink.
pub(crate) fn prune(request: &mut ModelToolRequestV1, policy: &Policy) -> Vec<Pruned> {
    let mut changes = Vec::new();
    for (index, exchange) in request.exchanges.iter_mut().enumerate() {
        for result in &mut exchange.results {
            let original = result.content.clone();
            let (before, after) = prune_value(&mut result.content, policy);
            if after < before {
                changes.push(Pruned {
                    exchange: index,
                    call_id: result.call_id.clone(),
                    before_hash: hash(&original),
                    chars_before: before,
                    chars_after: after,
                });
            }
        }
    }
    changes
}
fn prune_value(value: &mut Value, policy: &Policy) -> (usize, usize) {
    // Structured MCP content arrays are rich; ordinary structured results are
    // rendered as JSON by the providers, so prune that exact rendered text.
    let rich = rich_blocks(value).is_some();
    if rich {
        let blocks = if value.is_array() {
            value.as_array_mut()
        } else {
            value["content"].as_array_mut()
        }
        .expect("rich content");
        let before = blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .map(|s| s.chars().count())
            .sum();
        if before <= policy.threshold_chars {
            return (before, before);
        }
        let mut consumed = 0;
        let mut marker = false;
        for block in blocks.iter_mut().filter(|b| b["type"] == "text") {
            let Some(text) = block["text"].as_str() else {
                continue;
            };
            let points: Vec<_> = text.chars().collect();
            let head = policy.head_chars.saturating_sub(consumed).min(points.len());
            let tail = before
                .saturating_sub(policy.tail_chars)
                .saturating_sub(consumed)
                .min(points.len());
            let intersects = consumed < before - policy.tail_chars
                && consumed + points.len() > policy.head_chars;
            let mut kept: String = points[..head].iter().collect();
            if intersects && !marker {
                kept.push_str(PRUNE_MARKER);
                marker = true;
            }
            kept.extend(points[tail..].iter());
            consumed += points.len();
            block["text"] = json!(kept);
        }
        return (
            before,
            policy.head_chars + policy.tail_chars + PRUNE_MARKER.chars().count(),
        );
    }
    let text = value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string());
    let count = text.chars().count();
    if count <= policy.threshold_chars {
        return (count, count);
    }
    let points: Vec<_> = text.chars().collect();
    let reduced = format!(
        "{}{}{}",
        points[..policy.head_chars].iter().collect::<String>(),
        PRUNE_MARKER,
        points[count - policy.tail_chars..]
            .iter()
            .collect::<String>()
    );
    let after = reduced.chars().count();
    *value = json!(reduced);
    (count, after)
}
