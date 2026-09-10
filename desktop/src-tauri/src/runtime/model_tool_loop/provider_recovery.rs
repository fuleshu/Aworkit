//! Recovery notices for incomplete provider responses, never tool re-execution.

use aworkit_capability_host::ProviderError;

use super::PROVIDER_TIMEOUT_NOTICE;

/// Only known response interruptions qualify. The gateway discards their
/// partial output; retries retain the same frozen route and settled exchanges.
pub(crate) fn provider_recovery_notice(error: &ProviderError) -> Option<&'static str> {
    match error {
        ProviderError::RequestTimedOut => Some(PROVIDER_TIMEOUT_NOTICE),
        ProviderError::StreamInterrupted => Some(
            "Aworkit recovery notice: the previous provider response stream was interrupted. \
             Its partial response was discarded and no tools from that attempt were executed. \
             Continue the task using the conversation and completed tool results available here.",
        ),
        _ => None,
    }
}
