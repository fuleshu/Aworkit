//! Approved-envelope adapter for exact verified-isolation execution.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    AdmissionReceipt, AdmittedInvocationDispatcherV1, ApprovedInvocationEnvelopeV1,
    CancellationToken, CapabilityKind,
};

use super::{
    IsolatedExecutionV1, IsolationBackendPortV1, IsolationProfileV1, IsolationRunReportV1,
    IsolationRuntime, IsolationRuntimeError,
};

/// Signed payload accepted by the isolation gateway adapter.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IsolationGatewayRequestV1 {
    pub profile: IsolationProfileV1,
    pub execution: IsolatedExecutionV1,
}

/// One-shot dispatcher that can only be reached after gateway admission.
pub struct IsolationGatewayDispatcherV1<B> {
    runtime: IsolationRuntime<B>,
}

impl<B: IsolationBackendPortV1> IsolationGatewayDispatcherV1<B> {
    #[must_use]
    pub fn new(runtime: IsolationRuntime<B>) -> Self {
        Self { runtime }
    }

    #[must_use]
    pub fn runtime(&self) -> &IsolationRuntime<B> {
        &self.runtime
    }
}

impl<B: IsolationBackendPortV1> AdmittedInvocationDispatcherV1 for IsolationGatewayDispatcherV1<B> {
    type Output = Result<IsolationRunReportV1, IsolationGatewayDispatchErrorV1>;

    fn dispatch(
        &self,
        envelope: &ApprovedInvocationEnvelopeV1,
        admission: &AdmissionReceipt,
        cancellation: &CancellationToken,
    ) -> Self::Output {
        if envelope.kind != CapabilityKind::Isolation
            || admission.descriptor.kind != CapabilityKind::Isolation
        {
            return Err(IsolationGatewayDispatchErrorV1::KindMismatch);
        }
        let request: IsolationGatewayRequestV1 =
            serde_json::from_value(envelope.payload.clone())
                .map_err(|_| IsolationGatewayDispatchErrorV1::InvalidPayload)?;
        let required_profile = envelope
            .required_isolation_profile
            .as_deref()
            .ok_or(IsolationGatewayDispatchErrorV1::ProfileDrift)?;
        if request.profile.profile_id != required_profile
            || request.execution.profile_id != required_profile
            || request.execution.profile_hash != request.profile.profile_hash
        {
            return Err(IsolationGatewayDispatchErrorV1::ProfileDrift);
        }
        if request.execution.invocation_id != envelope.invocation_id.as_str()
            || request.execution.deadline_epoch_millis != envelope.deadline_epoch_millis
        {
            return Err(IsolationGatewayDispatchErrorV1::InvocationDrift);
        }
        if request.execution.transfer_limits.maximum_result_bytes > envelope.max_output_bytes {
            return Err(IsolationGatewayDispatchErrorV1::OutputLimitBroadened);
        }
        self.runtime
            .execute(&request.profile, &request.execution, cancellation)
            .map_err(IsolationGatewayDispatchErrorV1::Runtime)
    }
}

#[derive(Debug, Error)]
pub enum IsolationGatewayDispatchErrorV1 {
    #[error("admitted capability is not an isolation adapter")]
    KindMismatch,
    #[error("signed isolation payload is malformed")]
    InvalidPayload,
    #[error("isolation profile differs from the admitted descriptor")]
    ProfileDrift,
    #[error("isolation invocation identity or deadline drifted")]
    InvocationDrift,
    #[error("isolation result limit broadens the admitted envelope")]
    OutputLimitBroadened,
    #[error(transparent)]
    Runtime(#[from] IsolationRuntimeError),
}
