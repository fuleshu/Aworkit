//! Canonical history retention: what a Chat keeps, and what it tombstones.
//!
//! One turn of an Agent node stores its context twice: once as a durable
//! `context.checkpoint` (the resumption and provider-prefix authority) and once
//! as the model-call span's compiled `input` (evidence of what was sent). On a
//! long session those two dominate the store — measured on a real profile,
//! 7,694 checkpoints and 7,923 model-call inputs were about 97% of a 15 GB
//! payload, and every other kind together was a few hundred megabytes.
//!
//! Nothing here touches the conversation, the tool records, the usage, the
//! timing or the sidebar index. Retention decides only which *superseded*
//! snapshots keep their bytes:
//!
//! - the newest checkpoint per `(ownerKey, nodeId, child)` scope is always kept,
//!   because that is exactly the key [`super::compaction`]'s context snapshot
//!   resumes from, so every Chat can still continue where it ended;
//! - the newest [`RETAINED_TURNS_V1`] checkpoints of *each* such scope, and the
//!   newest [`RETAINED_TURNS_V1`] model-call inputs of the Chat, are kept whole,
//!   so its recent turns stay fully inspectable;
//! - every older payload becomes a tombstone that names what was dropped
//!   (`digestBefore`, `bytes`, `prunedAt`, `reason`). The event keeps its
//!   identity, kind, sequence, timing and usage, so a projection still renders
//!   the row and can state the omission instead of silently showing less.
//!
//! A fork or a replay from an intermediate turn may therefore no longer find
//! that turn's exact snapshot; it must report that rather than invent context.
//! Forking or continuing at the head is unaffected.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{Map, Value, json};

use super::digest;

/// How many of one Chat's newest turns keep their full snapshot payload.
pub(crate) const RETAINED_TURNS_V1: usize = 20;

/// The progress phase a pass reports while it releases superseded payloads.
///
/// A stable label, not user-facing copy: the Settings action owns the wording.
pub(crate) const RECLAIM_PHASE_RELEASING: &str = "releasing";
/// The progress phase a pass reports while it rewrites the store file.
pub(crate) const RECLAIM_PHASE_REWRITING: &str = "rewriting";

/// How many already-delivered delivery records one pass or one append keeps.
///
/// The queue's next cursor is derived from its maximum, and an acknowledgement
/// may still be in flight for a record delivered moments ago, so the newest
/// records stay. Everything older is a duplicate of an event the store already
/// holds, and nothing reads it again.
pub(crate) const OUTBOX_RETAINED_V1: u32 = 512;

/// How many superseded payloads one append may release.
///
/// The explicit reclaim is what clears a backlog. This bound is what keeps a
/// live Chat from growing one: an append releases the turn it just displaced, so
/// the write path never digests a history it did not just walk past.
pub(crate) const MAX_RELEASES_PER_APPEND_V1: usize = 4;

/// How far back one append's scan looks.
///
/// The scan walks newest first, so a bounded look back is enough to find the turn
/// an append displaced. See [`plan_displaced`] for why truncating it is safe.
pub(crate) const APPEND_SCAN_LIMIT_V1: usize = 4_096;

/// The checkpoint kind that carries a resumable snapshot.
const CHECKPOINT_KIND: &str = "context.checkpoint";
/// The span kind whose payload carries a compiled provider request.
const MODEL_CALL_SPAN_KIND: &str = "model_call";
/// The event kinds a reclaim pass has to read. Every other kind is conversation,
/// tool evidence, usage or timing, and retention never touches it.
pub(crate) const PRUNABLE_KINDS: [&str; 2] = [CHECKPOINT_KIND, "span.started"];

/// What one reclaim pass did, for the caller to report to the user.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReclaimReportV1 {
    /// Chat streams examined.
    pub streams_scanned: u64,
    /// Payloads replaced by a tombstone.
    pub payloads_pruned: u64,
    /// Bytes the released payloads held, as recorded in their tombstones.
    pub payload_bytes_released: u64,
    /// Already-deleted Chats whose events were removed.
    pub chats_purged: u64,
    /// Events removed with those Chats.
    pub events_removed: u64,
    /// Already-delivered delivery records removed.
    pub outbox_rows_removed: u64,
    /// The payload bytes those delivery records held.
    pub outbox_bytes_released: u64,
}

/// A reclaim pass plus the store size it moved from and to.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReclaimOutcomeV1 {
    pub report: ReclaimReportV1,
    /// Database file size before the pass, in bytes.
    pub store_bytes_before: u64,
    /// Database file size after the vacuum, in bytes.
    pub store_bytes_after: u64,
}

/// The progress of one reclaim pass, as the visible action reports it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReclaimProgressV1 {
    /// The step the pass is in, in the user's words.
    pub phase: String,
    /// How much of that step is done.
    pub done: u64,
    /// How much of that step there is, or zero while it is unknown.
    pub total: u64,
}

/// What the store holds and what a reclaim could release.
///
/// Measured, not estimated: the sizes are the payload bytes actually stored, so
/// the Settings line can state the policy and its cost without guessing.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryStoreStatusV1 {
    /// Bytes the store database and its write-ahead log hold right now.
    pub store_bytes: u64,
    /// Bytes every stored payload holds together.
    pub payload_bytes: u64,
    /// The payload bytes the two superseded kinds hold: the pool a reclaim
    /// draws from. The newest turns of that pool are kept, so a reclaim releases
    /// at most this much.
    pub snapshot_bytes: u64,
    /// Chats the user deleted whose events are still stored.
    pub deleted_chats: u64,
    /// The payload bytes those deleted Chats still hold.
    pub deleted_chat_bytes: u64,
    /// Delivery records the store still holds, delivered or not.
    pub outbox_rows: u64,
    /// How many of those were already delivered: the ones a pass can remove.
    pub outbox_delivered_rows: u64,
    /// The payload bytes every delivery record holds together.
    pub outbox_bytes: u64,
    /// Turns a Chat keeps whole.
    pub retained_turns: u64,
    /// The stored payload, largest kind first.
    pub kinds: Vec<HistoryPayloadKindV1>,
}

/// One event kind's share of the stored payload.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPayloadKindV1 {
    pub kind: String,
    pub events: u64,
    pub bytes: u64,
}

/// The payload field that marks a payload whose bytes were dropped.
pub(crate) const PRUNED_PAYLOAD_FIELD: &str = "prunedPayload";
/// Shape version of the tombstone this build writes.
const PRUNED_PAYLOAD_SCHEMA_VERSION: u64 = 1;
/// Why a payload was dropped, recorded in the tombstone.
const PRUNED_REASON: &str = "superseded: only the newest snapshot per context scope and the newest turns of a Chat keep their bytes";

/// Which read a pruned payload served.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PrunedPayloadKindV1 {
    /// A `context.checkpoint` snapshot: resumption and provider-prefix authority.
    ContextCheckpoint,
    /// A model-call span's compiled input: evidence of what was sent.
    ModelCallInput,
}

impl PrunedPayloadKindV1 {
    /// The stable name recorded in the tombstone.
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ContextCheckpoint => "context_checkpoint",
            Self::ModelCallInput => "model_call_input",
        }
    }
}

/// One committed event, as retention sees it.
pub(crate) struct RetentionEventV1<'a> {
    pub event_id: &'a str,
    pub sequence: u64,
    pub kind: &'a str,
    pub payload: &'a Value,
}

/// One payload retention replaces, with the exact replacement to store.
#[derive(Debug)]
pub(crate) struct PrunedPayloadV1 {
    pub event_id: String,
    pub sequence: u64,
    pub kind: PrunedPayloadKindV1,
    /// The complete replacement payload: the original identity and timing with
    /// the dropped value replaced by the tombstone marker.
    pub payload: Value,
}

/// Whether a payload is already a tombstone.
#[must_use]
pub(crate) fn is_pruned(payload: &Value) -> bool {
    payload
        .get(PRUNED_PAYLOAD_FIELD)
        .is_some_and(Value::is_object)
}

/// The exact context scope `context_snapshot` resumes from, as a stable key.
fn checkpoint_scope(payload: &Value) -> String {
    let text = |field: &str| {
        payload
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let child = match payload.get("child") {
        None | Some(Value::Null) => String::new(),
        Some(value) => value.to_string(),
    };
    format!("{}|{}|{}", text("ownerKey"), text("nodeId"), child)
}

/// The payload field a kind keeps its heavy bytes in.
fn heavy_field(kind: PrunedPayloadKindV1) -> &'static str {
    match kind {
        PrunedPayloadKindV1::ContextCheckpoint => "snapshot",
        PrunedPayloadKindV1::ModelCallInput => "input",
    }
}

/// Classifies one payload as a prunable one, if it is.
pub(crate) fn prunable_kind(kind: &str, payload: &Value) -> Option<PrunedPayloadKindV1> {
    if is_pruned(payload) {
        return None;
    }
    let kind = if kind == CHECKPOINT_KIND {
        PrunedPayloadKindV1::ContextCheckpoint
    } else if kind == "span.started"
        && payload.get("spanKind").and_then(Value::as_str) == Some(MODEL_CALL_SPAN_KIND)
    {
        PrunedPayloadKindV1::ModelCallInput
    } else {
        return None;
    };
    // A payload that never carried the heavy value is nothing to prune.
    payload
        .get(heavy_field(kind))
        .filter(|value| !value.is_null())
        .map(|_| kind)
}

/// Classifies one event as a prunable payload, if it is one.
pub(crate) fn prunable(event: &RetentionEventV1<'_>) -> Option<PrunedPayloadKindV1> {
    prunable_kind(event.kind, event.payload)
}

/// The windows an append moved, which are the only ones it has to answer for.
///
/// An append that carries neither a checkpoint nor a model-call input displaces
/// nothing, so the write path leaves retention alone entirely.
pub(crate) fn triggering_kinds<'a>(
    events: impl IntoIterator<Item = (&'a str, &'a Value)>,
) -> Vec<PrunedPayloadKindV1> {
    let mut kinds: Vec<PrunedPayloadKindV1> = Vec::new();
    for (kind, payload) in events {
        if let Some(kind) = prunable_kind(kind, payload) {
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
    }
    kinds
}

/// Builds the tombstone payload: every original field kept, the dropped value
/// replaced by a marker that records what was dropped and when.
fn tombstone(
    payload: &Value,
    kind: PrunedPayloadKindV1,
    dropped: &Value,
    pruned_at: &str,
    retained_turns: usize,
) -> Result<Value, String> {
    let mut object: Map<String, Value> = payload
        .as_object()
        .ok_or_else(|| "a prunable payload must be an object".to_owned())?
        .iter()
        .filter(|(field, _)| field.as_str() != heavy_field(kind))
        .map(|(field, value)| (field.clone(), value.clone()))
        .collect();
    // `hasInput` is deliberately preserved: the call did carry a request. The
    // marker is what says its bytes are gone.
    object.insert(
        PRUNED_PAYLOAD_FIELD.to_owned(),
        json!({
            "schemaVersion": PRUNED_PAYLOAD_SCHEMA_VERSION,
            "kind": kind.as_str(),
            "bytes": serde_json::to_vec(dropped).map_err(|error| error.to_string())?.len(),
            "digestBefore": digest::canonical_hash(dropped)?,
            "prunedAt": pruned_at,
            "retainedTurns": retained_turns,
            "reason": PRUNED_REASON,
        }),
    );
    Ok(Value::Object(object))
}

/// Decides which payloads of one Chat to replace.
///
/// `events` is that Chat's committed stream. The plan is idempotent: planning
/// again over the result prunes nothing new, because a tombstone is not
/// prunable.
///
/// # Errors
///
/// Returns a reason when a payload cannot be digested or re-encoded.
pub(crate) fn plan(
    events: &[RetentionEventV1<'_>],
    retained_turns: usize,
    pruned_at: &str,
) -> Result<Vec<PrunedPayloadV1>, String> {
    let candidates = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| prunable(event).map(|kind| (index, kind)))
        .collect::<Vec<_>>();

    // Newest first, so "the newest N turns" is a prefix of each class.
    let mut by_kind = [
        (PrunedPayloadKindV1::ContextCheckpoint, Vec::new()),
        (PrunedPayloadKindV1::ModelCallInput, Vec::new()),
    ];
    for (kind, indices) in &mut by_kind {
        *indices = candidates
            .iter()
            .filter(|(_, candidate)| candidate == kind)
            .map(|(index, _)| *index)
            .collect();
        indices.sort_by_key(|index| std::cmp::Reverse(events[*index].sequence));
    }

    let mut keep = BTreeSet::new();
    // Checkpoints: the newest N turns of *each* context scope, so a scope that
    // runs alongside another cannot be starved of its own recent snapshots. The
    // newest checkpoint of every scope is therefore always kept, which is the
    // exact record a resumed pass reads.
    let mut kept_per_scope: BTreeMap<String, usize> = BTreeMap::new();
    for index in &by_kind[0].1 {
        let scope = checkpoint_scope(events[*index].payload);
        let kept = kept_per_scope.entry(scope).or_default();
        if *kept < retained_turns {
            *kept += 1;
            keep.insert(*index);
        }
    }
    // Compiled requests are evidence of what a call sent, not resumption state,
    // so the Chat's newest N of them is the whole rule.
    for index in by_kind[1].1.iter().take(retained_turns) {
        keep.insert(*index);
    }

    let mut plan = Vec::new();
    for (index, kind) in candidates {
        if keep.contains(&index) {
            continue;
        }
        let event = &events[index];
        let dropped = &event.payload[heavy_field(kind)];
        plan.push(PrunedPayloadV1 {
            event_id: event.event_id.to_owned(),
            sequence: event.sequence,
            kind,
            payload: tombstone(event.payload, kind, dropped, pruned_at, retained_turns)?,
        });
    }
    plan.sort_by_key(|pruned| pruned.sequence);
    Ok(plan)
}

/// Decides which payloads one append just displaced from the retained window.
///
/// `events` is the Chat's committed stream in sequence order, and `triggering`
/// names the kinds the append actually added: an append only has to answer for a
/// window it moved. At most `limit` payloads are released, so a live Chat stays
/// bounded without the write path ever digesting a whole history.
///
/// The scan walks newest first and stops after [`APPEND_SCAN_LIMIT_V1`] events.
/// Truncating it is safe by construction: every event it never reached is older
/// than every event it decided about, so it can only leave a displaced payload
/// in place for the next append. It can never release a payload that is still
/// inside the window, and never the newest snapshot of a scope.
///
/// # Errors
///
/// Returns a reason when a payload cannot be digested or re-encoded.
pub(crate) fn plan_displaced(
    events: &[RetentionEventV1<'_>],
    triggering: &[PrunedPayloadKindV1],
    retained_turns: usize,
    pruned_at: &str,
    limit: usize,
) -> Result<Vec<PrunedPayloadV1>, String> {
    let mut per_scope: BTreeMap<String, usize> = BTreeMap::new();
    let mut model_calls = 0_usize;
    let mut displaced = Vec::new();
    let first = events.len().saturating_sub(APPEND_SCAN_LIMIT_V1);
    for event in events[first..].iter().rev() {
        let Some(kind) = prunable(event) else {
            continue;
        };
        if !triggering.contains(&kind) {
            continue;
        }
        let kept = match kind {
            PrunedPayloadKindV1::ContextCheckpoint => {
                let count = per_scope
                    .entry(checkpoint_scope(event.payload))
                    .or_default();
                *count += 1;
                *count
            }
            PrunedPayloadKindV1::ModelCallInput => {
                model_calls += 1;
                model_calls
            }
        };
        if kept <= retained_turns {
            continue;
        }
        let dropped = &event.payload[heavy_field(kind)];
        displaced.push(PrunedPayloadV1 {
            event_id: event.event_id.to_owned(),
            sequence: event.sequence,
            kind,
            payload: tombstone(event.payload, kind, dropped, pruned_at, retained_turns)?,
        });
        if displaced.len() >= limit {
            break;
        }
    }
    displaced.sort_by_key(|pruned| pruned.sequence);
    Ok(displaced)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkpoint(sequence: u64, owner: &str, node: &str) -> (String, String, Value) {
        (
            format!("event.checkpoint.{owner}.{sequence}"),
            CHECKPOINT_KIND.to_owned(),
            json!({
                "ownerKey": owner,
                "nodeId": node,
                "child": null,
                "requestId": format!("request.{sequence}"),
                "runId": "run.fixture",
                "createdAt": sequence.to_string(),
                "pressureTokens": 1000 + sequence,
                "snapshot": {"document": {"exchanges": [sequence]}, "filler": "x".repeat(64)},
            }),
        )
    }

    fn model_call(sequence: u64) -> (String, String, Value) {
        (
            format!("event.model.{sequence}"),
            "span.started".to_owned(),
            json!({
                "spanId": format!("span.model.{sequence}"),
                "spanKind": MODEL_CALL_SPAN_KIND,
                "parentSpanId": "span.node.agent.1",
                "turn": sequence,
                "title": "Model call",
                "status": "running",
                "hasInput": true,
                "usage": {"inputTokens": sequence},
                "input": {"messages": [{"role": "user", "content": "y".repeat(64)}], "turn": sequence},
            }),
        )
    }

    fn conversation(sequence: u64) -> (String, String, Value) {
        (
            format!("event.message.{sequence}"),
            "message.user".to_owned(),
            json!({"body": format!("message {sequence}")}),
        )
    }

    fn events<'a>(fixture: &'a [(String, String, Value)]) -> Vec<RetentionEventV1<'a>> {
        fixture
            .iter()
            .enumerate()
            .map(|(index, (event_id, kind, payload))| RetentionEventV1 {
                event_id,
                sequence: u64::try_from(index + 1).unwrap(),
                kind,
                payload,
            })
            .collect()
    }

    #[test]
    fn the_newest_scope_and_the_newest_turns_survive_and_the_rest_is_tombstoned() {
        // 50 turns of one scope, a second scope that stopped at turn 45, a third
        // scope whose newest checkpoint is far outside the turn window, and 50
        // model-call inputs. Conversation events must never be candidates.
        let mut fixture = Vec::new();
        for turn in 1..=50 {
            fixture.push(checkpoint(turn, "owner.a", "agent.1"));
            fixture.push(model_call(turn));
            fixture.push(conversation(turn));
            if turn == 3 {
                // A scope that stopped long ago: its newest snapshot is far
                // outside the turn window and must survive anyway.
                fixture.push(checkpoint(3, "owner.c", "agent.1"));
            }
            if turn == 45 {
                // A scope that stopped inside the window.
                fixture.push(checkpoint(45, "owner.b", "agent.1"));
            }
        }

        let stream = events(&fixture);
        let plan = plan(&stream, RETAINED_TURNS_V1, "2026-10-05T00:00:00Z").unwrap();

        let pruned_ids = plan
            .iter()
            .map(|pruned| pruned.event_id.clone())
            .collect::<BTreeSet<_>>();
        assert!(
            !pruned_ids.contains("event.checkpoint.owner.a.50"),
            "the newest checkpoint is the head a Chat resumes from"
        );
        assert!(
            !pruned_ids.contains("event.checkpoint.owner.c.3"),
            "owner.c's newest checkpoint is kept whatever its age"
        );
        assert!(
            !pruned_ids.contains("event.checkpoint.owner.b.45"),
            "owner.b's newest checkpoint is kept"
        );
        assert!(
            !pruned_ids.contains("event.model.50"),
            "the newest model-call input is kept"
        );
        for turn in 31..=50 {
            assert!(!pruned_ids.contains(&format!("event.checkpoint.owner.a.{turn}")));
            assert!(!pruned_ids.contains(&format!("event.model.{turn}")));
        }
        for turn in 1..=30 {
            assert!(pruned_ids.contains(&format!("event.checkpoint.owner.a.{turn}")));
            assert!(pruned_ids.contains(&format!("event.model.{turn}")));
        }
        assert!(
            !plan
                .iter()
                .any(|pruned| pruned.event_id.starts_with("event.message.")),
            "the conversation is never a retention candidate"
        );

        // The tombstone keeps identity and timing, drops only the heavy value,
        // and names what was dropped.
        let tombstoned = plan
            .iter()
            .find(|pruned| pruned.event_id == "event.checkpoint.owner.a.1")
            .expect("turn 1 is superseded");
        let payload = &tombstoned.payload;
        assert!(payload.get("snapshot").is_none());
        assert_eq!(payload["nodeId"], "agent.1");
        assert_eq!(payload["ownerKey"], "owner.a");
        assert_eq!(payload["requestId"], "request.1");
        assert_eq!(payload["pressureTokens"], 1001);
        let marker = &payload[PRUNED_PAYLOAD_FIELD];
        assert_eq!(marker["kind"], "context_checkpoint");
        assert_eq!(
            marker["retainedTurns"],
            u64::try_from(RETAINED_TURNS_V1).unwrap()
        );
        assert!(marker["bytes"].as_u64().unwrap() > 64);
        assert!(
            marker["digestBefore"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );

        let tombstoned_span = plan
            .iter()
            .find(|pruned| pruned.event_id == "event.model.1")
            .expect("turn 1's request is superseded");
        assert!(tombstoned_span.payload.get("input").is_none());
        assert_eq!(tombstoned_span.payload["spanId"], "span.model.1");
        assert_eq!(tombstoned_span.payload["turn"], 1);
        assert_eq!(tombstoned_span.payload["usage"]["inputTokens"], 1);
        assert_eq!(tombstoned_span.payload["hasInput"], true);
    }

    #[test]
    fn the_plan_is_idempotent_and_a_kept_payload_is_untouched() {
        let fixture = (1..=40)
            .flat_map(|turn| [checkpoint(turn, "owner.a", "agent.1"), model_call(turn)])
            .collect::<Vec<_>>();
        let stream = events(&fixture);
        let first = plan(&stream, RETAINED_TURNS_V1, "now").unwrap();
        assert!(!first.is_empty());

        // Apply the plan, then plan again: nothing new to prune, and the kept
        // payload is byte-identical to what it was.
        let mut applied = fixture.clone();
        for pruned in &first {
            let (_, _, payload) = applied
                .iter_mut()
                .find(|(event_id, _, _)| event_id == &pruned.event_id)
                .expect("planned event exists");
            assert!(is_pruned(&pruned.payload));
            *payload = pruned.payload.clone();
        }
        let kept_before = fixture
            .iter()
            .find(|(event_id, _, _)| event_id == "event.checkpoint.owner.a.40")
            .unwrap()
            .2
            .clone();
        let second = plan(&events(&applied), RETAINED_TURNS_V1, "later").unwrap();
        assert!(second.is_empty(), "a tombstone is not prunable: {second:?}");
        let kept_after = applied
            .iter()
            .find(|(event_id, _, _)| event_id == "event.checkpoint.owner.a.40")
            .unwrap()
            .2
            .clone();
        assert_eq!(kept_before, kept_after);
    }

    #[test]
    fn a_chat_within_the_window_is_never_pruned() {
        let fixture = (1..=RETAINED_TURNS_V1 as u64)
            .flat_map(|turn| [checkpoint(turn, "owner.a", "agent.1"), model_call(turn)])
            .collect::<Vec<_>>();
        let plan = plan(&events(&fixture), RETAINED_TURNS_V1, "now").unwrap();
        assert!(plan.is_empty(), "a short Chat keeps every snapshot");
    }
}
