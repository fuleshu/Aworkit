//! Recent-first history windows. Support facts keep partial spans truthful;
//! omitted history is never represented as a fully replayed event prefix.
use super::*;
use crate::runtime::{ChatEventPage, ChatEventWindow};

impl ChatHistory {
    pub(crate) fn feed_page(
        &self,
        after: u64,
        before: Option<u64>,
        head: u64,
    ) -> Result<ChatEventPage, String> {
        let identity = self.selected_identity()?;
        if head > self.head_for_chat(&identity.chat_id)? || after > head || before == Some(0) {
            return Err("Chat page cursor is outside the committed stream".into());
        }
        let chat = identity.chat_id.as_str();
        let through = before
            .map(|cursor| cursor.saturating_sub(1))
            .unwrap_or(head)
            .min(head);
        let rows = self
            .store
            .event_window(chat, BRANCH_ID, after.min(through), through, after == 0)
            .map_err(|e| e.to_string())?;
        let first = rows.first().map_or(through.saturating_add(1), |row| row.0);
        let last = rows.last().map_or(through, |row| row.0);
        let mut support = BTreeMap::new();
        let mut pending = rows
            .iter()
            .filter_map(|(_, event)| {
                event
                    .payload
                    .get("spanId")
                    .and_then(Value::as_str)
                    .map(|id| (id.to_owned(), true))
            })
            .collect::<Vec<_>>();
        let mut visited = BTreeMap::new();
        while let Some((span, content)) = pending.pop() {
            if visited
                .get(&span)
                .is_some_and(|previous| *previous || !content)
            {
                continue;
            }
            visited.insert(span.clone(), content);
            // Span inputs, earlier content and terminal facts are exact stored
            // records. Ancestors carry only lifecycle; unrelated children are
            // never pulled in by a long-running parent span.
            for (sequence, event) in self
                .store
                .span_window_support(chat, BRANCH_ID, &span, head, content)
                .map_err(|e| e.to_string())?
            {
                if let Some(parent) = event.payload.get("parentSpanId").and_then(Value::as_str) {
                    pending.push((parent.to_owned(), false));
                }
                if sequence < first || sequence > last {
                    support.insert(
                        sequence,
                        envelope(
                            chat,
                            BRANCH_ID,
                            sequence,
                            SemanticEventDraft::new(event.kind, event.payload),
                        ),
                    );
                }
            }
        }
        Ok(ChatEventPage {
            window: ChatEventWindow {
                first_sequence: first,
                last_sequence: last,
                head_sequence: head,
                has_more: first > 1 && through > 0,
                supporting_events: support.into_values().collect(),
            },
            events: rows
                .into_iter()
                .map(|(sequence, event)| {
                    envelope(
                        chat,
                        BRANCH_ID,
                        sequence,
                        SemanticEventDraft::new(event.kind, event.payload),
                    )
                })
                .collect(),
        })
    }

    /// One delegated child's own evidence, read from the same canonical Run
    /// history with the same bounded windowed semantics as the Chat feed.
    ///
    /// A child's facts carry its durable `subagentChildId`, so the window is
    /// filtered to that child while the raw scan still steps back by the same
    /// bounded page. A page may contain no child event at all: the caller keeps
    /// paging back until an earlier child activity appears or history is
    /// exhausted, exactly like an older load that only filled support.
    pub(crate) fn subagent_feed_page(
        &self,
        child_id: &str,
        before: Option<u64>,
        head: u64,
    ) -> Result<ChatEventPage, String> {
        let identity = self.selected_identity()?;
        if head > self.head_for_chat(&identity.chat_id)? || before == Some(0) {
            return Err("Chat page cursor is outside the committed stream".into());
        }
        let chat = identity.chat_id.as_str();
        let through = before
            .map(|cursor| cursor.saturating_sub(1))
            .unwrap_or(head)
            .min(head);
        let rows = self
            .store
            .event_window(chat, BRANCH_ID, 0, through, true)
            .map_err(|e| e.to_string())?;
        // The cursor is the oldest raw event this page scanned, so a page made
        // only of other scopes still advances toward the start of history.
        let first = rows.first().map_or(through, |row| row.0);
        let last = rows.last().map_or(through, |row| row.0);
        let child_rows = rows
            .iter()
            .filter(|(_, event)| {
                event.payload.get("subagentChildId").and_then(Value::as_str) == Some(child_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut support = BTreeMap::new();
        let mut pending = child_rows
            .iter()
            .filter_map(|(_, event)| {
                event
                    .payload
                    .get("spanId")
                    .and_then(Value::as_str)
                    .map(|id| (id.to_owned(), true))
            })
            .collect::<Vec<_>>();
        let mut visited = BTreeMap::new();
        while let Some((span, content)) = pending.pop() {
            if visited
                .get(&span)
                .is_some_and(|previous| *previous || !content)
            {
                continue;
            }
            visited.insert(span.clone(), content);
            for (sequence, event) in self
                .store
                .span_window_support(chat, BRANCH_ID, &span, head, content)
                .map_err(|e| e.to_string())?
            {
                if let Some(parent) = event.payload.get("parentSpanId").and_then(Value::as_str) {
                    pending.push((parent.to_owned(), false));
                }
                if sequence < first || sequence > last {
                    support.insert(
                        sequence,
                        envelope(
                            chat,
                            BRANCH_ID,
                            sequence,
                            SemanticEventDraft::new(event.kind, event.payload),
                        ),
                    );
                }
            }
        }
        Ok(ChatEventPage {
            window: ChatEventWindow {
                first_sequence: first,
                last_sequence: last,
                head_sequence: head,
                has_more: first > 1 && through > 0,
                supporting_events: support.into_values().collect(),
            },
            events: child_rows
                .into_iter()
                .map(|(sequence, event)| {
                    envelope(
                        chat,
                        BRANCH_ID,
                        sequence,
                        SemanticEventDraft::new(event.kind, event.payload),
                    )
                })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::semantic_events::noop_committed_event_port;

    #[test]
    fn recent_pages_skip_large_old_records_and_recover_exact_history() {
        let root = tempfile::tempdir().unwrap();
        let history =
            ChatHistory::open_with_committed_events(root.path(), noop_committed_event_port())
                .unwrap();
        let identity = history.selected_identity().unwrap();
        let commit = |head, drafts: Vec<(&str, Value)>| {
            history
                .store
                .commit(&CommitBatch {
                    chat_id: identity.chat_id.to_string(),
                    branch_id: BRANCH_ID.into(),
                    expected_head: head,
                    events: drafts
                        .into_iter()
                        .enumerate()
                        .map(|(i, (kind, payload))| Event {
                            event_id: event_identity(
                                identity.chat_id.as_str(),
                                BRANCH_ID,
                                head + i as u64 + 1,
                            ),
                            kind: kind.into(),
                            payload,
                        })
                        .collect(),
                    attempt: None,
                    checkpoint: None,
                    deduplication: None,
                    outbox: vec![],
                })
                .unwrap()
        };
        let mut drafts = vec![
            (
                "context.checkpoint",
                json!({"large": "x".repeat(5 * 1024 * 1024)}),
            ),
            ("span.started", json!({"spanId":"parent","createdAt":"12"})),
            (
                "span.started",
                json!({"spanId":"child","parentSpanId":"parent","input":{"exact":"完整😀"}}),
            ),
            (
                "span.content_delta",
                json!({"spanId":"child","delta":"earlier output"}),
            ),
        ];
        drafts.extend((0..300).map(|i| ("message.user", json!({"body":i.to_string()}))));
        drafts.push(("span.completed", json!({"spanId":"child","createdAt":"23"})));
        commit(0, drafts);
        let metadata = history.snapshot(u64::MAX).unwrap();
        let full = history.snapshot(0).unwrap();
        assert!(metadata.events.is_empty());
        assert_eq!(metadata.state_hash, full.state_hash);
        assert_eq!(
            serde_json::to_value(metadata.history).unwrap(),
            serde_json::to_value(full.history).unwrap()
        );
        commit(305, vec![("message.user", json!({"body":"later"}))]);
        let page = history.feed_page(0, None, 305).unwrap();
        assert_eq!(page.events.len(), 128);
        assert_eq!(page.window.first_sequence, 178);
        assert_eq!(page.window.last_sequence, 305);
        assert!(page.window.has_more);
        assert_eq!(page.window.supporting_events, full.events[1..4]);
        let mut recovered = page.events;
        while recovered[0].sequence > 1 {
            let mut older = history
                .feed_page(0, Some(recovered[0].sequence), 305)
                .unwrap()
                .events;
            assert!(!older.is_empty());
            assert_eq!(older.last().unwrap().sequence + 1, recovered[0].sequence);
            older.extend(recovered);
            recovered = older;
        }
        assert_eq!(recovered, full.events);
        assert!(history.feed_page(305, None, 305).unwrap().events.is_empty());
        assert!(history.feed_page(0, None, 307).is_err());
        assert!(history.feed_page(306, None, 305).is_err());
    }

    #[test]
    fn a_child_page_returns_one_child_scope_and_still_steps_back() {
        let root = tempfile::tempdir().unwrap();
        let history =
            ChatHistory::open_with_committed_events(root.path(), noop_committed_event_port())
                .unwrap();
        let identity = history.selected_identity().unwrap();
        let commit = |head, drafts: Vec<(&str, Value)>| {
            history
                .store
                .commit(&CommitBatch {
                    chat_id: identity.chat_id.to_string(),
                    branch_id: BRANCH_ID.into(),
                    expected_head: head,
                    events: drafts
                        .into_iter()
                        .enumerate()
                        .map(|(i, (kind, payload))| Event {
                            event_id: event_identity(
                                identity.chat_id.as_str(),
                                BRANCH_ID,
                                head + i as u64 + 1,
                            ),
                            kind: kind.into(),
                            payload,
                        })
                        .collect(),
                    attempt: None,
                    checkpoint: None,
                    deduplication: None,
                    outbox: vec![],
                })
                .unwrap()
        };
        let mut drafts = vec![
            ("message.user", json!({"body":"parent"})),
            (
                "span.started",
                json!({"spanId":"span.run.child.a","subagentChildId":"child.a"}),
            ),
            (
                "span.started",
                json!({"spanId":"span.model.a","parentSpanId":"span.run.child.a","subagentChildId":"child.a"}),
            ),
            (
                "span.content_delta",
                json!({"spanId":"span.model.a","append":"child a","subagentChildId":"child.a"}),
            ),
            (
                "span.started",
                json!({"spanId":"span.run.child.b","subagentChildId":"child.b"}),
            ),
            (
                "span.content_delta",
                json!({"spanId":"span.run.child.b","append":"child b","subagentChildId":"child.b"}),
            ),
        ];
        drafts.extend((0..200).map(|i| ("message.user", json!({"body":i.to_string()}))));
        commit(0, drafts);

        // The newest raw page is entirely parent activity, so the child scope
        // is empty but the cursor still advances toward the start of history.
        let page = history
            .subagent_feed_page("child.a", None, 206)
            .unwrap();
        assert!(page.events.is_empty());
        assert_eq!(page.window.first_sequence, 79);
        assert_eq!(page.window.last_sequence, 206);
        assert!(page.window.has_more);
        // The older page carries only child.a, never child.b or the parent.
        let older = history
            .subagent_feed_page("child.a", Some(page.window.first_sequence), 206)
            .unwrap();
        assert_eq!(
            older
                .events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![2, 3, 4]
        );
        assert_eq!(older.window.first_sequence, 1);
        assert!(!older.window.has_more);
        let other = history
            .subagent_feed_page("child.b", None, 6)
            .unwrap();
        assert_eq!(
            other
                .events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![5, 6]
        );
        assert!(
            history
                .subagent_feed_page("child.unknown", None, 6)
                .unwrap()
                .events
                .is_empty()
        );
    }
}
