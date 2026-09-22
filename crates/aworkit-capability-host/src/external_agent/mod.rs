//! Configured external-agent lifecycle adapters, opaque session correlation,
//! and the one-shot subagent backend seam.

mod claude_code;
mod codex_one_shot;
mod contracts;
mod manager;
mod provider;

pub use claude_code::*;
pub use codex_one_shot::*;
pub use contracts::*;
pub use manager::*;
pub use provider::*;
