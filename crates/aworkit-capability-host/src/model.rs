use serde_json::Value;
use thiserror::Error;
/// A provider-neutral request retained wholly in Aworkit terms.
#[derive(Clone, Debug, Eq, PartialEq)] pub struct ModelRequest { pub binding_id: String, pub input: Value, pub max_output_bytes: usize }
#[derive(Clone, Debug, Eq, PartialEq)] pub struct ModelResponse { pub text: String, pub input_tokens: u64, pub output_tokens: u64 }
pub trait ModelProvider: Send + Sync { fn binding_id(&self) -> &str; fn complete(&self, request: &ModelRequest) -> Result<ModelResponse, ProviderError>; }
/// Resolves only a frozen allowed provider binding; native sessions stay in the provider.
pub struct ModelGateway<P> { provider: P, allowed_binding: String }
impl<P: ModelProvider> ModelGateway<P> { pub fn new(provider: P, allowed_binding: impl Into<String>) -> Self { Self { provider, allowed_binding: allowed_binding.into() } } pub fn complete(&self, request: &ModelRequest) -> Result<ModelResponse, ProviderError> { if request.binding_id != self.allowed_binding || self.provider.binding_id() != request.binding_id { return Err(ProviderError::BindingDrift); } let result = self.provider.complete(request)?; if result.text.len() > request.max_output_bytes { return Err(ProviderError::OutputBound); } Ok(result) } }
#[derive(Debug, Error, Eq, PartialEq)] pub enum ProviderError { #[error("provider binding drift")] BindingDrift, #[error("provider output exceeds binding limit")] OutputBound, #[error("provider failed: {0}")] Failed(String) }
