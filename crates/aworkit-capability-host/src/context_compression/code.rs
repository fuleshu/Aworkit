//! Parse-backed source outlines. Omitted ranges are explicit data, never
//! executable replacement code. Parse failures leave source untouched.
use super::{Policy, relevance};
use serde_json::{Value, json};
use tree_sitter::{Language, Node, Parser};

fn grammar(path: &str) -> Option<Language> {
    Some(match path.rsplit('.').next()?.to_lowercase().as_str() {
        "rs" => tree_sitter_rust::LANGUAGE.into(),
        "py" => tree_sitter_python::LANGUAGE.into(),
        "js" | "jsx" | "mjs" | "cjs" => tree_sitter_javascript::LANGUAGE.into(),
        "ts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "c" | "h" => tree_sitter_c::LANGUAGE.into(),
        "cpp" | "cc" | "cxx" | "hpp" => tree_sitter_cpp::LANGUAGE.into(),
        _ => return None,
    })
}
pub fn supported(path: &str) -> bool {
    grammar(path).is_some()
}

fn bodies(
    node: Node<'_>,
    text: &str,
    query: &str,
    policy: &Policy,
    out: &mut Vec<(usize, usize)>,
    depth: usize,
) {
    if depth > 128 || out.len() > 2048 {
        return;
    }
    if matches!(
        node.kind(),
        "function_item"
            | "function_definition"
            | "function_declaration"
            | "method_definition"
            | "method_declaration"
            | "constructor_declaration"
            | "arrow_function"
    ) {
        if let Some(body) = node.child_by_field_name("body") {
            let source = &text[node.byte_range()];
            let query_terms: Vec<_> = relevance::terms(query)
                .into_iter()
                .filter(|t| t.len() > 2)
                .collect();
            let identifiers = relevance::terms(source);
            let relevant = query_terms.iter().any(|term| identifiers.contains(term));
            if !relevant
                && !relevance::protected(source, &policy.protected_text)
                && body.end_byte() - body.start_byte() > 256
            {
                out.push((body.start_byte(), body.end_byte()));
                return;
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        bodies(child, text, query, policy, out, depth + 1);
    }
}

pub fn outline(text: &str, path: &str, query: &str, policy: &Policy) -> Option<Value> {
    let mut parser = Parser::new();
    parser.set_language(&grammar(path)?).ok()?;
    #[allow(deprecated)]
    parser.set_timeout_micros(100_000);
    let tree = parser.parse(text, None)?;
    if tree.root_node().has_error() {
        return None;
    }
    let mut omitted = Vec::new();
    bodies(tree.root_node(), text, query, policy, &mut omitted, 0);
    if omitted.is_empty() {
        return None;
    }
    omitted.sort_unstable();
    let mut start = 0;
    let mut spans = Vec::new();
    for &(a, b) in &omitted {
        if a > start {
            spans.push(json!({"start":start,"end":a,"text":&text[start..a]}));
        }
        start = b;
    }
    if start < text.len() {
        spans.push(json!({"start":start,"end":text.len(),"text":&text[start..]}));
    }
    Some(
        json!({"format":"aworkit.spans.v1","kind":"code_outline","path":path,"notice":"Source outline; function bodies omitted. Retrieve original before editing or reasoning about omitted implementation.","originalBytes":text.len(),"spans":spans,"omittedRanges":omitted}),
    )
}
