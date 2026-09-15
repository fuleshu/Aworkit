use super::{ApprovalStore, FilesystemGrant};

impl ApprovalStore {
    pub fn filesystem_grants(&self) -> Result<Vec<FilesystemGrant>, String> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT body FROM approval_filesystem_grants ORDER BY id")
            .map_err(|e| e.to_string())?;
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .map(|row| {
                serde_json::from_str(&row.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
            })
            .collect()
    }

    pub fn revoke_filesystem(&self, id: &str) -> Result<(), String> {
        self.connection()?
            .execute("DELETE FROM approval_filesystem_grants WHERE id=?1", [id])
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
