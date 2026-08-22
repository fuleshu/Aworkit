//! Trusted, same-user extension subprocess contracts.
//!
//! Parsing and attestation are deliberately separate from process launch. A
//! manifest is inert data until the trusted core supplies an exact enabled,
//! compatible pin for the active capability-host generation.

mod lifecycle;
mod manifest;
mod process;
mod protocol;

pub use lifecycle::{
    PluginDispatchPhaseV1, PluginInvocationSettlementV1, PluginLifecycleError,
    PluginLifecycleLimitsV1, PluginLifecycleStateV1, PluginReplayDispositionV1,
    PluginRestartPolicyV1, TrustedPluginLifecycleV1,
};
pub use manifest::{
    AttestedPluginPinV1, ExtensionManifestV1, PinnedPluginManifestV1, PluginContributionKindV1,
    PluginContributionV1, PluginDependencyV1, PluginEntryPointV1, PluginManifestError,
    PluginManifestLimitsV1, PluginPinError, parse_extension_manifest_v1,
};
pub use process::{
    NativePluginProcessV1, PluginProcessDiagnosticsV1, PluginProcessError, PluginProcessExitV1,
    PluginProcessLimitsV1,
};
pub use protocol::{
    PluginCancelRequestV1, PluginCancelResultV1, PluginEffectStatusV1, PluginFrameCodecV1,
    PluginFrameDecoderV1, PluginFrameError, PluginHandshakeIdentityV1, PluginHandshakeRequestV1,
    PluginHandshakeResultV1, PluginHealthRequestV1, PluginHealthResultV1, PluginHealthStatusV1,
    PluginInvocationAcceptedV1, PluginInvocationEventKindV1, PluginInvocationEventV1,
    PluginInvocationRequestV1, PluginInvocationResultV1, PluginProtocolErrorV1,
    PluginProtocolFrameV1, PluginProtocolLimitsV1, PluginProtocolMessageV1,
    PluginShutdownRequestV1, PluginShutdownResultV1, PluginTerminalStatusV1,
};

/// Required user-facing truth about the ordinary extension process boundary.
///
/// A plugin can bypass Aworkit-mediated file, network, approval, and secret
/// brokers because it executes with the desktop user's operating-system rights.
pub const TRUSTED_PLUGIN_SECURITY_DISCLOSURE: &str = concat!(
    "Trusted plugins run with the desktop user's operating-system authority. The subprocess ",
    "boundary provides lifecycle and crash containment; it is not a security sandbox."
);
