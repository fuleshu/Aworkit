//! Typed ports for the authenticated bootstrap protocol gateway.
//!
//! The gateway is the helper's sole core-facing boundary. It holds the
//! activation journal (which owns durability) plus two unprivileged ports it
//! does not implement itself: the platform profile preflight (M11.4) and the
//! managed-local enrollment preparer (M11.3/M11.6). Both are injected as
//! platform-neutral ports so the gateway can be tested hermetically.

use std::sync::Arc;

use aworkit_protocol::ProcessGeneration;
use aworkit_protocol::{
    AuthenticatedBootstrapResultV1, BootstrapAdmissionV1, BuildBundleRefV1, BuildProvenanceV1,
    EnrollmentPreparedV1, ManagedLocalEnrollmentRequestV1, PlatformCapabilityReportV1,
    RepairActivationBatonV1,
};

use crate::journal::ActivationJournalPortV1;

use super::error::GatewayError;
use super::model::{
    BootstrapChallengeV1, BootstrapCommandAckV1, BootstrapCommandV1, EnrollmentPlanV1,
    EnrollmentPreparationV1, LocalBuildEnrollmentV1, PeerIdentityV1,
};

/// The gateway's core-facing, versioned local protocol port.
///
/// Only the trusted core (through its OS-authenticated channel) holds
/// operational access. There is no UI, worker, capability-host, portable-store,
/// or plugin port.
pub trait BootstrapProtocolPortV1: Send + Sync {
    /// Issues a one-use challenge binding a peer to this protocol version.
    fn begin_bootstrap_challenge(
        &self,
        now_epoch_ms: u64,
        peer: &PeerIdentityV1,
    ) -> Result<BootstrapChallengeV1, GatewayError>;

    /// Returns a bounded, advisory platform capability report.
    fn query_activation_capability(
        &self,
        provenance: &BuildProvenanceV1,
        enrollment: &LocalBuildEnrollmentV1,
        candidate: &BuildBundleRefV1,
        previous: Option<&BuildBundleRefV1>,
    ) -> Result<PlatformCapabilityReportV1, GatewayError>;

    /// Prepares a managed-local enrollment root and journals its intent before
    /// returning the durable prepared receipt.
    fn prepare_managed_local_enrollment(
        &self,
        request: &ManagedLocalEnrollmentRequestV1,
    ) -> Result<EnrollmentPreparedV1, GatewayError>;

    /// Admits an authenticated, generation-fenced activation baton, or returns
    /// a protected Unsupported receipt when the guarantee changed.
    fn submit_repair_activation_baton(
        &self,
        now_epoch_ms: u64,
        peer: &PeerIdentityV1,
        baton: &RepairActivationBatonV1,
    ) -> Result<BootstrapAdmissionV1, GatewayError>;

    /// Admits one closed-union bootstrap command after fencing and dedup,
    /// journaling its hash before acknowledgement.
    fn submit_bootstrap_command(
        &self,
        command: &BootstrapCommandV1,
    ) -> Result<BootstrapCommandAckV1, GatewayError>;

    /// Returns the protected result only to its sealed recipient generation.
    fn read_bootstrap_result(
        &self,
        recipient: &ProcessGeneration,
    ) -> Result<AuthenticatedBootstrapResultV1, GatewayError>;
}

/// Unprivileged profile preflight: capability classification and enrollment
/// planning. Implemented by the platform profile adapter (M11.4).
pub trait BootstrapPreflightPortV1: Send + Sync {
    /// Classifies a candidate build into a bounded capability report.
    fn capability_report(
        &self,
        provenance: &BuildProvenanceV1,
        enrollment: &LocalBuildEnrollmentV1,
        candidate: &BuildBundleRefV1,
        previous: Option<&BuildBundleRefV1>,
    ) -> Result<PlatformCapabilityReportV1, String>;

    /// Independently revalidates the live capability binding named by an
    /// admitted baton, so a changed or drifted generation fails closed.
    fn revalidate_baton_binding(
        &self,
        baton: &RepairActivationBatonV1,
    ) -> Result<PlatformCapabilityReportV1, String>;

    /// Emits the fixed helper-controlled enrollment plan for a request.
    fn enrollment_plan(
        &self,
        request: &ManagedLocalEnrollmentRequestV1,
    ) -> Result<EnrollmentPlanV1, String>;
}

/// Unprivileged managed-local enrollment preparer. Implemented by the build
/// slot and coordinator components (M11.3/M11.6).
pub trait BootstrapEnrollmentPortV1: Send + Sync {
    /// Materializes and durably verifies the bounded enrollment root, returning
    /// the exact observation and the terminal prepared receipt.
    fn materialize(
        &self,
        request: &ManagedLocalEnrollmentRequestV1,
        plan: &EnrollmentPlanV1,
    ) -> Result<EnrollmentPreparationV1, String>;
}

/// Convenience alias for an injected activation journal.
pub type ArcActivationJournal = Arc<dyn ActivationJournalPortV1>;
