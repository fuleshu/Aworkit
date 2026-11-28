//! Classification of a provider answer, and the reaction each class requires.
//!
//! A provider failure is not one condition. The recorded run that spent EUR 20
//! carried 297 failed attempts whose body was `Canonical model result settled.`
//! and no usage row, while the one cause that mattered - a spend-based 429 -
//! appeared only at Agent level. This module turns the provider's own answer into
//! a stable class, so the harness can stop, wait, compact or report deliberately
//! instead of re-sending the same request and hoping.
//!
//! The class is what the repeated-refusal guard counts. A byte-identical payload
//! test let one child reach 107 refusals while its parent stopped at ten, because
//! the child's notice and turn counter changed with every retry.

use std::time::Duration;

use aworkit_capability_host::ProviderError;

/// The provider answer classes the failure policy names.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum ProviderFailureClassV1 {
    /// The account cannot pay for the request. No retry can succeed until a
    /// human changes the limit, and the limit belongs to the account, so the
    /// whole Chat stops dispatching to this provider.
    SpendOrQuotaLimit,
    /// The provider is throttling. Wait for its own `Retry-After` when it named
    /// one, otherwise a bounded backoff.
    RateLimit,
    /// A 5xx, a reset stream or a timeout: retry after a bounded backoff.
    Transient,
    /// The credential, endpoint, model id or binding cannot work. No retry can
    /// fix it; the user must change a setting.
    AuthenticationOrConfiguration,
    /// The request, not the account, is too large. Compact or trim once and
    /// re-dispatch; never an identical resend.
    ContextTooLarge,
    /// A refusal none of the above explains. Reported to the model, which
    /// decides what to do differently.
    Refusal,
}

impl ProviderFailureClassV1 {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::SpendOrQuotaLimit => "spend_or_quota_limit",
            Self::RateLimit => "rate_limit",
            Self::Transient => "transient",
            Self::AuthenticationOrConfiguration => "authentication_or_configuration",
            Self::ContextTooLarge => "context_too_large",
            Self::Refusal => "refusal",
        }
    }

    /// The classes whose first observation ends the pass for this provider.
    ///
    /// A spend limit is a fact about the account and an unusable credential is a
    /// fact about the configuration; neither is a decision the model can take.
    pub(crate) fn ends_pass_on_first_observation(self) -> bool {
        matches!(
            self,
            Self::SpendOrQuotaLimit | Self::AuthenticationOrConfiguration
        )
    }

    /// A human-readable instruction for what to change, for the classes where a
    /// setting rather than the work is the problem.
    pub(crate) fn remedy(self) -> &'static str {
        match self {
            Self::SpendOrQuotaLimit => {
                "The limit belongs to the provider account, so no retry can succeed until it is raised. Every pass and delegated child dispatching to this provider in this Chat has stopped."
            }
            Self::AuthenticationOrConfiguration => {
                "Check the provider binding: its credential, base URL and model id. This is not a transient condition and was not retried."
            }
            Self::RateLimit => "The provider is throttling requests; the harness waited as it asked.",
            Self::Transient => "The provider or its transport failed transiently; the harness retried after a bounded backoff.",
            Self::ContextTooLarge => {
                "The request exceeds what this model accepts; the harness reduced the context once before re-dispatching."
            }
            Self::Refusal => "The provider refused the request for its own reason.",
        }
    }
}

/// The reaction the harness applies for one classified failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProviderReactionV1 {
    /// Stop this pass and every other dispatch to the same provider in this Chat.
    StopAccountLimit,
    /// Wait, then re-dispatch the same frozen route.
    WaitThenRetry { seconds: u64 },
    /// The wait budget is spent: end the pass with the provider's own text.
    StopWaitBudgetSpent,
    /// Nothing can be fixed by retrying: end the pass naming the setting.
    StopConfiguration,
    /// Reduce the context once, then re-dispatch.
    CompactAndRetry,
    /// Tell the model and let it decide; the class-based guard ends a refusal
    /// streak that no model turn can change.
    ReportAndContinue,
}

impl ProviderReactionV1 {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::StopAccountLimit => "stopped: account spend or quota limit",
            Self::WaitThenRetry { .. } => "waited, then re-dispatched",
            Self::StopWaitBudgetSpent => "stopped: the wait budget was spent",
            Self::StopConfiguration => "stopped: configuration cannot be retried",
            Self::CompactAndRetry => "compacted the context, then re-dispatched",
            Self::ReportAndContinue => "reported the condition and continued",
        }
    }
}

/// Classify one provider failure. `None` means the condition is not a provider
/// refusal at all: caller cancellation and an ambiguous acceptance are decisions,
/// not answers to classify.
pub(crate) fn classify_provider_failure(
    error: &ProviderError,
) -> Option<ProviderFailureClassV1> {
    match error {
        ProviderError::Cancelled | ProviderError::AcceptanceAmbiguous => None,
        ProviderError::ContextWindowExceeded | ProviderError::InputBoundExceeded { .. } => {
            Some(ProviderFailureClassV1::ContextTooLarge)
        }
        ProviderError::RequestTimedOut
        | ProviderError::StreamInterrupted
        | ProviderError::TransportFailed => Some(ProviderFailureClassV1::Transient),
        ProviderError::InvalidToolCall(_) => Some(ProviderFailureClassV1::Refusal),
        ProviderError::HttpStatus { status, detail, .. } => {
            Some(classify_status(*status, detail))
        }
        ProviderError::Failed(text) => {
            let (status, detail) = split_http_status(text);
            Some(match status {
                Some(status) => classify_status(status, &detail),
                None => classify_text(&text.to_lowercase()),
            })
        }
        // Binding drift, an unusable plan or a missing usage event are contract
        // and configuration facts, never something a resend repairs.
        ProviderError::BindingDrift
        | ProviderError::InvalidPlan
        | ProviderError::NoCandidateAccepted
        | ProviderError::MissingOrDuplicateUsage
        | ProviderError::OutputBound
        | ProviderError::ConflictingAcceptanceEvidence => {
            Some(ProviderFailureClassV1::AuthenticationOrConfiguration)
        }
    }
}

/// The provider's bounded text, which is what Run details must show.
pub(crate) fn provider_failure_text(error: &ProviderError) -> String {
    error.to_string()
}

/// The delay a provider asked for, when the answer carries one.
pub(crate) fn retry_after_seconds(error: &ProviderError) -> Option<u64> {
    match error {
        ProviderError::HttpStatus {
            retry_after_seconds,
            ..
        } => *retry_after_seconds,
        _ => None,
    }
}

/// The status and bounded detail of an error rendered as
/// `provider returned HTTP status 429: ...`. Older call sites stringify their
/// status, so this keeps classification working for them without changing every
/// producer at once.
fn split_http_status(text: &str) -> (Option<u16>, String) {
    let Some(rest) = text.strip_prefix("provider returned HTTP status ") else {
        return (None, String::new());
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let Ok(status) = digits.parse::<u16>() else {
        return (None, String::new());
    };
    let detail = rest[digits.len()..].trim_start_matches(':').trim();
    (Some(status), detail.to_owned())
}

fn classify_status(status: u16, detail: &str) -> ProviderFailureClassV1 {
    let text = detail.to_lowercase();
    let spend = names_spend_limit(&text);
    match status {
        401 | 403 => ProviderFailureClassV1::AuthenticationOrConfiguration,
        402 => ProviderFailureClassV1::SpendOrQuotaLimit,
        404 if names_model_problem(&text) => {
            ProviderFailureClassV1::AuthenticationOrConfiguration
        }
        429 if spend => ProviderFailureClassV1::SpendOrQuotaLimit,
        429 => ProviderFailureClassV1::RateLimit,
        408 => ProviderFailureClassV1::Transient,
        500..=599 => ProviderFailureClassV1::Transient,
        // A 400 that names a credential or model problem is configuration, not a
        // malformed conversation.
        _ if names_authentication_problem(&text) => {
            ProviderFailureClassV1::AuthenticationOrConfiguration
        }
        _ if spend => ProviderFailureClassV1::SpendOrQuotaLimit,
        _ => ProviderFailureClassV1::Refusal,
    }
}

fn classify_text(text: &str) -> ProviderFailureClassV1 {
    if text.contains("no frozen provider candidate accepted") {
        return ProviderFailureClassV1::AuthenticationOrConfiguration;
    }
    if names_spend_limit(text) {
        return ProviderFailureClassV1::SpendOrQuotaLimit;
    }
    if names_authentication_problem(text) {
        return ProviderFailureClassV1::AuthenticationOrConfiguration;
    }
    if text.contains("rate limit") || text.contains("too many requests") {
        return ProviderFailureClassV1::RateLimit;
    }
    if text.contains("overloaded")
        || text.contains("temporarily unavailable")
        || text.contains("service unavailable")
        || text.contains("connection")
        || text.contains("timed out")
    {
        return ProviderFailureClassV1::Transient;
    }
    ProviderFailureClassV1::Refusal
}

/// A spend, quota, credit or billing limit named by the provider itself.
fn names_spend_limit(lowercase: &str) -> bool {
    [
        "spend",
        "quota",
        "credit",
        "billing",
        "payment required",
        "insufficient_quota",
        "resource_exhausted",
        "exceeded your current",
    ]
    .iter()
    .any(|needle| lowercase.contains(needle))
}

fn names_authentication_problem(lowercase: &str) -> bool {
    [
        "api key",
        "apikey",
        "unauthenticated",
        "unauthorized",
        "permission_denied",
        "permission denied",
        "forbidden",
        "credential",
        "authentication",
        "invalid model",
        "model not found",
        "unknown model",
        "unsupported model",
        "does not exist",
    ]
    .iter()
    .any(|needle| lowercase.contains(needle))
}

fn names_model_problem(lowercase: &str) -> bool {
    ["model", "endpoint", "not found"].iter().any(|n| lowercase.contains(n))
}

/// The bounded wait this class asks for. The provider's own `Retry-After` wins
/// when it named one.
pub(crate) fn backoff_for(
    class: ProviderFailureClassV1,
    attempt: u32,
    retry_after_seconds: Option<u64>,
) -> Duration {
    if let Some(seconds) = retry_after_seconds {
        return Duration::from_secs(seconds);
    }
    let exponent = attempt.min(5);
    let seconds = match class {
        // A throttled provider is asked to slow down; start at a second.
        ProviderFailureClassV1::RateLimit => 1_u64 << exponent,
        _ => 1_u64 << exponent.saturating_sub(1).min(4),
    };
    // Jitter is deterministic on the attempt so the same streak is reproducible
    // in a test and across a restart.
    let jitter_millis = u64::from(attempt.wrapping_mul(97) % 250);
    Duration::from_secs(seconds).saturating_add(Duration::from_millis(jitter_millis))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spend_and_quota_answers_are_their_own_account_scoped_class() {
        let spend = ProviderError::HttpStatus {
            status: 429,
            detail: ": You exceeded your spend-based rate limit".into(),
            retry_after_seconds: None,
        };
        assert_eq!(
            classify_provider_failure(&spend),
            Some(ProviderFailureClassV1::SpendOrQuotaLimit)
        );
        let quota = ProviderError::HttpStatus {
            status: 429,
            detail: ": insufficient_quota: quota exceeded".into(),
            retry_after_seconds: Some(30),
        };
        assert_eq!(
            classify_provider_failure(&quota),
            Some(ProviderFailureClassV1::SpendOrQuotaLimit)
        );
        assert!(ProviderFailureClassV1::SpendOrQuotaLimit.ends_pass_on_first_observation());
    }

    #[test]
    fn a_throttling_429_without_a_spend_meaning_is_a_rate_limit() {
        let plain = ProviderError::HttpStatus {
            status: 429,
            detail: ": Too many requests".into(),
            retry_after_seconds: Some(12),
        };
        assert_eq!(
            classify_provider_failure(&plain),
            Some(ProviderFailureClassV1::RateLimit)
        );
        assert_eq!(
            backoff_for(ProviderFailureClassV1::RateLimit, 3, Some(12)),
            Duration::from_secs(12)
        );
        // Without a named delay the backoff is bounded and grows.
        let grown = backoff_for(ProviderFailureClassV1::RateLimit, 4, None);
        assert!(grown >= Duration::from_secs(16));
        assert!(grown < Duration::from_secs(17));
    }

    #[test]
    fn each_named_class_maps_to_its_reaction_rather_than_a_resend() {
        let auth = ProviderError::HttpStatus {
            status: 401,
            detail: ": invalid api key".into(),
            retry_after_seconds: None,
        };
        assert_eq!(
            classify_provider_failure(&auth),
            Some(ProviderFailureClassV1::AuthenticationOrConfiguration)
        );
        assert!(
            ProviderFailureClassV1::AuthenticationOrConfiguration
                .ends_pass_on_first_observation()
        );
        assert_eq!(
            classify_provider_failure(&ProviderError::TransportFailed),
            Some(ProviderFailureClassV1::Transient)
        );
        assert_eq!(
            classify_provider_failure(&ProviderError::ContextWindowExceeded),
            Some(ProviderFailureClassV1::ContextTooLarge)
        );
        assert_eq!(
            classify_provider_failure(&ProviderError::Failed("upstream rejected".into())),
            Some(ProviderFailureClassV1::Refusal)
        );
        // Cancellation is the user's decision, not a provider answer.
        assert_eq!(
            classify_provider_failure(&ProviderError::Cancelled),
            None
        );
    }

    #[test]
    fn a_legacy_stringified_status_still_classifies() {
        let legacy = ProviderError::Failed(
            "provider returned HTTP status 429: You exceeded your spend-based rate limit".into(),
        );
        assert_eq!(
            classify_provider_failure(&legacy),
            Some(ProviderFailureClassV1::SpendOrQuotaLimit)
        );
    }
}
