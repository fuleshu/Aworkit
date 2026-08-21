//! Core-approved, generation-fenced capability execution.
mod gateway;
mod model;
mod normalize;
mod files;
mod process;
mod registry;
pub use gateway::{ApprovedInvocation, CapabilityHost, HostError, InvocationResult};
pub use model::{ModelGateway, ModelRequest, ModelResponse, ModelProvider, ProviderError};
pub use normalize::{CapabilityOutcome, OutcomeKind, Redactor, StreamNormalizer};
pub use files::{FileAuthority, FileToolError, ProjectFiles};
pub use process::{ProcessError, ProcessRequest, ProcessResult, ProcessRunner};
pub use registry::{AdapterDescriptor, AdapterRegistry, CapabilityKind};
