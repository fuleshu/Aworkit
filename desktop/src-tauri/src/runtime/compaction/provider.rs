//! Lazy auxiliary adapter. The acting model cannot choose this route: it is
//! frozen into the outer invocation before broker admission. Every summary
//! materializes only its own invocation-scoped credential lease.
use super::*;
use crate::runtime::compaction as c;
use aworkit_capability_host::{
    ModelEventV1, ModelRequestV1, ModelToolEventV1, ModelToolRequestV1, ProviderAcceptanceV1,
    ProviderError,
};
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) fn providers(
    primary: Box<dyn ProviderEnginePortV1>,
    metadata: &Value,
    factory: Arc<dyn ProviderFactoryV1>,
    authority: Arc<PipelineLeaseAuthority>,
    outer: StableId,
    run: StableId,
) -> Result<Vec<Box<dyn ProviderEnginePortV1>>, String> {
    let metadata: c::Metadata =
        serde_json::from_value(metadata.clone()).map_err(|e| e.to_string())?;
    let mut providers = vec![primary];
    if let Some(target) = metadata.summary_target {
        providers.push(Box::new(SummaryProvider {
            version: c::hash(&target),
            target,
            factory,
            authority,
            outer,
            run,
            next: AtomicU64::new(0),
        }));
    }
    Ok(providers)
}
struct SummaryProvider {
    target: c::FrozenSummaryTarget,
    version: String,
    factory: Arc<dyn ProviderFactoryV1>,
    authority: Arc<PipelineLeaseAuthority>,
    outer: StableId,
    run: StableId,
    next: AtomicU64,
}
impl SummaryProvider {
    fn provider(&self) -> Result<Box<dyn ProviderEnginePortV1>, String> {
        let protocol =
            ProviderProtocolV1::parse(&self.target.provider.kind).map_err(|e| e.to_string())?;
        let descriptor = model_descriptor(protocol).map_err(|e| e.to_string())?;
        let invocation = digest_id(
            "summary",
            &format!(
                "{}:{}:{}",
                self.outer,
                self.version,
                self.next.fetch_add(1, Ordering::Relaxed)
            ),
        )
        .map_err(|e| e.to_string())?;
        let materialized = if let Some(credential) = &self.target.credential {
            let secret = StoredSecretBindingV1 {
                opaque_ref: credential.credential_ref.clone(),
                field_names: credential.field_names.clone(),
                revision: credential.revision,
            };
            let lease = lease_id(&invocation, &secret).map_err(|e| e.to_string())?;
            self.authority
                .prepare(
                    Some(&secret),
                    &invocation,
                    &self.run,
                    std::slice::from_ref(&lease),
                )
                .map_err(|e| e.to_string())?;
            Some(
                SecretMaterializer::new(CoreSecretLeaseClient {
                    authority: self.authority.clone(),
                })
                .materialize(&SecretMaterializationPlanV1 {
                    decision_id: invocation.clone(),
                    invocation_id: invocation,
                    host_generation: self.authority.generation,
                    lease: SecretLeaseHandleV1 { lease_id: lease },
                    fields: vec![SecretFieldPlanV1 {
                        field: API_KEY_FIELD.into(),
                        target: InjectionTargetV1::Header(protocol.api_key_header().into()),
                    }],
                })
                .map_err(|e| format!("Summary credential lease could not be materialized: {e}"))?,
            )
        } else {
            None
        };
        let api_key = materialized
            .as_ref()
            .and_then(|m| m.value(API_KEY_FIELD))
            .map(|b| {
                String::from_utf8(b.to_vec())
                    .map(Zeroizing::new)
                    .map_err(|_| "Summary credential is not UTF-8".to_owned())
            })
            .transpose()?;
        let limits = self.target.provider.runtime_limits()?;
        let binding = StoredProviderBindingV1 {
            kind: self.target.provider.kind.clone(),
            base_url: self.target.provider.base_url.clone(),
            model: self.target.model.remote_id.clone(),
            parameters: self.target.model.parameters.clone(),
            model_context: json!({}),
            request_timeout_seconds: limits.request_timeout_seconds,
            maximum_tool_output_bytes: limits.maximum_tool_output_bytes,
        };
        self.factory
            .create(&descriptor, &binding, api_key)
            .map_err(|e| redact_error(&materialized, &e))
    }
}
impl ProviderEnginePortV1 for SummaryProvider {
    fn binding_id(&self) -> &str {
        c::SUMMARY_BINDING
    }
    fn version_hash(&self) -> &str {
        &self.version
    }
    fn execute(
        &self,
        request: &ModelRequestV1,
        emit: &mut dyn FnMut(ModelEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        self.provider()
            .map_err(ProviderError::Failed)?
            .execute(request, emit)
    }
    fn execute_tool_turn_cancellable(
        &self,
        request: &ModelToolRequestV1,
        cancellation: &CancellationToken,
        emit: &mut dyn FnMut(ModelToolEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        if cancellation.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        self.provider()
            .map_err(ProviderError::Failed)?
            .execute_tool_turn_cancellable(request, cancellation, emit)
    }
}
