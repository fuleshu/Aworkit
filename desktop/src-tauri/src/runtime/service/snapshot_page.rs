//! Desktop event recovery is pinned to the Chat and head returned by its snapshot.
use super::*;

/// A read-only query handle, captured briefly under the coordinator and then
/// used on a worker with independent SQLite read connections.
pub struct ChatFeedReader {
    history: ChatHistory,
}

impl ChatFeedReader {
    pub fn page(
        &self,
        after: u64,
        before: Option<u64>,
        head: u64,
    ) -> Result<crate::runtime::ChatEventPage, String> {
        self.history.feed_page(after, before, head)
    }

    /// One delegated child's own evidence from the same canonical history. The
    /// child is a presentation scope over one Run, never a second feed.
    pub fn child_page(
        &self,
        child_id: &str,
        before: Option<u64>,
        head: u64,
    ) -> Result<crate::runtime::ChatEventPage, String> {
        if child_id.is_empty() || child_id.len() > 256 {
            return Err("Subagent child identity is invalid".into());
        }
        self.history.subagent_feed_page(child_id, before, head)
    }
}

impl DesktopRuntime {
    pub fn chat_feed_reader(&self, chat_id: &str) -> Result<ChatFeedReader, String> {
        Ok(ChatFeedReader {
            history: self.history.for_chat_query(chat_id)?,
        })
    }
}
