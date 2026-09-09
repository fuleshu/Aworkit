//! Native visible-pixel screenshots. Captures are immutable tool results; no
//! clipboard mutation, temporary user files or input automation is involved.
use serde_json::Value;

pub(crate) struct Capture {
    pub bytes: Vec<u8>,
    pub source: Value,
}

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{capture, list};

#[cfg(not(windows))]
pub(crate) fn list() -> Result<Value, String> {
    Err("Screenshot capture is currently supported on Windows only.".into())
}
#[cfg(not(windows))]
pub(crate) fn capture(_target: &str) -> Result<Capture, String> {
    Err("Screenshot capture is currently supported on Windows only.".into())
}
