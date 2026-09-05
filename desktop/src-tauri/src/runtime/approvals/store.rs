//! SQLite persistence shared by the desktop controls and tool authority.

use super::{ApprovalMode, ApprovalResolution, ProjectApprovalGrant};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone)]
pub(crate) struct ApprovalStore {
    database: PathBuf,
}

impl ApprovalStore {
    pub fn open(database: &Path) -> Result<Self, String> {
        if let Some(parent) = database.parent() {
            std::fs::create_dir_all(parent).map_err(error)?;
        }
        let store = Self {
            database: database.to_owned(),
        };
        store.connection()?.execute_batch("
            CREATE TABLE IF NOT EXISTS approval_chat_modes (chat_id TEXT PRIMARY KEY, mode TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS approval_project_grants (id TEXT PRIMARY KEY, body TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS approval_resolutions (decision_id TEXT PRIMARY KEY, body TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS approval_reviews (invocation_id TEXT PRIMARY KEY, body TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS approval_results (decision_id TEXT PRIMARY KEY, body TEXT NOT NULL);
        ").map_err(error)?;
        Ok(store)
    }

    fn connection(&self) -> Result<Connection, String> {
        let connection = Connection::open(&self.database).map_err(error)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(error)?;
        Ok(connection)
    }

    pub fn mode(&self, chat_id: &str, fallback: ApprovalMode) -> Result<ApprovalMode, String> {
        let value: Option<String> = self
            .connection()?
            .query_row(
                "SELECT mode FROM approval_chat_modes WHERE chat_id=?1",
                [chat_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)?;
        value
            .map(|value| serde_json::from_str(&value).map_err(error))
            .transpose()
            .map(|mode| mode.unwrap_or(fallback))
    }

    pub fn set_mode(&self, chat_id: &str, mode: ApprovalMode) -> Result<(), String> {
        self.connection()?.execute("INSERT INTO approval_chat_modes VALUES (?1,?2) ON CONFLICT(chat_id) DO UPDATE SET mode=excluded.mode",
            params![chat_id, serde_json::to_string(&mode).map_err(error)?]).map_err(error)?;
        Ok(())
    }

    pub fn grants(&self) -> Result<Vec<ProjectApprovalGrant>, String> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT body FROM approval_project_grants ORDER BY id")
            .map_err(error)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(error)?;
        rows.map(|row| serde_json::from_str(&row.map_err(error)?).map_err(error))
            .collect()
    }

    pub fn revoke(&self, id: &str) -> Result<(), String> {
        self.connection()?
            .execute("DELETE FROM approval_project_grants WHERE id=?1", [id])
            .map_err(error)?;
        Ok(())
    }

    pub fn resolution(&self, decision_id: &str) -> Result<Option<ApprovalResolution>, String> {
        let value: Option<String> = self
            .connection()?
            .query_row(
                "SELECT body FROM approval_resolutions WHERE decision_id=?1",
                [decision_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)?;
        value
            .map(|value| serde_json::from_str(&value).map_err(error))
            .transpose()
    }

    /// The user's one-use decision and optional project rule commit together.
    /// A crash/retry cannot change the original decision or broaden its grant.
    pub fn resolve(
        &self,
        decision_id: &str,
        resolution: &ApprovalResolution,
        grant: Option<&ProjectApprovalGrant>,
    ) -> Result<(), String> {
        resolution.validate()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction().map_err(error)?;
        let body = serde_json::to_string(resolution).map_err(error)?;
        let existing: Option<String> = transaction
            .query_row(
                "SELECT body FROM approval_resolutions WHERE decision_id=?1",
                [decision_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)?;
        if let Some(existing) = existing {
            if existing != body {
                return Err("This approval already has a different decision.".into());
            }
            return Ok(());
        }
        transaction
            .execute(
                "INSERT INTO approval_resolutions VALUES (?1,?2)",
                params![decision_id, body],
            )
            .map_err(error)?;
        if let Some(grant) = grant {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO approval_project_grants VALUES (?1,?2)",
                    params![grant.id, serde_json::to_string(grant).map_err(error)?],
                )
                .map_err(error)?;
        }
        transaction.commit().map_err(error)
    }

    pub fn review(
        &self,
        invocation_id: &str,
    ) -> Result<Option<super::reviewer::ReviewDecision>, String> {
        let value: Option<String> = self
            .connection()?
            .query_row(
                "SELECT body FROM approval_reviews WHERE invocation_id=?1",
                [invocation_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)?;
        value
            .map(|value| serde_json::from_str(&value).map_err(error))
            .transpose()
    }

    pub fn result(
        &self,
        decision_id: &str,
    ) -> Result<Option<crate::runtime::WorkflowExecutionResultV1>, String> {
        let value: Option<String> = self
            .connection()?
            .query_row(
                "SELECT body FROM approval_results WHERE decision_id=?1",
                [decision_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)?;
        value
            .map(|value| serde_json::from_str(&value).map_err(error))
            .transpose()
    }

    pub fn save_result(
        &self,
        decision_id: &str,
        result: &crate::runtime::WorkflowExecutionResultV1,
    ) -> Result<(), String> {
        self.connection()?
            .execute(
                "INSERT OR IGNORE INTO approval_results VALUES (?1,?2)",
                params![decision_id, serde_json::to_string(result).map_err(error)?],
            )
            .map_err(error)?;
        Ok(())
    }

    pub fn save_review(
        &self,
        invocation_id: &str,
        decision: &super::reviewer::ReviewDecision,
    ) -> Result<(), String> {
        self.connection()?
            .execute(
                "INSERT OR IGNORE INTO approval_reviews VALUES (?1,?2)",
                params![
                    invocation_id,
                    serde_json::to_string(decision).map_err(error)?
                ],
            )
            .map_err(error)?;
        Ok(())
    }
}

fn error(error: impl std::fmt::Display) -> String {
    format!("Approval store: {error}")
}
