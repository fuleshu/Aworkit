//! Incremental, immutable history views. A commit advances existing views instead
//! of invalidating them. Readers holding an older snapshot share its payloads;
//! a concurrent writer is reconciled by reading only its committed tail.
use super::*;

/// Retain recent Chat views across workers and navigation. Busy views are never
/// evicted; at most four otherwise idle Chats stay warm. Each Chat has its own
/// lock, so a large Chat cannot serialize a sibling's context preparation.
#[derive(Default)]
pub(super) struct StreamCaches(Mutex<RecentStreams>);

#[derive(Default)]
struct RecentStreams {
    tick: u64,
    entries: BTreeMap<String, (u64, Arc<Mutex<StreamCache>>)>,
}

impl StreamCaches {
    pub(super) fn get(&self, chat: &str) -> Arc<Mutex<StreamCache>> {
        let mut recent = self.0.lock().unwrap_or_else(|p| p.into_inner());
        recent.tick = recent.tick.saturating_add(1);
        let tick = recent.tick;
        let entry = recent.entries.entry(chat.into()).or_default();
        entry.0 = tick;
        let selected = entry.1.clone();
        let mut dropped = Vec::new();
        while recent.entries.len() > 4 {
            let oldest = recent
                .entries
                .iter()
                .filter(|(_, (_, cache))| Arc::strong_count(cache) == 1)
                .min_by_key(|(_, (used, _))| used)
                .map(|(key, _)| key.clone());
            let Some(oldest) = oldest else { break };
            dropped.push(recent.entries.remove(&oldest));
        }
        drop(recent);
        // Potentially large payloads are released outside the registry lock.
        drop(dropped);
        selected
    }
}

#[derive(Default)]
pub(super) struct StreamCache {
    pub(super) chat_id: Option<String>,
    pub(super) head: u64,
    pub(super) spans: SpanLedgerState,
    pub(super) decoded: DecodedStream,
}

#[derive(Default)]
pub(super) struct DecodedStream {
    pub(super) events: Arc<Vec<Arc<Event>>>,
    pub(super) envelopes: Option<super::super::semantic_events::SharedEvents>,
}

impl StreamCache {
    /// Advance from a durable sequence fence. Replacement/truncation is a cold
    /// recovery boundary; ordinary appends never reread historical payloads.
    pub(super) fn synchronize(
        &mut self,
        store: &LocalHistoryStore,
        chat: &str,
        head: u64,
    ) -> Result<(), String> {
        if self.chat_id.as_deref() != Some(chat) || head < self.head {
            *self = Self {
                chat_id: Some(chat.into()),
                ..Self::default()
            };
        }
        while self.head < head {
            let page = store
                .event_window(chat, BRANCH_ID, self.head, head, false)
                .map_err(|e| format!("cannot read committed Chat tail: {e}"))?;
            if page.is_empty() {
                return Err("Committed Chat tail is incomplete".into());
            }
            for (sequence, event) in page {
                if sequence != self.head + 1 {
                    return Err("Committed Chat tail is not contiguous".into());
                }
                observe_existing_span(&mut self.spans, &event.kind, &event.payload);
                if let Some(envelopes) = &mut self.decoded.envelopes {
                    Arc::make_mut(envelopes).push(Arc::new(envelope(
                        chat,
                        BRANCH_ID,
                        sequence,
                        SemanticEventDraft::new(event.kind.clone(), event.payload.clone()),
                    )));
                }
                Arc::make_mut(&mut self.decoded.events).push(Arc::new(event));
                self.head = sequence;
            }
        }
        Ok(())
    }

    /// Called only after durable commit succeeds. Previously returned snapshots
    /// stay immutable; Arc::make_mut copies pointers rather than JSON trees.
    pub(super) fn append_committed(&mut self, events: &[CoreEventEnvelope], head: u64) {
        for event in events {
            Arc::make_mut(&mut self.decoded.events).push(Arc::new(Event {
                event_id: event.event_id.clone(),
                kind: event.kind.clone(),
                payload: event.payload.clone(),
            }));
            if let Some(envelopes) = &mut self.decoded.envelopes {
                Arc::make_mut(envelopes).push(Arc::new(event.clone()));
            }
        }
        self.head = head;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::semantic_events::noop_committed_event_port;

    #[test]
    fn recent_chats_stay_warm_and_active_chats_are_not_evicted() {
        let caches = StreamCaches::default();
        let active = caches.get("active");
        for n in 0..10 {
            drop(caches.get(&format!("idle.{n}")));
        }
        assert!(Arc::ptr_eq(&active, &caches.get("active")));
        assert_eq!(caches.0.lock().unwrap().entries.len(), 4);
        let idle = caches.get("idle.9");
        assert!(Arc::ptr_eq(&idle, &caches.get("idle.9")));
    }

    #[test]
    fn snapshots_share_payloads_across_local_and_external_appends() {
        let root = tempfile::tempdir().unwrap();
        let history =
            ChatHistory::open_with_committed_events(root.path(), noop_committed_event_port())
                .unwrap();
        history
            .commit(vec![SemanticEventDraft::new(
                "context.checkpoint",
                json!({"body":"large context".repeat(100_000)}),
            )])
            .unwrap();
        let before = history.committed_events_shared().unwrap();
        let hit = history.committed_events_shared().unwrap();
        assert!(Arc::ptr_eq(&before, &hit), "unchanged head is a cache hit");
        history
            .commit(vec![SemanticEventDraft::new(
                "context.usage",
                json!({"tokens":1}),
            )])
            .unwrap();
        let after = history.committed_events_shared().unwrap();
        assert_eq!(before.len(), 1, "old snapshot remains immutable");
        assert_eq!(after.len(), 2);
        assert!(
            Arc::ptr_eq(&before[0], &after[0]),
            "append must not clone historical JSON"
        );
        let other =
            ChatHistory::open_with_committed_events(root.path(), noop_committed_event_port())
                .unwrap();
        other
            .commit(vec![SemanticEventDraft::new(
                "context.edited",
                json!({"nodeId":"other"}),
            )])
            .unwrap();
        let reconciled = history.committed_events_shared().unwrap();
        assert_eq!(reconciled.len(), 3);
        assert!(
            Arc::ptr_eq(&after[0], &reconciled[0]),
            "foreign append must read only the tail"
        );
        assert_eq!(after.len(), 2);
    }
}
