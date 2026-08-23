//! Trusted bootstrap and rollback helper.
//!
//! This crate is the independently surviving helper that performs managed-local
//! enrollment and bootstrap activation for the trusted core. Milestone 11 builds
//! its platform-neutral components against typed ports; native filesystem,
//! selector, process, and IPC implementations arrive in Milestone 12.
//!
//! The crate exposes the tamper-evident enrollment and activation journal
//! (Milestone 11.1) and the authenticated bootstrap protocol and generation
//! fence (Milestone 11.2).

pub mod coordinator;
pub mod journal;
pub mod profile;
pub mod protocol;
pub mod slots;
pub mod watchdog;

pub use coordinator::{
    ActivationControlPortV1, ActivationExecutionV1, ActivationRollbackCoordinator, CoordinatorError,
};

pub use journal::{
    ActivationJournal, ActivationJournalPortV1, BootstrapJournalError, FilesystemJournalStorage,
    InMemoryJournalStorage, JournalStorage,
};
pub use profile::{
    HermeticSelectorPort, ManagedLocalBuildProfileAdapter, ManagedLocalSelectorAdapter,
    PlatformActivationPortV1, ProfileError, ProfileObservationPortV1, SelectorMutationPortV1,
};
pub use protocol::{
    ArcActivationJournal, BootstrapEnrollmentPortV1, BootstrapGateway, BootstrapPreflightPortV1,
    BootstrapProtocolPortV1, GatewayError,
};
pub use slots::{
    BootstrapArtifactReadPortV1, BuildSlotError, BuildSlotStoragePortV1, BuildSlotVerifyPortV1,
    ImmutableBuildSlotManager, InMemoryBuildSlotStorage,
};
pub use watchdog::{
    ApplicationLaunchWatchdog, ApplicationLaunchWatchdogPortV1, HermeticPlatformProcessPort,
    PlatformProcessPortV1,
};
