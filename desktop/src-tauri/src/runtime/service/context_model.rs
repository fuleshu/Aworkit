//! Resolve model capacity from the exact provider binding, never from model-name guesses.
use super::*;
use crate::runtime::dto::ContextModelDto;
use std::sync::Arc;

/// One context-window discovery for a provider/model pair.
///
/// `Pending` is recorded before the background provider call starts, so several
/// renders of the same draft cannot launch duplicate HTTP requests. `Known(None)`
/// records a completed attempt that found no declared window.
pub(super) enum ContextWindowLookup {
    Pending,
    Known(Option<u64>),
}

impl DesktopRuntime {
    /// Read capacity for the visible Chat or selected draft workflow. The caller's Chat id
    /// fences delayed replies; discovery does not edit Settings or an existing Chat snapshot.
    ///
    /// A missing window is returned immediately and resolved in the background, so opening a
    /// new Chat never waits on a provider HTTP call.
    pub fn context_model(&mut self, chat_id: &str, workflow_id: Option<&str>) -> Result<Option<ContextModelDto>, String> {
        if self.history.current_chat_identity()?.is_none_or(|identity| identity.chat_id.as_str() != chat_id) {
            return Err("context query targets a different Chat".into());
        }
        let (provider, model) = if let Some(frozen) = self.history.current_frozen_context()? {
            (frozen.context.provider_snapshot, frozen.context.model_snapshot)
        } else {
            let Some(workflow_id) = workflow_id else { return Ok(None); };
            let workflow = self.documents.workflow_snapshot_for(workflow_id);
            let Some(tier) = graph_model_tier_ids(&workflow.document).into_iter().next() else { return Ok(None); };
            let Ok(resolved) = resolve_workflow_model(self.documents.settings(), &tier) else { return Ok(None); };
            (resolved.provider, resolved.model)
        };
        let saved_capacity = self.documents.settings().providers.iter().find(|saved| saved.id == provider.id
            && saved.base_url == provider.base_url && saved.kind == provider.kind)
            .and_then(|saved| saved.models.iter().find(|saved| saved.remote_id == model.remote_id))
            .and_then(|saved| saved.context_window);
        let capacity = model.context_window.or(saved_capacity)
            .or_else(|| self.cached_context_window(&provider, &model.remote_id));
        if capacity.is_none() {
            self.warm_context_window(provider, model.remote_id.clone());
        }
        Ok(Some(ContextModelDto { name: model.name, context_window: capacity }))
    }

    /// Key that identifies one discovery independently of the Settings document.
    fn context_window_key(provider: &ProviderConfigurationV2, remote_id: &str) -> String {
        format!("{}\u{1f}{}\u{1f}{}", provider.kind, provider.base_url, remote_id)
    }

    /// A completed discovery, when one is already cached.
    fn cached_context_window(&self, provider: &ProviderConfigurationV2, remote_id: &str) -> Option<u64> {
        let key = Self::context_window_key(provider, remote_id);
        match self.context_windows.lock() {
            Ok(cache) => match cache.get(&key) {
                Some(ContextWindowLookup::Known(value)) => *value,
                _ => None,
            },
            Err(_) => None,
        }
    }

    /// Starts one bounded discovery on a detached thread.
    ///
    /// The provider call is bounded at five seconds and used to run while the
    /// runtime lock was held, so every command behind it — including opening a
    /// new Chat — waited for the network. It stays best effort: an unreachable
    /// provider records "unknown" and a Chat still starts.
    fn warm_context_window(&mut self, provider: ProviderConfigurationV2, remote_id: String) {
        let key = Self::context_window_key(&provider, &remote_id);
        match self.context_windows.lock() {
            Ok(mut cache) => {
                if cache.contains_key(&key) {
                    return;
                }
                cache.insert(key.clone(), ContextWindowLookup::Pending);
            }
            Err(_) => return,
        }
        let credential = self
            .provider_operation_credential(&provider, None, provider.credential_ref.is_some())
            .ok()
            .flatten();
        let provider_port = Arc::clone(&self.provider);
        let cache = Arc::clone(&self.context_windows);
        let kind = provider.kind.clone();
        let base_url = provider.base_url.clone();
        std::thread::spawn(move || {
            let discovered = provider_port
                .discover_models(&kind, &base_url, credential, Duration::from_secs(5))
                .ok()
                .and_then(|models| models.into_iter().find(|model| model.remote_id == remote_id))
                .and_then(|model| model.context_window)
                .filter(|value| *value > 0);
            if let Ok(mut cache) = cache.lock() {
                cache.insert(key, ContextWindowLookup::Known(discovered));
            }
        });
    }

    /// A Settings change can reconfigure or move a provider, so cached
    /// discoveries must not outlive it.
    pub(super) fn clear_context_window_cache(&self) {
        if let Ok(mut cache) = self.context_windows.lock() {
            cache.clear();
        }
    }

    /// Discovery is best effort and bounded; unsupported catalog metadata must not prevent a Chat.
    /// The result is frozen with a new Chat, so execution policy and the UI use the same limit.
    ///
    /// A start consults the cache first and only then pays for the network call, so a draft that
    /// already warmed the window freezes it without a second request.
    pub(super) fn discover_context_window(&mut self, provider: &ProviderConfigurationV2, remote_id: &str) -> Option<u64> {
        if let Some(value) = self.cached_context_window(provider, remote_id) {
            return Some(value);
        }
        let credential = self.provider_operation_credential(provider, None, provider.credential_ref.is_some()).ok()?;
        let discovered = self.provider.discover_models(&provider.kind, &provider.base_url, credential, Duration::from_secs(5))
            .ok()?.into_iter().find(|model| model.remote_id == remote_id)?.context_window.filter(|value| *value > 0);
        if let Ok(mut cache) = self.context_windows.lock() {
            cache.insert(Self::context_window_key(provider, remote_id), ContextWindowLookup::Known(discovered));
        }
        discovered
    }

    /// Older Chats may predate capacity discovery. Use matching saved metadata for display only;
    /// their immutable model and authority snapshots retain their original hashes.
    pub(super) fn populate_context_model(&self, snapshot: &mut RuntimeSnapshot) -> Result<(), String> {
        if snapshot.context_model.as_ref().is_some_and(|model| model.context_window.is_some()) { return Ok(()); }
        let settings = self.documents.settings();
        let model = if let Some(frozen) = self.history.frozen_context(&StableId::parse(snapshot.chat.chat_id.clone()).map_err(|e| e.to_string())?)? {
            let context = frozen.context;
            settings.providers.iter().find(|provider| provider.id == context.provider_id
                && provider.base_url == context.provider_base_url && provider.kind == context.provider_kind)
                .and_then(|provider| provider.models.iter().find(|model| model.remote_id == context.remote_model_id))
                .map(|model| ContextModelDto { name: context.model_name, context_window: model.context_window })
        } else { None };
        if let Some(model) = model { snapshot.context_model = Some(model); }
        Ok(())
    }
}
