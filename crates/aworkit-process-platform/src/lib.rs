//! Owned process trees. No process runs before joining its cleanup boundary.
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::ProcessTree;
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::ProcessTree;
