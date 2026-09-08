//! Optional workspace guidance: bounded filesystem observations, faithful Harness
//! rendering, and reconciliation against authentic model-visible event references.
//! This library never commits history or grants filesystem authority.

mod config;
mod files;
mod render;
mod state;
#[cfg(test)]
mod tests;

pub use config::Configuration;
pub use files::{FileObservation, InstructionFiles, ProjectInstructionFiles};
pub use render::{Action, Change, RenderItem, Rendered, render};
pub use state::{Event, Owner, Preparation, Selection, prepare};

use sha1::{Digest, Sha1};

/// Reference-compatible change fingerprint; not an authority or security token.
pub fn digest(text: &str) -> String {
    format!("{:x}", Sha1::digest(text.as_bytes()))
}

fn check_cancelled(cancellation: &crate::CancellationToken) -> Result<(), String> {
    if cancellation.is_cancelled() { Err("workspace instruction preparation cancelled".into()) }
    else { Ok(()) }
}
