//! Standing permissions identify execution authority, not provider-facing prose.
//! Invocation/replay identities still hash the complete immutable binding.
use crate::runtime::tool_loop::StoredFileToolBindingV1;

pub(super) fn native_process(capability: &str) -> bool {
    matches!(
        capability,
        "tool.shell.host" | "tool.python.host" | "tool.shell.start" | "tool.python.start"
    )
}

pub(crate) fn binding_hash(binding: &StoredFileToolBindingV1) -> String {
    if !native_process(&binding.capability_id) {
        return super::digest(binding);
    }
    let mut authority = binding.clone();
    authority.description.clear();
    authority.options.instructions = None;
    // Keep executable, schema, configuration, limits, approval settings, secret
    // references and every other field. New fields default to authority-bearing.
    format!("execution-v1:{}", super::digest(&authority))
}
