//! Compatibility names for the Run event callback exposed by the desktop API.

pub use super::run_events::{
    RunEventEnvelopeV1 as LiveChatActivityV1, RunEventPort as LiveChatActivityPort,
};

pub(crate) use super::run_events::noop_run_event_port as noop_live_activity;
