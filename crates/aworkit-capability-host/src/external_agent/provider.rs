//! The one-shot subagent backend seam for configured external agents.
//!
//! A configured external agent is exposed to the model as its own delegation
//! tool, and that tool names exactly one registered backend. This module owns
//! the seam every backend implements: a stable name, the start-time options it
//! declares, and one unattended one-shot run that settles with the child's
//! final answer or a safe failure diagnostic.
//!
//! The backend composes the child itself. Child commentary, reasoning, tool
//! traffic, stderr, usage and workspace diffs never cross this boundary, and a
//! start-time option a backend does not support is rejected with a typed error
//! before anything starts rather than accepted and silently ignored.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

use aworkit_protocol::StableId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::CancellationToken;

/// Largest provider-authored diagnostic this seam carries.
pub const MAXIMUM_DIAGNOSTIC_BYTES: usize = 4_096;
/// Largest accepted delegated task.
pub const MAXIMUM_TASK_BYTES: usize = 64 * 1_024;
/// Largest final answer a backend may return.
pub const MAXIMUM_ANSWER_BYTES: usize = 1_024 * 1_024;
/// Largest accepted one-shot deadline.
pub const MAXIMUM_DEADLINE: Duration = Duration::from_secs(60 * 60);

/// Which start-time options a backend supports.
///
/// Each flag corresponds one-to-one to a [`SubagentStartOptionsV1`] field. A
/// request that needs an unsupported option is refused by
/// [`ExternalAgentBackendRegistryV1::resolve`] instead of reaching a backend
/// that would have to guess.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubagentBackendCapabilitiesV1 {
    /// The caller may fix the child's model route.
    pub agent_options: bool,
    /// The caller may require a schema-constrained structured result.
    pub output_schema: bool,
    /// The caller may lower the nested delegation depth.
    pub depth_limit: bool,
    /// The caller may narrow the child's tool selection.
    pub tool_filter: bool,
    /// The caller may supply a child persona.
    pub persona: bool,
}

impl SubagentBackendCapabilitiesV1 {
    /// Every option an external agent composes for itself.
    #[must_use]
    pub const fn external_agent() -> Self {
        Self {
            agent_options: false,
            output_schema: false,
            depth_limit: false,
            tool_filter: false,
            persona: false,
        }
    }

    /// The first requested option this capability set does not cover.
    fn first_unsupported(&self, options: &SubagentStartOptionsV1) -> Option<&'static str> {
        if options.model.is_some() && !self.agent_options {
            return Some("agent options");
        }
        if options.output_schema.is_some() && !self.output_schema {
            return Some("output schema");
        }
        if options.maximum_depth.is_some() && !self.depth_limit {
            return Some("depth limit");
        }
        if options.tool_filter && !self.tool_filter {
            return Some("tool filter");
        }
        if options.persona.is_some() && !self.persona {
            return Some("persona");
        }
        None
    }
}

/// Optional start-time requests a delegation tool may pass to a backend.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SubagentStartOptionsV1 {
    /// Fixed model route for this delegation.
    pub model: Option<String>,
    /// JSON schema the final answer must satisfy.
    pub output_schema: Option<Value>,
    /// Lowest nested delegation depth admitted for the child.
    pub maximum_depth: Option<u32>,
    /// The caller asked to narrow the child's tool selection.
    pub tool_filter: bool,
    /// Persona the caller asked the child to adopt.
    pub persona: Option<String>,
}

/// Why a one-shot delegation ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentStopReasonV1 {
    /// The child produced a final answer.
    Completed,
    /// Cancellation, a Run terminal state, or an interrupt settled the run.
    Aborted,
    /// A model, context or token bound ended the run.
    Limit,
    /// A policy or sandbox decision ended the run.
    AccessPolicy,
    /// The remote service failed.
    Service,
    /// The local transport or protocol failed.
    Transport,
    /// The product reported its own failure.
    ProductError,
    /// The product completed without a usable final answer.
    InvalidResult,
    /// The child process failed or exited early.
    Process,
    /// No safe classification was available.
    Unknown,
}

impl SubagentStopReasonV1 {
    /// Whether this reason carries a usable final answer.
    #[must_use]
    pub fn is_completed(self) -> bool {
        matches!(self, Self::Completed)
    }
}

/// One unattended one-shot delegation offered to a backend.
#[derive(Clone, Debug, PartialEq)]
pub struct OneShotDelegationV1 {
    /// Core-assigned identity of this delegation.
    pub run_id: StableId,
    /// The complete, self-contained task text.
    pub task: String,
    /// Workspace the child runs in.
    pub working_directory: PathBuf,
    /// Bound for the whole run, including child startup.
    pub deadline: Duration,
    /// Optional start-time requests, gated by the backend's capabilities.
    pub options: SubagentStartOptionsV1,
}

impl OneShotDelegationV1 {
    /// Validates the delegated task before any backend sees it.
    pub fn validate(&self) -> Result<(), SubagentBackendErrorV1> {
        if self.task.trim().is_empty()
            || self.task.len() > MAXIMUM_TASK_BYTES
            || self.task.contains('\0')
        {
            return Err(SubagentBackendErrorV1::InvalidTask);
        }
        if !self.working_directory.is_absolute() {
            return Err(SubagentBackendErrorV1::InvalidWorkingDirectory);
        }
        if self.deadline.is_zero() || self.deadline > MAXIMUM_DEADLINE {
            return Err(SubagentBackendErrorV1::InvalidDeadline);
        }
        if self
            .options
            .model
            .as_deref()
            .is_some_and(|model| model.trim().is_empty() || model.len() > 512)
        {
            return Err(SubagentBackendErrorV1::InvalidStartOptions);
        }
        if self
            .options
            .persona
            .as_deref()
            .is_some_and(|persona| persona.trim().is_empty() || persona.len() > 4_096)
        {
            return Err(SubagentBackendErrorV1::InvalidStartOptions);
        }
        Ok(())
    }
}

/// The terminal result of one one-shot delegation.
#[derive(Clone, Debug, PartialEq)]
pub struct SubagentOutcomeV1 {
    /// Why the run ended.
    pub stop_reason: SubagentStopReasonV1,
    /// The child's final answer, present only for a completed run.
    pub answer: Option<String>,
    /// The validated structured result, when one was requested and produced.
    pub structured: Option<Value>,
    /// Safe, secret-free failure detail. Never carries tool input, file
    /// content, environment values, credentials or raw protocol payloads.
    pub diagnostic: Option<String>,
}

impl SubagentOutcomeV1 {
    /// Settles a run that produced the child's final answer.
    pub fn completed(answer: String) -> Result<Self, SubagentBackendErrorV1> {
        if answer.trim().is_empty() || answer.len() > MAXIMUM_ANSWER_BYTES || answer.contains('\0')
        {
            return Err(SubagentBackendErrorV1::InvalidAnswer);
        }
        Ok(Self {
            stop_reason: SubagentStopReasonV1::Completed,
            answer: Some(answer),
            structured: None,
            diagnostic: None,
        })
    }

    /// Settles a run that did not produce a final answer.
    #[must_use]
    pub fn failed(stop_reason: SubagentStopReasonV1, diagnostic: impl Into<String>) -> Self {
        let reason = if stop_reason.is_completed() {
            SubagentStopReasonV1::Unknown
        } else {
            stop_reason
        };
        Self {
            stop_reason: reason,
            answer: None,
            structured: None,
            diagnostic: Some(bounded_diagnostic(diagnostic.into())),
        }
    }

    /// Settles a cancelled or interrupted run.
    #[must_use]
    pub fn aborted() -> Self {
        Self::failed(
            SubagentStopReasonV1::Aborted,
            "the delegation was cancelled before it produced a final answer",
        )
    }

    /// Attaches a validated structured result to a completed run.
    #[must_use]
    pub fn with_structured(mut self, structured: Value) -> Self {
        self.structured = Some(structured);
        self
    }
}

/// One registered transport for running an external agent.
///
/// Backends are trusted same-process implementations. The service may call one
/// backend concurrently for distinct children, so a backend keeps no shared
/// mutable run state and never couples one run's settlement to a sibling's.
pub trait ExternalAgentBackendV1: Send + Sync {
    /// Unique registry name, for example `codex`.
    fn name(&self) -> &str;
    /// The start-time options this backend supports.
    fn capabilities(&self) -> SubagentBackendCapabilitiesV1;
    /// Whether the child sees the parent's conversation. External agents do not.
    fn inherits_parent_context(&self) -> bool {
        false
    }
    /// Runs one unattended one-shot delegation to completion.
    ///
    /// On cancellation the backend terminates the child's complete process
    /// tree before returning [`SubagentStopReasonV1::Aborted`]; it must never
    /// leave a child running after it returns.
    fn run(
        &self,
        request: &OneShotDelegationV1,
        cancellation: &CancellationToken,
    ) -> SubagentOutcomeV1;
}

/// The set of backends this host generation may delegate to.
#[derive(Default)]
pub struct ExternalAgentBackendRegistryV1 {
    backends: BTreeMap<String, Arc<dyn ExternalAgentBackendV1>>,
}

impl ExternalAgentBackendRegistryV1 {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one backend under its declared name.
    pub fn register(
        &mut self,
        backend: Arc<dyn ExternalAgentBackendV1>,
    ) -> Result<(), SubagentBackendErrorV1> {
        let name = backend.name().to_owned();
        if StableId::parse(name.clone()).is_err() {
            return Err(SubagentBackendErrorV1::InvalidBackendName);
        }
        if self.backends.contains_key(&name) {
            return Err(SubagentBackendErrorV1::DuplicateBackend(name));
        }
        self.backends.insert(name, backend);
        Ok(())
    }

    /// Returns a registered backend by name.
    pub fn get(
        &self,
        name: &str,
    ) -> Result<Arc<dyn ExternalAgentBackendV1>, SubagentBackendErrorV1> {
        self.backends
            .get(name)
            .cloned()
            .ok_or_else(|| SubagentBackendErrorV1::UnknownBackend(name.to_owned()))
    }

    /// Returns every registered backend name in stable order.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.backends.keys().map(String::as_str).collect()
    }

    /// Resolves the backend for one validated delegation, gating its options.
    pub fn resolve(
        &self,
        name: &str,
        request: &OneShotDelegationV1,
    ) -> Result<Arc<dyn ExternalAgentBackendV1>, SubagentBackendErrorV1> {
        request.validate()?;
        let backend = self.get(name)?;
        if let Some(capability) = backend.capabilities().first_unsupported(&request.options) {
            return Err(SubagentBackendErrorV1::UnsupportedCapability {
                backend: name.to_owned(),
                capability,
            });
        }
        Ok(backend)
    }
}

/// Bounds, truncates on a character boundary, and never retains a partial code point.
fn bounded_diagnostic(diagnostic: String) -> String {
    if diagnostic.len() <= MAXIMUM_DIAGNOSTIC_BYTES {
        return diagnostic;
    }
    let mut end = MAXIMUM_DIAGNOSTIC_BYTES;
    while end > 0 && !diagnostic.is_char_boundary(end) {
        end -= 1;
    }
    diagnostic[..end].to_owned()
}

/// Why a delegation could not be admitted to a backend.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SubagentBackendErrorV1 {
    /// The backend name is not a valid stable identifier.
    #[error("subagent backend name is not a valid stable identifier")]
    InvalidBackendName,
    /// A backend is already registered under this name.
    #[error("subagent backend '{0}' is already registered")]
    DuplicateBackend(String),
    /// No backend is registered under this name.
    #[error("subagent backend '{0}' is not registered")]
    UnknownBackend(String),
    /// The delegated task is empty, contains a NUL, or is too large.
    #[error("the delegated task is empty or exceeds the accepted size")]
    InvalidTask,
    /// The child working directory is not an absolute path.
    #[error("the delegated working directory must be absolute")]
    InvalidWorkingDirectory,
    /// The deadline is zero or exceeds the accepted bound.
    #[error("the delegation deadline is zero or exceeds the accepted bound")]
    InvalidDeadline,
    /// A start-time option is malformed.
    #[error("a delegated start option is malformed")]
    InvalidStartOptions,
    /// The backend does not support a requested start-time option.
    #[error("subagent backend '{backend}' does not support the requested {capability}")]
    UnsupportedCapability {
        /// Backend that refused the option.
        backend: String,
        /// Option the backend does not support.
        capability: &'static str,
    },
    /// A completed run carried no usable final answer.
    #[error("the delegated final answer is empty or exceeds the accepted size")]
    InvalidAnswer,
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc, time::Duration};

    use aworkit_protocol::StableId;
    use serde_json::json;

    use super::*;

    struct StubBackend {
        name: &'static str,
        capabilities: SubagentBackendCapabilitiesV1,
    }

    impl ExternalAgentBackendV1 for StubBackend {
        fn name(&self) -> &str {
            self.name
        }

        fn capabilities(&self) -> SubagentBackendCapabilitiesV1 {
            self.capabilities
        }

        fn run(
            &self,
            _request: &OneShotDelegationV1,
            _cancellation: &CancellationToken,
        ) -> SubagentOutcomeV1 {
            SubagentOutcomeV1::completed("done".to_owned()).expect("valid answer")
        }
    }

    fn request(options: SubagentStartOptionsV1) -> OneShotDelegationV1 {
        OneShotDelegationV1 {
            run_id: StableId::parse("run.child").expect("stable id"),
            task: "Summarize the module".to_owned(),
            working_directory: PathBuf::from("/tmp"),
            deadline: Duration::from_secs(300),
            options,
        }
    }

    fn registry() -> ExternalAgentBackendRegistryV1 {
        let mut registry = ExternalAgentBackendRegistryV1::new();
        registry
            .register(Arc::new(StubBackend {
                name: "codex",
                capabilities: SubagentBackendCapabilitiesV1::external_agent(),
            }))
            .expect("registers");
        registry
    }

    #[test]
    fn registration_rejects_invalid_and_duplicate_names() {
        let mut registry = ExternalAgentBackendRegistryV1::new();
        assert_eq!(
            registry.register(Arc::new(StubBackend {
                name: "not a name",
                capabilities: SubagentBackendCapabilitiesV1::external_agent(),
            })),
            Err(SubagentBackendErrorV1::InvalidBackendName)
        );
        assert!(
            registry
                .register(Arc::new(StubBackend {
                    name: "codex",
                    capabilities: SubagentBackendCapabilitiesV1::external_agent(),
                }))
                .is_ok()
        );
        assert_eq!(
            registry.register(Arc::new(StubBackend {
                name: "codex",
                capabilities: SubagentBackendCapabilitiesV1::external_agent(),
            })),
            Err(SubagentBackendErrorV1::DuplicateBackend("codex".to_owned()))
        );
        assert_eq!(registry.names(), vec!["codex"]);
    }

    #[test]
    fn unknown_backend_is_refused_by_name() {
        assert_eq!(
            registry()
                .resolve("claude-code", &request(SubagentStartOptionsV1::default()))
                .err(),
            Some(SubagentBackendErrorV1::UnknownBackend(
                "claude-code".to_owned()
            ))
        );
    }

    #[test]
    fn unsupported_start_options_are_refused_before_the_backend_runs() {
        let registry = registry();
        for (options, capability) in [
            (
                SubagentStartOptionsV1 {
                    model: Some("gpt-5".to_owned()),
                    ..SubagentStartOptionsV1::default()
                },
                "agent options",
            ),
            (
                SubagentStartOptionsV1 {
                    output_schema: Some(json!({"type": "object"})),
                    ..SubagentStartOptionsV1::default()
                },
                "output schema",
            ),
            (
                SubagentStartOptionsV1 {
                    maximum_depth: Some(2),
                    ..SubagentStartOptionsV1::default()
                },
                "depth limit",
            ),
            (
                SubagentStartOptionsV1 {
                    tool_filter: true,
                    ..SubagentStartOptionsV1::default()
                },
                "tool filter",
            ),
            (
                SubagentStartOptionsV1 {
                    persona: Some("reviewer".to_owned()),
                    ..SubagentStartOptionsV1::default()
                },
                "persona",
            ),
        ] {
            assert_eq!(
                registry.resolve("codex", &request(options)).err(),
                Some(SubagentBackendErrorV1::UnsupportedCapability {
                    backend: "codex".to_owned(),
                    capability,
                })
            );
        }
    }

    #[test]
    fn a_supported_option_reaches_the_backend() {
        let backend = StubBackend {
            name: "codex",
            capabilities: SubagentBackendCapabilitiesV1 {
                agent_options: true,
                ..SubagentBackendCapabilitiesV1::external_agent()
            },
        };
        let mut registry = ExternalAgentBackendRegistryV1::new();
        registry.register(Arc::new(backend)).expect("registers");
        let resolved = registry
            .resolve(
                "codex",
                &request(SubagentStartOptionsV1 {
                    model: Some("gpt-5".to_owned()),
                    ..SubagentStartOptionsV1::default()
                }),
            )
            .expect("resolves");
        assert_eq!(resolved.name(), "codex");
    }

    #[test]
    fn delegation_validation_refuses_an_empty_task_or_relative_directory() {
        let mut empty = request(SubagentStartOptionsV1::default());
        empty.task = "   ".to_owned();
        assert_eq!(empty.validate(), Err(SubagentBackendErrorV1::InvalidTask));

        let mut relative = request(SubagentStartOptionsV1::default());
        relative.working_directory = PathBuf::from("workspace");
        assert_eq!(
            relative.validate(),
            Err(SubagentBackendErrorV1::InvalidWorkingDirectory)
        );

        let mut unbounded = request(SubagentStartOptionsV1::default());
        unbounded.deadline = Duration::ZERO;
        assert_eq!(
            unbounded.validate(),
            Err(SubagentBackendErrorV1::InvalidDeadline)
        );
        assert!(
            request(SubagentStartOptionsV1::default())
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn completed_outcomes_require_a_non_blank_answer() {
        assert_eq!(
            SubagentOutcomeV1::completed("  \n".to_owned()),
            Err(SubagentBackendErrorV1::InvalidAnswer)
        );
        let outcome =
            SubagentOutcomeV1::completed("The module is bounded".to_owned()).expect("valid answer");
        assert!(outcome.stop_reason.is_completed());
        assert_eq!(outcome.answer.as_deref(), Some("The module is bounded"));
    }

    #[test]
    fn a_failure_never_settles_as_completed_and_bounds_its_diagnostic() {
        let outcome = SubagentOutcomeV1::failed(
            SubagentStopReasonV1::Completed,
            "product: Codex; category: transport",
        );
        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Unknown);
        assert!(outcome.answer.is_none());

        let long = "é".repeat(MAXIMUM_DIAGNOSTIC_BYTES);
        let outcome = SubagentOutcomeV1::failed(SubagentStopReasonV1::Limit, long);
        let diagnostic = outcome.diagnostic.expect("diagnostic");
        assert!(diagnostic.len() <= MAXIMUM_DIAGNOSTIC_BYTES);
        assert!(diagnostic.chars().all(|character| character == 'é'));

        let outcome = SubagentOutcomeV1::aborted();
        assert_eq!(outcome.stop_reason, SubagentStopReasonV1::Aborted);
        assert!(outcome.diagnostic.is_some());
    }
}
