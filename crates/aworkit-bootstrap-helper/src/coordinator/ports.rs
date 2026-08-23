//! Core-facing coordinator policy port.

use aworkit_protocol::BootstrapResultV1;
use aworkit_protocol::StableId;

use super::{ActivationExecutionV1, CoordinatorError};

/// Executes or conservatively recovers one already-admitted activation.
pub trait ActivationControlPortV1: Send + Sync {
    fn execute_activation(
        &self,
        execution: &ActivationExecutionV1,
    ) -> Result<BootstrapResultV1, CoordinatorError>;

    fn recover_activation(
        &self,
        activation_id: &StableId,
        execution: Option<&ActivationExecutionV1>,
    ) -> Result<BootstrapResultV1, CoordinatorError>;
}
