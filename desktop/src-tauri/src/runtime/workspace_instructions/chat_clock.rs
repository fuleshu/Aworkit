//! Compact UTC clock context derived from the committed Chat lifecycle.
use crate::runtime::semantic_events::CoreEventEnvelope;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub(super) fn chat_start(events: &[CoreEventEnvelope]) -> String {
    let started = events.iter().find(|event| event.kind == "chat.started")
        .or_else(|| events.iter().find(|event| event.kind == "message.user"));
    let time = started.and_then(|event| event.payload["createdAt"].as_str())
        .and_then(|value| value.parse::<i64>().ok())
        .and_then(|seconds| OffsetDateTime::from_unix_timestamp(seconds).ok())
        .unwrap_or_else(OffsetDateTime::now_utc);
    time.replace_nanosecond(0).unwrap_or(time).format(&Rfc3339).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::semantic_events::{envelope, SemanticEventDraft};
    #[test]
    fn retains_the_chat_start_instead_of_the_latest_turn_time() {
        let events = [("chat.started", "1788854400"), ("message.user", "1788858000")].into_iter()
            .enumerate().map(|(index, (kind, created))| envelope("chat.clock", "main", index as u64 + 1,
                SemanticEventDraft::new(kind, serde_json::json!({"createdAt":created})))).collect::<Vec<_>>();
        assert_eq!(chat_start(&events), "2026-09-08T08:00:00Z");
    }
}
