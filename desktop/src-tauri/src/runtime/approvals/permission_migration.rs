//! Upgrade existing native process grants using trusted frozen binding evidence.
//! Never infer permission from review outcomes, prose, or revoked receipts.
use super::{ProjectApprovalGrant, digest, permission_identity};
use crate::runtime::tool_loop::StoredFileToolBindingV1;
use rusqlite::{Connection, params};
use serde_json::Value;
use std::collections::BTreeMap;

pub(super) fn migrate(connection: &mut Connection) -> Result<(), String> {
    let transaction = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(error)?;
    let mut pending = BTreeMap::new();
    {
        let mut statement = transaction
            .prepare("SELECT id,body FROM approval_project_grants")
            .map_err(error)?;
        let mut rows = statement.query([]).map_err(error)?;
        while let Some(row) = rows.next().map_err(error)? {
            let id: String = row.get(0).map_err(error)?;
            let grant: ProjectApprovalGrant =
                serde_json::from_str(&row.get::<_, String>(1).map_err(error)?).map_err(error)?;
            if permission_identity::native_process(&grant.capability_id)
                && grant.binding_hash.len() == 64
                && grant.binding_hash.bytes().all(|b| b.is_ascii_hexdigit())
                && grant.id == id
                && grant.action_hash == "project_tool"
                && id == digest(&(&grant.project_key, &grant.binding_hash, &grant.action_hash))
            {
                pending.insert(
                    (grant.project_key.clone(), grant.binding_hash.clone()),
                    grant,
                );
            }
        }
    }
    if pending.is_empty() {
        return Ok(());
    }
    let has_history: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='semantic_events')", [], |row| row.get(0)).map_err(error)?;
    if !has_history {
        return Ok(());
    }
    // Avoid rescanning unchanged history for an unprovable legacy grant.
    transaction.execute_batch("CREATE TABLE IF NOT EXISTS approval_identity_migrations (fingerprint TEXT PRIMARY KEY)").map_err(error)?;
    let head: i64 = transaction.query_row("SELECT COALESCE(MAX(sequence),0) FROM semantic_events WHERE chat_id='pipeline.execution' AND branch_id='main' AND kind='pipeline.execution-prepared'", [], |row| row.get(0)).map_err(error)?;
    let fingerprint = digest(&(head, pending.values().collect::<Vec<_>>()));
    let done: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM approval_identity_migrations WHERE fingerprint=?1)",
            [&fingerprint],
            |row| row.get(0),
        )
        .map_err(error)?;
    if done {
        return Ok(());
    }
    let mut upgrades = Vec::new();
    {
        // Project and bindings only: no transcript, results, or model reasoning
        // are read into memory. The SQL kind index skips tool exchange history.
        let mut statement = transaction.prepare("SELECT json_extract(payload,'$.record.approvals.projectKey'),json_extract(payload,'$.record.toolBindings') FROM semantic_events WHERE chat_id='pipeline.execution' AND branch_id='main' AND kind='pipeline.execution-prepared' ORDER BY sequence").map_err(error)?;
        let mut rows = statement.query([]).map_err(error)?;
        while let Some(row) = rows.next().map_err(error)? {
            let project: Option<String> = row.get(0).map_err(error)?;
            let Some(project) = project else {
                continue;
            };
            if !pending.keys().any(|(p, _)| p == &project) {
                continue;
            }
            let bindings: Option<String> = row.get(1).map_err(error)?;
            let Some(bindings) = bindings else {
                continue;
            };
            let bindings: Vec<Value> = serde_json::from_str(&bindings).map_err(error)?;
            for value in bindings {
                if !value["capabilityId"]
                    .as_str()
                    .is_some_and(permission_identity::native_process)
                {
                    continue;
                }
                let key = (project.clone(), digest(&value));
                let Some(grant) = pending.get(&key) else {
                    continue;
                };
                // An exact legacy hash and a recognized frozen type are both
                // required. Unknown old formats stay unapproved, never guessed.
                let Ok(binding) = serde_json::from_value::<StoredFileToolBindingV1>(value) else {
                    continue;
                };
                if binding.capability_id != grant.capability_id {
                    continue;
                }
                let mut upgraded = pending.remove(&key).expect("matched grant");
                let old_id = upgraded.id.clone();
                upgraded.binding_hash = permission_identity::binding_hash(&binding);
                upgraded.set_tool_scope();
                upgrades.push((old_id, upgraded));
            }
            if pending.is_empty() {
                break;
            }
        }
    }
    for (old_id, grant) in upgrades {
        transaction
            .execute("DELETE FROM approval_project_grants WHERE id=?1", [&old_id])
            .map_err(error)?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO approval_project_grants VALUES (?1,?2)",
                params![grant.id, serde_json::to_string(&grant).map_err(error)?],
            )
            .map_err(error)?;
    }
    transaction
        .execute(
            "INSERT OR IGNORE INTO approval_identity_migrations VALUES (?1)",
            [fingerprint],
        )
        .map_err(error)?;
    transaction.commit().map_err(error)
}

fn error(error: impl std::fmt::Display) -> String {
    format!("Approval identity migration: {error}")
}
