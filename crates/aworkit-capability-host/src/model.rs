//! Provider-neutral model execution with frozen, conservative fallback.

use std::collections::BTreeSet;

use serde_json::Value;
use thiserror::Error;

use crate::CancellationToken;

/// Compatibility request retained for simple provider adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRequest {
    pub binding_id: String,
    pub input: Value,
    pub max_output_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelResponse {
    pub text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

pub trait ModelProvider: Send + Sync {
    fn binding_id(&self) -> &str;
    fn complete(&self, request: &ModelRequest) -> Result<ModelResponse, ProviderError>;
}

/// Legacy one-provider gateway still fails closed on any binding drift.
pub struct ModelGateway<P> {
    provider: P,
    allowed_binding: String,
}

impl<P: ModelProvider> ModelGateway<P> {
    pub fn new(provider: P, allowed_binding: impl Into<String>) -> Self {
        Self {
            provider,
            allowed_binding: allowed_binding.into(),
        }
    }

    pub fn complete(&self, request: &ModelRequest) -> Result<ModelResponse, ProviderError> {
        if request.binding_id != self.allowed_binding
            || self.provider.binding_id() != request.binding_id
        {
            return Err(ProviderError::BindingDrift);
        }
        let result = self.provider.complete(request)?;
        if result.text.len() > request.max_output_bytes {
            return Err(ProviderError::OutputBound);
        }
        Ok(result)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelCandidateV1 {
    pub binding_id: String,
    pub version_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelResolutionPlanV1 {
    pub candidates: Vec<ModelCandidateV1>,
    pub maximum_input_bytes: usize,
    pub maximum_output_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRequestV1 {
    pub input: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelEventV1 {
    AssistantOutput(String),
    ReasoningRaw(String),
    ReasoningSummary(String),
    Progress(String),
    Usage {
        input_tokens: u64,
        output_tokens: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderAcceptanceV1 {
    /// The provider positively proved that it never accepted the request.
    DefinitelyNotAccepted,
    /// The provider accepted the request and produced a conclusive terminal result.
    Accepted,
    /// Transport or provider evidence cannot establish acceptance or terminal state.
    Ambiguous,
}

/// Stable provider seam. Native provider objects stay behind this interface.
pub trait ProviderEnginePortV1: Send + Sync {
    fn binding_id(&self) -> &str;
    fn version_hash(&self) -> &str;
    fn execute(
        &self,
        request: &ModelRequestV1,
        emit: &mut dyn FnMut(ModelEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError>;

    fn execute_cancellable(
        &self,
        request: &ModelRequestV1,
        cancellation: &CancellationToken,
        emit: &mut dyn FnMut(ModelEventV1) -> Result<(), ProviderError>,
    ) -> Result<ProviderAcceptanceV1, ProviderError> {
        if cancellation.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        self.execute(request, emit)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelDispatchEvidenceV1 {
    pub selected_binding: String,
    pub attempted_bindings: Vec<String>,
    pub events: Vec<ModelEventV1>,
}

/// Executes exactly the ordered candidates frozen by the trusted core.
pub struct FrozenModelGateway {
    providers: Vec<Box<dyn ProviderEnginePortV1>>,
}

impl FrozenModelGateway {
    #[must_use]
    pub fn new(providers: Vec<Box<dyn ProviderEnginePortV1>>) -> Self {
        Self { providers }
    }

    pub fn execute(
        &self,
        plan: &ModelResolutionPlanV1,
        request: &ModelRequestV1,
    ) -> Result<ModelDispatchEvidenceV1, ProviderError> {
        self.execute_cancellable(plan, request, &CancellationToken::default())
    }

    pub fn execute_cancellable(
        &self,
        plan: &ModelResolutionPlanV1,
        request: &ModelRequestV1,
        cancellation: &CancellationToken,
    ) -> Result<ModelDispatchEvidenceV1, ProviderError> {
        let identities: BTreeSet<_> = plan
            .candidates
            .iter()
            .map(|candidate| candidate.binding_id.as_str())
            .collect();
        if plan.candidates.is_empty()
            || identities.len() != plan.candidates.len()
            || plan.maximum_input_bytes == 0
            || plan.maximum_output_bytes == 0
            || serde_json::to_vec(&request.input)
                .map_err(|_| ProviderError::InvalidPlan)?
                .len()
                > plan.maximum_input_bytes
        {
            return Err(ProviderError::InvalidPlan);
        }
        let mut attempted = Vec::new();
        for candidate in &plan.candidates {
            if cancellation.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            let provider = self
                .providers
                .iter()
                .find(|provider| provider.binding_id() == candidate.binding_id)
                .ok_or(ProviderError::BindingDrift)?;
            if provider.version_hash() != candidate.version_hash {
                return Err(ProviderError::BindingDrift);
            }
            attempted.push(candidate.binding_id.clone());
            let mut events = Vec::new();
            let mut output_bytes = 0_usize;
            let mut emit = |event: ModelEventV1| {
                output_bytes = output_bytes.saturating_add(event_text_bytes(&event));
                if output_bytes > plan.maximum_output_bytes {
                    return Err(ProviderError::OutputBound);
                }
                events.push(event);
                Ok(())
            };
            let acceptance = provider.execute_cancellable(request, cancellation, &mut emit)?;
            match acceptance {
                ProviderAcceptanceV1::Accepted => {
                    if events
                        .iter()
                        .filter(|event| matches!(event, ModelEventV1::Usage { .. }))
                        .count()
                        != 1
                    {
                        return Err(ProviderError::MissingOrDuplicateUsage);
                    }
                    return Ok(ModelDispatchEvidenceV1 {
                        selected_binding: candidate.binding_id.clone(),
                        attempted_bindings: attempted,
                        events,
                    });
                }
                ProviderAcceptanceV1::DefinitelyNotAccepted if events.is_empty() => continue,
                ProviderAcceptanceV1::DefinitelyNotAccepted => {
                    return Err(ProviderError::ConflictingAcceptanceEvidence);
                }
                ProviderAcceptanceV1::Ambiguous => return Err(ProviderError::AcceptanceAmbiguous),
            }
        }
        Err(ProviderError::NoCandidateAccepted)
    }
}

fn event_text_bytes(event: &ModelEventV1) -> usize {
    match event {
        ModelEventV1::AssistantOutput(value)
        | ModelEventV1::ReasoningRaw(value)
        | ModelEventV1::ReasoningSummary(value)
        | ModelEventV1::Progress(value) => value.len(),
        ModelEventV1::Usage { .. } => 0,
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ProviderError {
    #[error("provider binding drift")]
    BindingDrift,
    #[error("provider output exceeds binding limit")]
    OutputBound,
    #[error("provider acceptance is ambiguous; fallback is forbidden")]
    AcceptanceAmbiguous,
    #[error("provider emitted data while claiming it definitely did not accept")]
    ConflictingAcceptanceEvidence,
    #[error("no frozen provider candidate accepted the request")]
    NoCandidateAccepted,
    #[error("frozen provider plan is invalid")]
    InvalidPlan,
    #[error("provider failed: {0}")]
    Failed(String),
    #[error("provider execution was cancelled")]
    Cancelled,
    #[error("an accepted provider result must contain exactly one usage event")]
    MissingOrDuplicateUsage,
}
