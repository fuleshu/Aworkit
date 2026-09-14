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
        ProviderError::TransportFailed => Some(
            "Aworkit recovery notice: the previous provider connection failed before a response \
             was received. No tools from that attempt were executed. Continue the task using the \
             conversation and completed tool results available here.",
        ),
        ProviderError::InvalidToolCall => Some(
            "Aworkit recovery notice: the previous response contained a tool call that does not \
             match any available tool. Retry using only the exact tool names listed in the tool \
             definitions, with the arguments each tool expects.",
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_provider_failures_are_recoverable_but_cancellation_is_not() {
        // A dropped stream, a transport failure, and a timed-out request retry
        // with a recovery notice instead of aborting the Agent node.
        assert!(provider_recovery_notice(&ProviderError::RequestTimedOut).is_some());
        assert!(provider_recovery_notice(&ProviderError::StreamInterrupted).is_some());
        assert!(provider_recovery_notice(&ProviderError::TransportFailed).is_some());
        assert!(provider_recovery_notice(&ProviderError::InvalidToolCall).is_some());
        // Generic provider failures and cancellation are deliberate stops.
        assert!(provider_recovery_notice(&ProviderError::Failed("tool request is invalid".into())).is_none());
        assert!(provider_recovery_notice(&ProviderError::Cancelled).is_none());
    }
}
