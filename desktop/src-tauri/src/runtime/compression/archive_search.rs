//! Recover references forgotten by a later summary. Scans a bounded page of
//! already authorized archives, returning one exact hit per matching result.
use crate::runtime::semantic_events::CoreEventEnvelope;
use aworkit_capability_host::context_compression::retrieval::{self, Request};
use serde_json::{Value, json};

pub(super) fn search(
    archives: &[&CoreEventEnvelope],
    request: &Request,
    maximum_bytes: usize,
) -> Result<Value, String> {
    let limit = request.limit.unwrap_or(16384).min(maximum_bytes);
    let start = request.offset.unwrap_or(0);
    if start > archives.len() {
        return Err("Search offset exceeds the context archive".into());
    }
    let mut hits = Vec::new();
    let mut next = start;
    for archive in archives.iter().skip(start).take(64) {
        if archive.payload["originalHash"]
            != crate::runtime::compaction::hash(&archive.payload["original"])
        {
            return Err("Context original integrity check failed".into());
        }
        let mut query = request.clone();
        query.reference = archive.payload["reference"].as_str().map(str::to_owned);
        query.offset = None;
        query.limit = Some(limit.saturating_sub(384).max(256));
        match retrieval::retrieve(
            &archive.payload["original"],
            &query,
            limit.saturating_sub(384),
        ) {
            Ok(page) => {
                if let Some(hit) = page["matches"].as_array().and_then(|m| m.first()) {
                    let hit = json!({"reference":query.reference,"capabilityId":archive.payload["capabilityId"],"pointer":query.pointer,"match":hit});
                    hits.push(hit);
                    if json!(&hits).to_string().len() + 256 > limit {
                        hits.pop();
                        break;
                    }
                }
            }
            Err(error) if error == "JSON pointer not found in original" => {}
            Err(error) => return Err(error),
        }
        next += 1;
    }
    if next == start && start < archives.len() {
        return Err("Increase retrieval page size to fit one archive hit".into());
    }
    let result = json!({"matches":hits,"scanned":next-start,"totalArchives":archives.len(),"nextOffset":if next<archives.len(){Some(next)}else{None},"offsetUnit":"archives; use a returned reference for all matches or original bytes"});
    if result.to_string().len() > limit {
        return Err("Search metadata exceeds the selected page size".into());
    }
    Ok(result)
}
