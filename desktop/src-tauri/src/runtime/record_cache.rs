//! Disposable read index for append-only operational records. The durable head
//! fences every read; an append decodes only the tail. Selection borrows cached
//! records and copies only matches, so unrelated tool results never enter a
//! lookup or get deserialized again during ordinary continuation.
use aworkit_local_store::{Event, LocalHistoryStore};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Default)]
pub(super) struct RecordCache(Arc<Mutex<Index>>);

#[derive(Default)]
struct Index {
    stream: Option<(String, String)>,
    head: u64,
    events: Vec<Arc<Event>>,
    kinds: BTreeMap<String, Vec<Arc<Event>>>,
}

impl RecordCache {
    pub(super) fn select<T>(
        &self,
        store: &LocalHistoryStore,
        chat: &str,
        branch: &str,
        kind: Option<&str>,
        mut select: impl FnMut(&Event) -> Option<T>,
    ) -> Result<Vec<T>, String> {
        let mut index = self.0.lock().map_err(|_| "record cache lock poisoned")?;
        let head = store
            .head_sequence(chat, branch)
            .map_err(|e| e.to_string())?
            .unwrap_or(0);
        if index
            .stream
            .as_ref()
            .is_none_or(|(c, b)| c != chat || b != branch)
            || head < index.head
        {
            *index = Index {
                stream: Some((chat.into(), branch.into())),
                ..Index::default()
            };
        }
        while index.head < head {
            let page = store
                .event_window(chat, branch, index.head, head, false)
                .map_err(|e| e.to_string())?;
            if page.is_empty() {
                return Err("Committed record tail is incomplete".into());
            }
            for (sequence, event) in page {
                if sequence != index.head + 1 {
                    return Err("Committed record tail is not contiguous".into());
                }
                let event = Arc::new(event);
                index
                    .kinds
                    .entry(event.kind.clone())
                    .or_default()
                    .push(event.clone());
                index.events.push(event);
                index.head = sequence;
            }
        }
        Ok(if let Some(kind) = kind {
            index
                .kinds
                .get(kind)
                .into_iter()
                .flatten()
                .filter_map(|e| select(e))
                .collect()
        } else {
            index.events.iter().filter_map(|e| select(e)).collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aworkit_local_store::CommitBatch;
    use serde_json::json;

    #[test]
    fn kind_queries_and_foreign_tails_preserve_record_order_and_identity() {
        let root = tempfile::tempdir().unwrap();
        let store = LocalHistoryStore::open(root.path().join("records.sqlite3")).unwrap();
        let append = |head, kind: &str| {
            store
                .commit(&CommitBatch {
                    chat_id: "records".into(),
                    branch_id: "main".into(),
                    expected_head: head,
                    events: vec![Event {
                        event_id: format!("event.{head}"),
                        kind: kind.into(),
                        payload: json!({"record":head}),
                    }],
                    attempt: None,
                    checkpoint: None,
                    deduplication: None,
                    outbox: vec![],
                })
                .unwrap()
        };
        append(0, "z.proposed");
        append(1, "a.authorized");
        let cache = RecordCache::default();
        let read = |kind| {
            cache
                .select(&store, "records", "main", kind, |e| {
                    Some(e.payload["record"].as_u64().unwrap())
                })
                .unwrap()
        };
        assert_eq!(
            read(None),
            vec![0, 1],
            "a kind index must not reorder the ledger"
        );
        let address = {
            let index = cache.0.lock().unwrap();
            Arc::as_ptr(&index.events[0])
        };
        append(2, "z.proposed");
        assert_eq!(read(Some("z.proposed")), vec![0, 2]);
        assert_eq!(read(None), vec![0, 1, 2]);
        assert_eq!(address, Arc::as_ptr(&cache.0.lock().unwrap().events[0]));
    }
}
