//! Adaptive extraction preserves source positions and mandatory evidence.
use super::{Policy, relevance};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub fn rows(value: &Value, query: &str, policy: &Policy) -> Option<Value> {
    let rows = value.as_array()?;
    if rows.len() < 12 || rows.len() > 2048 {
        return None;
    }
    let texts: Vec<_> = rows.iter().map(Value::to_string).collect();
    let docs: Vec<_> = texts.iter().map(String::as_str).collect();
    let mut keep = BTreeSet::from([0, rows.len() - 1]);
    for (i, text) in texts.iter().enumerate() {
        if relevance::protected(text, &policy.protected_text) {
            keep.insert(i);
        }
    }
    let keys: BTreeSet<_> = rows
        .iter()
        .filter_map(Value::as_object)
        .flat_map(|m| m.keys())
        .collect();
    for key in keys {
        let values: Vec<_> = rows.iter().filter_map(|r| r.get(key)).collect();
        if values.len() * 5 < rows.len() {
            for (i, r) in rows.iter().enumerate() {
                if r.get(key).is_some() {
                    keep.insert(i);
                }
            }
        }
        let numbers: Vec<_> = values.iter().filter_map(|v| v.as_f64()).collect();
        if numbers.len() == values.len() && numbers.len() >= 5 {
            let mean = numbers.iter().sum::<f64>() / numbers.len() as f64;
            let deviation = (numbers.iter().map(|v| (v - mean).powi(2)).sum::<f64>()
                / numbers.len() as f64)
                .sqrt();
            for (i, row) in rows.iter().enumerate() {
                if let Some(n) = row.get(key).and_then(Value::as_f64) {
                    if deviation > 0.0 && (n - mean).abs() > 2.0 * deviation {
                        keep.insert(i);
                    }
                    if let Some(prior) = i
                        .checked_sub(1)
                        .and_then(|p| rows[p].get(key))
                        .and_then(Value::as_f64)
                    {
                        if deviation > 0.0 && (n - prior).abs() > 2.0 * deviation {
                            keep.extend([i - 1, i]);
                        }
                    }
                }
            }
        }
        let mut frequencies = BTreeMap::new();
        for value in &values {
            if value.is_string() || value.is_boolean() {
                *frequencies.entry(value.to_string()).or_insert(0usize) += 1;
            }
        }
        if (2..=50).contains(&frequencies.len()) {
            let mut ranked: Vec<_> = frequencies.iter().collect();
            ranked.sort_by_key(|(key, n)| (std::cmp::Reverse(**n), *key));
            let mut majority = BTreeSet::new();
            let mut count = 0;
            for (value, n) in ranked.into_iter().take(5) {
                majority.insert(value);
                count += n;
                if count * 5 >= values.len() * 4 {
                    break;
                }
            }
            if count * 5 >= values.len() * 4 {
                for (i, row) in rows.iter().enumerate() {
                    if let Some(v) = row.get(key) {
                        if !majority.contains(&v.to_string()) {
                            keep.insert(i);
                        }
                    }
                }
            }
        }
    }
    let selected = relevance::select(&docs, query, &keep, policy.target_ratio);
    if selected.len() == rows.len() {
        return None;
    }
    Some(
        json!({"format":"aworkit.rows.v1","totalRows":rows.len(),"selected":selected.iter().map(|&i|json!({"index":i,"value":rows[i]})).collect::<Vec<_>>(),"omittedRows":rows.len()-selected.len()}),
    )
}

/// Sentence/record boundaries retain punctuation and line endings. Indented
/// continuations stay attached; one-line prose uses Unicode sentence ends.
pub fn spans(text: &str) -> Vec<(usize, usize)> {
    let mut result = Vec::new();
    let mut start = 0;
    if text.contains('\n') {
        let mut cursor = 0;
        for line in text.split_inclusive('\n') {
            let continuation = line.starts_with(char::is_whitespace) && !line.trim().is_empty();
            if cursor > start && !continuation {
                result.push((start, cursor));
                start = cursor;
            }
            cursor += line.len();
        }
    } else {
        for (i, c) in text.char_indices() {
            let end = i + c.len_utf8();
            if matches!(c, '。' | '！' | '？')
                || (matches!(c, '.' | '!' | '?') && text[end..].starts_with(char::is_whitespace))
            {
                result.push((start, end));
                start = end;
            }
        }
    }
    if start < text.len() {
        result.push((start, text.len()));
    }
    result
}

pub fn text(text: &str, query: &str, policy: &Policy) -> Option<Value> {
    let is_diff = text.contains("@@ ") && (text.contains("diff --git ") || text.contains("--- "));
    let spans = if is_diff {
        let mut cursor = 0;
        text.split_inclusive('\n')
            .map(|line| {
                let start = cursor;
                cursor += line.len();
                (start, cursor)
            })
            .collect()
    } else {
        spans(text)
    };
    if spans.len() < 12 || spans.len() > 2048 {
        return None;
    }
    let docs: Vec<_> = spans.iter().map(|&(a, b)| &text[a..b]).collect();
    let mut keep = BTreeSet::from([0, spans.len() - 1]);
    for (i, s) in docs.iter().enumerate() {
        let must = relevance::protected(s, &policy.protected_text)
            || (is_diff
                && (s.starts_with(['+', '-', '@', '\\'])
                    || s.starts_with("diff ")
                    || s.starts_with("index ")))
            || ["at ", "File ", "Caused by:"]
                .iter()
                .any(|p| s.trim_start().starts_with(p))
            || s.contains("test result:")
            || s.contains("Tests:")
            || s.contains("passed") && s.contains("failed");
        if must {
            keep.extend(i.saturating_sub(1)..=(i + 1).min(spans.len() - 1));
        }
    }
    let log = docs
        .iter()
        .filter(|s| {
            [
                "INFO",
                "DEBUG",
                "TRACE",
                "WARN",
                "ERROR",
                "Compiling",
                "PASS",
            ]
            .iter()
            .any(|w| s.contains(w))
        })
        .count()
        * 4
        >= docs.len();
    let search = docs
        .iter()
        .filter(|s| {
            s.split(':')
                .any(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        })
        .count()
        * 2
        >= docs.len();
    if !is_diff && !log && !search && !policy.extract_prose {
        return None;
    }
    if search {
        let mut files = BTreeSet::new();
        for (i, s) in docs.iter().enumerate() {
            // Use the prefix before the line-number delimiter; Windows drive
            // colons are not mistaken for line numbers.
            let parts: Vec<_> = s.split(':').collect();
            if let Some(p) = parts
                .iter()
                .position(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
            {
                if files.insert(parts[..p].join(":")) {
                    keep.insert(i);
                }
            }
        }
    }
    let chosen = relevance::select(&docs, query, &keep, policy.target_ratio);
    if chosen.len() == docs.len() {
        return None;
    }
    Some(
        json!({"format":"aworkit.spans.v1","kind":if is_diff{"diff"}else if log{"log"}else if search{"search"}else{"prose"},"originalBytes":text.len(),"spans":chosen.iter().map(|&i|json!({"start":spans[i].0,"end":spans[i].1,"text":docs[i]})).collect::<Vec<_>>(),"omittedSpans":docs.len()-chosen.len()}),
    )
}
