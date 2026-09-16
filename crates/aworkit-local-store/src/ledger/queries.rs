//! Independent SQLite readers for interactive history queries. They never hold
//! the writer connection while decoding a page or waiting for another reader.
use super::*;

impl LocalHistoryStore {
    /// Timestamp projection without decoding the possibly large last payload.
    pub fn latest_event_time(
        &self,
        chat: &str,
        branch: &str,
    ) -> Result<Option<String>, StoreError> {
        validate_id(chat)?;
        validate_id(branch)?;
        let _lease = self.gate.shared()?;
        let connection = self.query_connection()?;
        let mut statement = connection.prepare("SELECT json_extract(payload,'$.createdAt') FROM semantic_events WHERE chat_id=?1 AND branch_id=?2 AND json_type(payload,'$.createdAt')='text' ORDER BY sequence DESC LIMIT 1")?;
        let mut rows = statement.query(params![chat, branch])?;
        rows.next()?
            .map(|row| row.get(0).map_err(StoreError::from))
            .transpose()
    }

    fn query_connection(&self) -> Result<Connection, StoreError> {
        let connection = Connection::open_with_flags(
            self.path.as_ref(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(connection)
    }

    /// Exact events in a bounded sequence window, returned in ascending order.
    /// A large individual event is returned intact so every page advances.
    pub fn event_window(
        &self,
        chat: &str,
        branch: &str,
        after: u64,
        through: u64,
        newest_first: bool,
    ) -> Result<Vec<(u64, Event)>, StoreError> {
        validate_id(chat)?;
        validate_id(branch)?;
        let _lease = self.gate.shared()?;
        let connection = self.query_connection()?;
        let order = if newest_first { "DESC" } else { "ASC" };
        let mut statement = connection.prepare(&format!("SELECT sequence,event_id,kind,payload FROM semantic_events WHERE chat_id=?1 AND branch_id=?2 AND sequence>?3 AND sequence<=?4 ORDER BY sequence {order} LIMIT 128"))?;
        let mut rows = statement.query(params![chat, branch, to_i64(after)?, to_i64(through)?])?;
        let mut events = Vec::new();
        let mut bytes = 0usize;
        while let Some(row) = rows.next()? {
            let payload: String = row.get(3)?;
            if !events.is_empty() && bytes.saturating_add(payload.len()) > 4 * 1024 * 1024 {
                break;
            }
            bytes = bytes.saturating_add(payload.len());
            events.push((
                from_i64(row.get(0)?)?,
                Event {
                    event_id: row.get(1)?,
                    kind: row.get(2)?,
                    payload: serde_json::from_str(&payload)?,
                },
            ));
        }
        if newest_first {
            events.reverse();
        }
        Ok(events)
    }

    /// Read a small semantic projection without decoding unrelated model inputs
    /// and checkpoints. Kinds are bound values, never interpolated SQL.
    pub fn events_of_kinds(
        &self,
        chat: &str,
        branch: &str,
        kinds: &[&str],
    ) -> Result<Vec<Event>, StoreError> {
        validate_id(chat)?;
        validate_id(branch)?;
        let _lease = self.gate.shared()?;
        let connection = self.query_connection()?;
        let mut statement = connection.prepare("SELECT event_id,kind,payload FROM semantic_events WHERE chat_id=?1 AND branch_id=?2 AND kind IN (SELECT value FROM json_each(?3)) ORDER BY sequence")?;
        let mut rows = statement.query(params![chat, branch, serde_json::to_string(kinds)?])?;
        let mut events = Vec::new();
        while let Some(row) = rows.next()? {
            let payload: String = row.get(2)?;
            events.push(Event {
                event_id: row.get(0)?,
                kind: row.get(1)?,
                payload: serde_json::from_str(&payload)?,
            });
        }
        Ok(events)
    }

    /// Exact span support for a feed boundary. Ancestors need lifecycle facts;
    /// spans intersecting the visible page also need their earlier content.
    pub fn span_window_support(
        &self,
        chat: &str,
        branch: &str,
        span: &str,
        through: u64,
        content: bool,
    ) -> Result<Vec<(u64, Event)>, StoreError> {
        validate_id(chat)?;
        validate_id(branch)?;
        let _lease = self.gate.shared()?;
        let connection = self.query_connection()?;
        let mut statement = connection.prepare("SELECT sequence,event_id,kind,payload FROM semantic_events WHERE chat_id=?1 AND branch_id=?2 AND kind GLOB 'span.*' AND json_extract(payload,'$.spanId')=?3 AND sequence<=?4 AND (?5 OR kind IN ('span.started','span.completed','span.failed','span.cancelled')) ORDER BY sequence")?;
        let mut rows = statement.query(params![chat, branch, span, to_i64(through)?, content])?;
        let mut events = Vec::new();
        while let Some(row) = rows.next()? {
            let payload: String = row.get(3)?;
            events.push((
                from_i64(row.get(0)?)?,
                Event {
                    event_id: row.get(1)?,
                    kind: row.get(2)?,
                    payload: serde_json::from_str(&payload)?,
                },
            ));
        }
        Ok(events)
    }
}
