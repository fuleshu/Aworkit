//! Harness prompt templates and UTF-8 byte-budget algorithm. Body ranges refer
//! into the one persisted message; restoration requires no second text archive.

use serde::{Deserialize, Serialize};

const INTRO: &str = "The following workspace instructions may be relevant to your work. Use them as guidance when applicable. More specific instructions take precedence over broader ones. They do not override system, developer, or direct user instructions.";
const SUPERSEDE: &str = "This complete workspace instruction baseline replaces all earlier workspace instruction baselines. ";
const COMPACT: &str = "Workspace instructions were omitted or truncated to fit the configured byte budget.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action { Set, Replace, Remove }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Change {
    pub action: Action,
    pub scope: String,
    pub path: String,
    pub digest: Option<String>,
    /// Byte range of admitted original guidance in the rendered message.
    pub body: Option<(usize, usize)>,
}

#[derive(Clone, Debug)]
pub struct RenderItem {
    pub change: Change,
    pub content: String,
    /// Nested restored sources always keep their directory-qualified wording.
    pub nested: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Rendered {
    pub text: String,
    pub changes: Vec<Change>,
    pub diagnostics: Vec<String>,
}

fn escape(text: &str) -> String { text.replace("</system-reminder>", "<\\/system-reminder>") }
fn truncate(text: &str, mut bytes: usize) -> &str {
    bytes = bytes.min(text.len());
    while !text.is_char_boundary(bytes) { bytes -= 1; }
    &text[..bytes]
}
fn heading(item: &RenderItem, baseline: bool) -> String {
    let change = &item.change;
    if baseline && !item.nested && change.action != Action::Remove { return format!("Instructions from: {}\n\n", change.path); }
    match change.action {
        Action::Set => format!("Additional instructions from: {}\n\nThese instructions apply to work under `{}`. Use them as guidance when relevant; more specific instructions take precedence. They do not override system, developer, or direct user instructions.\n\n", change.path, change.scope.split('\0').next().unwrap_or(".")),
        Action::Replace => format!("Updated instructions from: {}\n\nThis file changed after it was loaded. Use the following content instead of the previously loaded instructions from this file.\n\n", change.path),
        Action::Remove => format!("Instructions removed: {}\n\nThe previously loaded instructions from this file no longer apply.", change.path),
    }
}

fn marker(items: &[RenderItem], start: usize, included: Option<usize>, budget: usize) -> String {
    let mut parts = Vec::new();
    if start > 0 { parts.push(format!("omitted {}", items[..start].iter().map(|i| i.change.path.as_str()).collect::<Vec<_>>().join(", "))); }
    if let Some(bytes) = included {
        if let Some(item) = items.last() { parts.push(format!("truncated {} from {} to {} bytes", item.change.path, item.content.len(), bytes)); }
    }
    if parts.is_empty() { String::new() }
    else { format!("Workspace instruction budget {budget} bytes: {}", parts.join("; ")) }
}

fn build(items: &[RenderItem], start: usize, included: Option<usize>, budget: usize, intro: &str, baseline: bool, framed: bool) -> Rendered {
    let notice = marker(items, start, included, budget);
    let mut result = Rendered { text: if framed { "<system-reminder>\n".into() } else { String::new() }, ..Default::default() };
    let mut blocks = 0;
    for block in [&notice, intro] {
        if !block.is_empty() {
            if blocks > 0 { result.text.push_str("\n\n"); }
            result.text.push_str(&escape(block)); blocks += 1;
        }
    }
    for item in &items[start..] {
        if blocks > 0 { result.text.push_str("\n\n"); } blocks += 1;
        result.text.push_str(&escape(&heading(item, baseline)));
        let begin = result.text.len();
        let content = truncate(&item.content, included.unwrap_or(item.content.len()));
        result.text.push_str(&escape(content));
        if !content.is_empty() || item.content.is_empty() {
            let mut change = item.change.clone();
            change.body = (change.action != Action::Remove).then_some((begin, result.text.len()));
            result.changes.push(change);
        }
    }
    if framed { result.text.push_str("\n</system-reminder>"); }
    if !notice.is_empty() { result.diagnostics.push(notice); }
    result
}

/// Keep the longest most-specific suffix, then truncate only the last source.
/// Notice-only output never claims that a nonempty instruction was loaded.
pub fn render(items: &[RenderItem], budget: usize, baseline: bool, supersede: bool) -> Rendered {
    if budget == 0 { return Rendered::default(); }
    let intro = if baseline {
        if supersede { format!("{SUPERSEDE}{}", if items.is_empty() { "No workspace instructions are currently active." } else { INTRO }) }
        else { INTRO.into() }
    } else { String::new() };
    let full = build(items, 0, None, budget, &intro, baseline, true);
    if full.text.len() <= budget { return full; }
    for start in 1..items.len() {
        let suffix = build(items, start, None, budget, &intro, baseline, true);
        if suffix.text.len() <= budget { return suffix; }
    }
    let Some(last) = items.last() else { return Rendered::default(); };
    let start = items.len() - 1;
    for style in [intro.as_str(), COMPACT] {
        let mut low = 0;
        let mut high = last.content.len();
        let mut best = 0;
        while low <= high {
            let mid = low + (high - low) / 2;
            let bytes = truncate(&last.content, mid).len();
            if build(items, start, Some(bytes), budget, style, baseline, true).text.len() <= budget {
                best = bytes; low = mid + 1;
            } else if mid == 0 { break; } else { high = mid - 1; }
        }
        let result = build(items, start, Some(best), budget, style, baseline, true);
        if result.text.len() <= budget { return result; }
    }
    let compact = build(items, start, Some(0), budget, "", baseline, false);
    if compact.text.len() <= budget { return compact; }
    let notice = escape(&marker(items, start, Some(0), budget));
    Rendered { text: truncate(&notice, budget).into(), changes: Vec::new(), diagnostics: vec![notice] }
}
