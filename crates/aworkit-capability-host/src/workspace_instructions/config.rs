//! Frozen, bounded loader configuration. Invalid candidate names are inert.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Configuration {
    pub aworkit_home: PathBuf,
    pub project_root_markers: Vec<String>,
    pub instruction_file_candidates: Vec<String>,
    pub local_instruction_file_candidates: Vec<String>,
    pub max_bytes: usize,
    pub max_source_bytes: usize,
}

impl Configuration {
    /// Resolve environment-dependent paths once, at Chat freeze.
    pub fn resolve(mut self, home: Option<&Path>) -> Result<Self, String> {
        let value = self.aworkit_home.to_string_lossy();
        self.aworkit_home = if value.is_empty() {
            home.ok_or("user home unavailable; configure Aworkit home")?.join(".aworkit")
        } else if value == "~" || value.starts_with("~/") || value.starts_with("~\\") {
            home.ok_or("user home unavailable; configure an absolute Aworkit home")?
                .join(value.get(2..).unwrap_or_default())
        } else { self.aworkit_home };
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.aworkit_home.is_absolute() || self.aworkit_home.to_string_lossy().len() > 4096 {
            return Err("Aworkit home must be an absolute bounded path".into());
        }
        if self.max_bytes > 65536 || self.max_source_bytes == 0 || self.max_source_bytes > 1048576 {
            return Err("instruction budget must be 0..65536 and source limit 1..1048576 bytes".into());
        }
        for list in [&self.project_root_markers, &self.instruction_file_candidates,
            &self.local_instruction_file_candidates] {
            if list.len() > 64 || list.iter().any(|s| s.len() > 255) {
                return Err("instruction candidate lists exceed 64 names or 255 bytes per name".into());
            }
        }
        Ok(())
    }

    pub fn diagnostics(&self) -> Vec<String> {
        self.project_root_markers.iter().chain(&self.instruction_file_candidates)
            .chain(&self.local_instruction_file_candidates).filter(|name| !valid_name(name))
            .map(|name| format!("Ignored unusable instruction candidate: {name:?}")).collect()
    }

    pub(super) fn candidates(&self) -> impl Iterator<Item = &str> {
        self.instruction_file_candidates.iter().chain(&self.local_instruction_file_candidates)
            .map(String::as_str).filter(|s| valid_name(s))
    }
}

pub(super) fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".."
        && !name.contains(['/', '\\', '\0', ':'])
}
