//! Resolve model capacity from the exact provider binding, never from model-name guesses.
use super::*;
use crate::runtime::dto::ContextModelDto;

impl DesktopRuntime {
    /// Read capacity for the visible Chat or selected draft workflow. The caller's Chat id
    /// fences delayed replies; discovery does not edit Settings or an existing Chat snapshot.
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
            .or_else(|| self.discover_context_window(&provider, &model.remote_id));
        Ok(Some(ContextModelDto { name: model.name, context_window: capacity }))
    }

    /// Discovery is best effort and bounded; unsupported catalog metadata must not prevent a Chat.
    /// The result is frozen with a new Chat, so execution policy and the UI use the same limit.
    pub(super) fn discover_context_window(&mut self, provider: &ProviderConfigurationV2, remote_id: &str) -> Option<u64> {
        let credential = self.provider_operation_credential(provider, None, provider.credential_ref.is_some()).ok()?;
        self.provider.discover_models(&provider.kind, &provider.base_url, credential, Duration::from_secs(5))
            .ok()?.into_iter().find(|model| model.remote_id == remote_id)?.context_window.filter(|value| *value > 0)
    }

    /// Older Chats may predate capacity discovery. Use matching saved metadata for display only;
    /// their immutable model and authority snapshots retain their original hashes.
    pub(super) fn populate_context_model(&self, snapshot: &mut RuntimeSnapshot) -> Result<(), String> {
        if snapshot.context_model.as_ref().is_some_and(|model| model.context_window.is_some()) { return Ok(()); }
        let settings = self.documents.settings();
        let model = if let Some(frozen) = self.history.current_frozen_context()? {
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
