//! Native projection gateway for trusted-core Management repair contracts.

mod dto;
mod gateway;
mod local_ledger;
mod ports;
mod projection;

pub use dto::ManagementRepairProjectionDto;
pub use gateway::{
    ManagementRepairCommandInput, ManagementRepairGateway, ManagementRepairNativeContext,
    ManagementRepairReceipt,
};
pub use local_ledger::LocalRepairLedgerAdapter;
pub use ports::{GloballyCommittedRepairEventV1, ManagementRepairProjectionPortV1};
