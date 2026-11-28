//! Account-scoped provider limits observed by any pass or delegated child.
//!
//! A spend or quota limit is a fact about the provider account, not about one
//! model call. In the recorded run six delegated children each discovered the
//! same 429 separately: one child reached 107 refusals while its parent stopped
//! at ten, and the group spent EUR 20 against the same wall. The first observation
//! therefore stops every pass and child dispatching to that provider in the same
//! Chat, instead of each of them having to learn it again.
//!
//! The registry is in-memory by design: it arms only for the life of the process
//! that observed the limit. A restart that reconnects to a still-exhausted
//! account classifies the first answer again on its own, which is correct - the
//! human may have raised the limit in between.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// One Chat's observed provider limits, shared by every invocation and child of
/// that Chat.
#[derive(Clone, Default)]
pub(crate) struct ProviderLimitRegistryV1 {
    limits: Arc<Mutex<BTreeMap<String, String>>>,
}

impl ProviderLimitRegistryV1 {
    /// Record the first account-scoped limit for one Chat. Later observations do
    /// not replace the text, so the reason the Chat stopped stays the one a human
    /// can act on.
    pub(crate) fn record(&self, chat_id: &str, provider_text: &str) {
        if let Ok(mut limits) = self.limits.lock() {
            limits
                .entry(chat_id.to_owned())
                .or_insert_with(|| provider_text.to_owned());
        }
    }

    /// The provider text the Chat already stopped on, if any.
    pub(crate) fn notice(&self, chat_id: &str) -> Option<String> {
        self.limits
            .lock()
            .ok()
            .and_then(|limits| limits.get(chat_id).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_observation_is_shared_with_every_sibling_and_kept() {
        let registry = ProviderLimitRegistryV1::default();
        assert!(registry.notice("chat.a").is_none());
        // Six children of one Chat observe the same wall.
        for _ in 0..6 {
            registry.record(
                "chat.a",
                "provider returned HTTP status 429: You exceeded your spend-based rate limit",
            );
        }
        assert_eq!(
            registry.notice("chat.a").as_deref(),
            Some("provider returned HTTP status 429: You exceeded your spend-based rate limit")
        );
        // A different Chat, or a different provider account, is unaffected.
        assert!(registry.notice("chat.b").is_none());
    }

    #[test]
    fn the_recorded_reason_is_not_replaced_by_a_later_observation() {
        let registry = ProviderLimitRegistryV1::default();
        registry.record("chat.a", "the first limit");
        registry.record("chat.a", "a later, less useful condition");
        assert_eq!(registry.notice("chat.a").as_deref(), Some("the first limit"));
    }

    #[test]
    fn the_registry_is_shared_across_clones() {
        let registry = ProviderLimitRegistryV1::default();
        let sibling = registry.clone();
        registry.record("chat.a", "limit");
        assert_eq!(sibling.notice("chat.a").as_deref(), Some("limit"));
    }
}
