//! Recovery notices for provider failures. Every provider failure that is not
//! caller cancellation is reported back to the model on the same frozen route,
//! so the Agent keeps working and the model decides when the task is done.
//! Nothing here replays a tool: a failed turn settles no tool calls.

use aworkit_capability_host::ProviderError;

use super::PROVIDER_TIMEOUT_NOTICE;

/// The model-visible notice for one provider failure, or `None` for caller
/// cancellation, which is the only stop the authority owns.
pub(crate) fn provider_recovery_notice(error: &ProviderError) -> Option<String> {
    let notice = match error {
        ProviderError::Cancelled => return None,
        // Ambiguous acceptance is a durable safety verdict, not a transient
        // failure: reporting it and retrying would replay a request that may
        // already have produced provider work.
        ProviderError::AcceptanceAmbiguous => return None,
        ProviderError::RequestTimedOut => PROVIDER_TIMEOUT_NOTICE.to_owned(),
        ProviderError::StreamInterrupted => {
            "Aworkit recovery notice: the previous provider response stream was interrupted. \
             Its partial response was discarded and no tools from that attempt were executed. \
             Continue the task using the conversation and completed tool results available here."
                .to_owned()
        }
        ProviderError::TransportFailed => {
            "Aworkit recovery notice: the previous provider connection failed before a response \
             was received. No tools from that attempt were executed. Continue the task using the \
             conversation and completed tool results available here."
                .to_owned()
        }
        ProviderError::InvalidToolCall => {
            "Aworkit recovery notice: the previous response contained a tool call that does not \
             match any available tool. Retry using only the exact tool names listed in the tool \
             definitions, with the arguments each tool expects."
                .to_owned()
        }
        // A context rejection that compaction could not resolve is still the
        // model's turn: tell it what happened, that the conversation is intact,
        // and let it finish or narrow the task itself.
        ProviderError::ContextWindowExceeded | ProviderError::InputBoundExceeded { .. } => format!(
            "Aworkit recovery notice: this model request was rejected because it exceeds what this \
             model accepts ({error}). No tools from that attempt were executed and no history was \
             discarded. Continue from what you already know and complete the task with a shorter \
             next step, or tell the user plainly what could not be completed."
        ),
        // The provider refused or failed this exact request for its own reason.
        // The text is the provider's own bounded diagnostic, so the model can
        // adjust its request, its arguments, or its plan and keep going.
        other => format!(
            "Aworkit recovery notice: the previous provider turn failed with: {other}. No tools \
             from that attempt were executed. The request was not accepted. Continue the task \
             using the conversation and completed tool results available here; adjust your next \
             action if that failure was caused by it."
        ),
    };
    Some(notice)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_provider_failure_is_reported_to_the_model_except_cancellation() {
        // A dropped stream, a transport failure, and a timed-out request resume
        // the same frozen route with a notice instead of ending the Agent node.
        assert!(provider_recovery_notice(&ProviderError::RequestTimedOut).is_some());
        assert!(provider_recovery_notice(&ProviderError::StreamInterrupted).is_some());
        assert!(provider_recovery_notice(&ProviderError::TransportFailed).is_some());
        assert!(provider_recovery_notice(&ProviderError::InvalidToolCall).is_some());
        // A provider's own refusal or failure is reported with its diagnostic.
        let refused = provider_recovery_notice(&ProviderError::Failed("quota exceeded".into()))
            .expect("a provider failure is the model's to handle");
        assert!(refused.contains("quota exceeded"));
        // An over-bound or overflowing request carries an actionable notice too,
        // so a compaction that cannot reduce further never ends the node.
        let bound = provider_recovery_notice(&ProviderError::InputBoundExceeded {
            input_bytes: 8,
            maximum_input_bytes: 4,
        })
        .expect("a context rejection is the model's to handle");
        assert!(bound.contains("exceeds what this model accepts"));
        assert!(provider_recovery_notice(&ProviderError::ContextWindowExceeded).is_some());
        // Only caller cancellation and an ambiguous acceptance stop the loop:
        // the first is the user's decision, the second is a durable safety
        // verdict that must never be retried.
        assert!(provider_recovery_notice(&ProviderError::Cancelled).is_none());
        assert!(provider_recovery_notice(&ProviderError::AcceptanceAmbiguous).is_none());
    }
}
