//! Wire-only DTOs for the Management repair Tauri boundary.

use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementRepairProjectionDto {
    pub version: u64,
    pub last_sequence: u64,
    pub events: Vec<RepairEventDto>,
    pub chat: ManagementChatDto,
    pub error_groups: Vec<ErrorGroupDto>,
    pub investigation: Option<Value>,
    pub candidates: Vec<Value>,
    pub capability_reports: Vec<Value>,
    pub evidence: Vec<Value>,
    pub restart_recovery: Option<Value>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementChatDto {
    /// Present only when a committed investigation supplies the exact Chat.
    pub id: Option<String>,
    pub title: String,
    pub scope: String,
    pub maintainer_tier: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorGroupDto {
    pub id: String,
    pub fingerprint: String,
    pub title: String,
    pub occurrence_count: usize,
    pub chat_count: usize,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub last_repair_at: Option<String>,
    pub state: &'static str,
    pub evidence_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairEventDto {
    pub sequence: u64,
    pub kind: &'static str,
    pub occurred_at: String,
    pub subject_id: String,
}
