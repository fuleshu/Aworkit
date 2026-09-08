//! Deterministic lexical relevance shared by extraction and original search.
use std::collections::{BTreeMap, BTreeSet};

pub fn terms(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut word = String::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        if matches!(c as u32,0x3000..=0x9fff | 0xac00..=0xd7af | 0x20000..=0x2fa1f) {
            if !word.is_empty() {
                result.push(std::mem::take(&mut word));
            }
            result.push(c.to_string());
        } else if c.is_alphanumeric() || c == '_' {
            word.push(c);
        } else if !word.is_empty() {
            result.push(std::mem::take(&mut word));
        }
    }
    if !word.is_empty() {
        result.push(word);
    }
    result
}

/// BM25 scores, bounded query vocabulary and one corpus pass. No embeddings,
/// stemming or fuzzy identifier rewrites that could confuse exact retrieval.
pub fn scores(documents: &[&str], query: &str) -> Vec<f64> {
    let query: BTreeSet<_> = terms(query)
        .into_iter()
        .filter(|w| w.len() > 1)
        .take(128)
        .collect();
    let docs: Vec<_> = documents.iter().map(|d| terms(d)).collect();
    let average = docs.iter().map(Vec::len).sum::<usize>() as f64 / docs.len().max(1) as f64;
    let mut df = BTreeMap::new();
    for doc in &docs {
        for term in doc.iter().collect::<BTreeSet<_>>() {
            if query.contains(term) {
                *df.entry(term.clone()).or_insert(0usize) += 1;
            }
        }
    }
    docs.iter()
        .map(|doc| {
            let mut counts = BTreeMap::new();
            for term in doc {
                if query.contains(term) {
                    *counts.entry(term).or_insert(0usize) += 1;
                }
            }
            counts
                .iter()
                .map(|(term, count)| {
                    let freq = *df.get(*term).unwrap_or(&0) as f64;
                    let idf = (1.0 + (docs.len() as f64 - freq + 0.5) / (freq + 0.5)).ln();
                    let tf = *count as f64;
                    idf * tf * 2.2
                        / (tf + 1.2 * (0.25 + 0.75 * doc.len() as f64 / average.max(1.0)))
                })
                .sum()
        })
        .collect()
}

/// Greedy marginal vocabulary coverage finds the information knee. Protected
/// units are never evicted for a nominal target. Stable ties use source order.
pub fn select(
    documents: &[&str],
    query: &str,
    protected: &BTreeSet<usize>,
    ratio: f64,
) -> BTreeSet<usize> {
    let scores = scores(documents, query);
    let words: Vec<BTreeSet<_>> = documents
        .iter()
        .map(|s| terms(s).into_iter().collect())
        .collect();
    let mut keep = protected.clone();
    let mut known: BTreeSet<_> = keep
        .iter()
        .flat_map(|&i| words[i].iter().cloned())
        .collect();
    let target = ((documents.len() as f64 * ratio).ceil() as usize).max(2);
    let mut candidates: Vec<_> = (0..documents.len()).filter(|i| !keep.contains(i)).collect();
    while keep.len() < target && !candidates.is_empty() {
        let best = candidates
            .iter()
            .enumerate()
            .map(|(p, &i)| {
                let novel =
                    words[i].difference(&known).count() as f64 / words[i].len().max(1) as f64;
                (p, i, scores[i] + novel)
            })
            .max_by(|a, b| a.2.total_cmp(&b.2).then_with(|| b.1.cmp(&a.1)))
            .unwrap();
        if best.2 <= 0.0 {
            break;
        }
        keep.insert(best.1);
        known.extend(words[best.1].iter().cloned());
        candidates.remove(best.0);
    }
    keep
}

pub fn protected(text: &str, literals: &[String]) -> bool {
    if literals.iter().any(|p| text.contains(p)) {
        return true;
    }
    terms(text).iter().any(|w| {
        matches!(
            w.as_str(),
            "error"
                | "errors"
                | "failed"
                | "failure"
                | "fatal"
                | "panic"
                | "exception"
                | "traceback"
                | "denied"
                | "warning"
                | "warn"
                | "assertion"
                | "critical"
        )
    })
}
