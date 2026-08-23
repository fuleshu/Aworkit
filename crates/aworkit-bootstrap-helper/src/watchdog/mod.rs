//! Platform-neutral generation launch and watchdog policy (Milestone 11.5).

mod hermetic;
mod model;
mod ports;
mod watchdog;

#[cfg(test)]
mod tests;

pub use hermetic::{HermeticGenerationScriptV1, HermeticPlatformProcessPort};
pub use model::*;
pub use ports::{ApplicationLaunchWatchdogPortV1, PlatformProcessPortV1};
pub use watchdog::ApplicationLaunchWatchdog;
