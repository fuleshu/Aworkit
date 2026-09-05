//! User-owned approval preferences and durable, narrowly scoped grants.
//!
//! Approval grants never change a tool's execution authority. The broker still
//! checks the exact frozen capability and records a one-use invocation decision.

pub(crate) mod reviewer;
mod store;
#[cfg(test)]
mod tests;
pub(crate) use reviewer::review_action;
pub(crate) use store::ApprovalStore;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    #[default]
    AskForApproval,
    ApproveForMe,
    FullAccess,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovalSettings {
    #[serde(default)]
    pub default_mode: ApprovalMode,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ApprovalContext {
    pub mode: ApprovalMode,
    /// Absent for projectless chats. Identity includes the native workspace.
    pub project_key: Option<String>,
    pub project_name: Option<String>,
    pub chat_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalChoice {
    ApproveOnce,
    AlwaysApproveInProject,
    Deny,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovalResolution {
    pub choice: ApprovalChoice,
    #[serde(default)]
    pub reason: Option<String>,
}

impl ApprovalResolution {
    pub fn once(approved: bool) -> Self {
        Self {
            choice: if approved {
                ApprovalChoice::ApproveOnce
            } else {
                ApprovalChoice::Deny
            },
            reason: None,
        }
    }

    pub fn approved(&self) -> bool {
        self.choice != ApprovalChoice::Deny
    }

    pub fn validate(&self) -> Result<(), String> {
        if self
            .reason
            .as_ref()
            .is_some_and(|reason| reason.len() > 4096)
        {
            return Err("Approval reason must be at most 4096 bytes.".into());
        }
        if self.approved() && self.reason.is_some() {
            return Err("Only a denial accepts a reason.".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectApprovalGrant {
    pub id: String,
    pub project_key: String,
    pub project_name: String,
    pub capability_id: String,
    pub scope: String,
    pub action_summary: String,
    pub binding_hash: String,
    pub action_hash: String,
}

/// File tools are confined to the selected project; other tools retain exact
/// arguments. In particular, shell and Python never receive wildcard grants.
pub(crate) fn action_scope(capability: &str, arguments: &Value) -> (String, String) {
    if matches!(capability, "tool.files.edit" | "tool.files.write") {
        ("Files in this project".into(), "project_files".into())
    } else {
        (
            "This exact action in this project".into(),
            digest(arguments),
        )
    }
}

pub(crate) fn digest(value: &impl Serialize) -> String {
    // serde_json's default map is sorted, including nested object keys.
    let value = serde_json::to_value(value).expect("approval values are serializable");
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&value).expect("JSON value"))
    )
}
