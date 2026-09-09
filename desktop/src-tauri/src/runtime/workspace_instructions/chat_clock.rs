//! Trusted clock context anchored to the committed user turn, stable during replay.
use crate::runtime::semantic_events::CoreEventEnvelope;
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

// A fixed reservation keeps the file-guidance identity stable across date/weekday lengths.
pub(super) const MAX_BYTES: usize = 1024;

fn timestamp(event: &CoreEventEnvelope) -> Option<OffsetDateTime> {
    event.payload["createdAt"]
        .as_str()?
        .parse::<i64>()
        .ok()
        .and_then(|seconds| OffsetDateTime::from_unix_timestamp(seconds).ok())
}

/// Retain Chat provenance but refresh "today" from each new committed user input.
pub(super) fn context(events: &[CoreEventEnvelope]) -> String {
    let started = events
        .iter()
        .find(|event| event.kind == "chat.started")
        .and_then(timestamp);
    let current = events
        .iter()
        .rev()
        .filter(|event| event.kind == "message.user")
        .find_map(timestamp)
        .or(started)
        .unwrap_or_else(OffsetDateTime::now_utc);
    render(
        started.unwrap_or(current),
        current,
        UtcOffset::local_offset_at(current).ok(),
    )
}

fn render(started: OffsetDateTime, current: OffsetDateTime, offset: Option<UtcOffset>) -> String {
    let format = |date: OffsetDateTime| {
        date.replace_nanosecond(0)
            .unwrap_or(date)
            .format(&Rfc3339)
            .unwrap_or_default()
    };
    let local = offset.map(|offset| current.to_offset(offset));
    let date = local.unwrap_or(current);
    format!(
        "\n<system-reminder>\nTrusted Aworkit host clock for the latest user turn. \
        Use this timestamp to answer today's date or time directly; no shell, Python or web verification \
        is needed unless the user requests a fresh clock reading or another timezone. \
        This runtime clock supersedes earlier clock entries and dates in project memory.\n\
        Chat started: {}\nCurrent turn UTC: {}\n{}: {} ({})\n</system-reminder>",
        format(started),
        format(current),
        if local.is_some() {
            "Host local date/time"
        } else {
            "Date/time in UTC (host timezone unavailable)"
        },
        format(date),
        date.weekday()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::semantic_events::{SemanticEventDraft, envelope};
    #[test]
    fn retains_chat_start_and_uses_latest_user_turn() {
        let events = [
            ("chat.started", "1788854400"),
            ("message.user", "1788858000"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (kind, created))| {
            envelope(
                "chat.clock",
                "main",
                index as u64 + 1,
                SemanticEventDraft::new(kind, serde_json::json!({"createdAt":created})),
            )
        })
        .collect::<Vec<_>>();
        let text = context(&events);
        assert!(text.contains("Chat started: 2026-09-08T08:00:00Z"));
        assert!(text.contains("Current turn UTC: 2026-09-08T09:00:00Z"));
    }

    #[test]
    fn local_date_can_differ_from_utc_and_chat_start() {
        let start = OffsetDateTime::from_unix_timestamp(1788854400).unwrap();
        let current = start + time::Duration::hours(15);
        let text = render(start, current, Some(UtcOffset::from_hms(2, 0, 0).unwrap()));
        assert!(text.contains("Host local date/time: 2026-09-09T01:00:00+02:00 (Wednesday)"));
        assert!(text.contains("no shell, Python or web verification"));
    }
}
