//! Bounded access to a caller-authorized original. This module never resolves
//! references or authority. UTF-8 byte positions always address original data.
use super::{extract, relevance, render};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    pub operation: String,
    pub reference: Option<String>,
    pub pointer: Option<String>,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
    pub query: Option<String>,
}
impl Request {
    pub fn parse(value: &Value) -> Result<Self, String> {
        let request: Self = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        if !matches!(request.operation.as_str(), "read" | "search" | "stats")
            || request
                .reference
                .as_ref()
                .is_some_and(|s| s.len() != 64 || !s.bytes().all(|c| c.is_ascii_hexdigit()))
            || request.operation == "read" && request.reference.is_none()
            || request.operation == "search"
                && request.query.as_ref().is_none_or(|q| q.trim().is_empty())
            || request
                .query
                .as_ref()
                .is_some_and(|q| q.len() > 4096 || q.contains('\0'))
            || request
                .pointer
                .as_ref()
                .is_some_and(|p| p.len() > 4096 || !p.is_empty() && !p.starts_with('/'))
            || request.limit.is_some_and(|n| !(256..=65536).contains(&n))
            || request.operation == "stats"
                && (request.reference.is_some()
                    || request.pointer.is_some()
                    || request.query.is_some()
                    || request.offset.is_some()
                    || request.limit.is_some())
        {
            return Err("Invalid context retrieval arguments".into());
        }
        Ok(request)
    }
}

pub fn retrieve(
    original: &Value,
    request: &Request,
    maximum_bytes: usize,
) -> Result<Value, String> {
    let selected = match &request.pointer {
        Some(p) => original
            .pointer(p)
            .ok_or("JSON pointer not found in original")?,
        None => original,
    };
    let text = render(selected);
    let limit = request.limit.unwrap_or(16384).min(maximum_bytes);
    if limit < 256 {
        return Err("Context retrieval needs at least 256 output bytes".into());
    }
    if request.operation == "search" {
        let pattern = relevance::terms(request.query.as_deref().unwrap_or(""))
            .into_iter()
            .filter(|t| t.len() > 1)
            .take(128)
            .map(|t| regex::escape(&t))
            .collect::<Vec<_>>()
            .join("|");
        let matcher = regex::RegexBuilder::new(&pattern)
            .case_insensitive(true)
            .build()
            .map_err(|e| e.to_string())?;
        let spans = extract::spans(&text);
        let docs: Vec<_> = spans.iter().map(|&(a, b)| &text[a..b]).collect();
        let scores = relevance::scores(&docs, request.query.as_deref().unwrap_or(""));
        let mut ranked: Vec<_> = scores
            .iter()
            .enumerate()
            .filter(|(_, s)| **s > 0.0)
            .collect();
        ranked.sort_by(|a, b| b.1.total_cmp(a.1).then_with(|| a.0.cmp(&b.0)));
        let total = ranked.len();
        let start = request.offset.unwrap_or(0);
        if start > total {
            return Err("Search offset exceeds matching spans".into());
        }
        let mut matches = Vec::new();
        let mut next = start;
        let header=json!({"reference":request.reference,"pointer":request.pointer,"matches":[],"totalMatches":total,"nextOffset":total,"offsetUnit":"ranked matches; start/end are original UTF-8 bytes"}).to_string().len()+32;
        for &(i, score) in ranked.iter().skip(start) {
            let (a, b) = spans[i];
            let overhead = json!({"start":a,"end":b,"spanEnd":b,"text":"","score":score})
                .to_string()
                .len()
                + 16;
            let room = limit.saturating_sub(header + json!(&matches).to_string().len() + overhead);
            let mut window = a;
            if b - a > room {
                // Common words near the start must not hide a rare identifier
                // near the end of a large record. Prefer its least frequent
                // matching term, with stable earliest-position ties.
                let mut occurrences = std::collections::BTreeMap::new();
                for m in matcher.find_iter(&text[a..b]) {
                    let entry = occurrences
                        .entry(m.as_str().to_lowercase())
                        .or_insert((0usize, m.start()));
                    entry.0 += 1;
                }
                let matched = occurrences
                    .values()
                    .min()
                    .map_or(0, |(_, position)| *position);
                window = a + matched;
                let mut prefix_bytes = 0;
                for c in text[a..window].chars().rev() {
                    prefix_bytes += escaped_char_bytes(c);
                    if prefix_bytes > room / 4 {
                        break;
                    }
                    window -= c.len_utf8();
                }
            }
            let end = window + escaped_prefix(&text[window..b], room);
            if end == window {
                break;
            }
            let item = json!({"start":window,"end":end,"spanEnd":b,"text":&text[window..end],"score":score});
            matches.push(item);
            next += 1;
        }
        let result = json!({"reference":request.reference,"pointer":request.pointer,"matches":matches,"totalMatches":total,"nextOffset":if next<total{Some(next)}else{None},"offsetUnit":"ranked matches; start/end are original UTF-8 bytes"});
        if result.to_string().len() > limit || next == start && start < total {
            return Err("Increase retrieval page size to fit metadata and one match".into());
        }
        return Ok(result);
    }
    let start = request.offset.unwrap_or(0);
    if start > text.len() || !text.is_char_boundary(start) {
        return Err("Offset must be an original UTF-8 byte boundary".into());
    }
    let header=json!({"reference":request.reference,"pointer":request.pointer,"start":start,"end":text.len(),"totalBytes":text.len(),"text":"","nextOffset":text.len(),"offsetUnit":"UTF-8 bytes"}).to_string().len()+16;
    let end = start + escaped_prefix(&text[start..], limit.saturating_sub(header));
    let result = json!({"reference":request.reference,"pointer":request.pointer,"start":start,"end":end,"totalBytes":text.len(),"text":&text[start..end],"nextOffset":if end<text.len(){Some(end)}else{None},"offsetUnit":"UTF-8 bytes"});
    if result.to_string().len() > limit || end == start && start < text.len() {
        return Err("Increase retrieval page size to fit metadata and one character".into());
    }
    Ok(result)
}

/// Fit actual JSON escaping cost rather than wasting five sixths of every
/// retrieval page. Returned offsets always land on a Unicode boundary.
fn escaped_prefix(text: &str, budget: usize) -> usize {
    let mut used = 0;
    let mut end = 0;
    for (i, c) in text.char_indices() {
        let bytes = escaped_char_bytes(c);
        if used + bytes > budget {
            break;
        }
        used += bytes;
        end = i + c.len_utf8();
    }
    end
}

fn escaped_char_bytes(c: char) -> usize {
    match c {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        c if c < ' ' => 6,
        _ => c.len_utf8(),
    }
}
