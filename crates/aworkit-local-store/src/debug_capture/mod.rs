//! Optional, independently retained protocol and stream evidence.
//!
//! Capture is never canonical Chat history. Every complete frame is redacted
//! before compression, hard quotas seal a truncated manifest instead of
//! applying backpressure to semantic commits, and corruption/expiry leave
//! inspectable metadata after payload bytes are discarded.

mod common;
mod maintenance;
mod model;
mod read;
mod store;
mod write;

#[cfg(test)]
mod tests;

pub use model::{
    CaptureAppendOutcome, CaptureChunk, CaptureChunkMetadata, CaptureCorrelation, CaptureError,
    CaptureFrame, CaptureManifest, CapturePage, CapturePolicy, CaptureRequest, CaptureSource,
    CaptureState, CaptureStoreMode, RetentionReport,
};
pub use read::CaptureReader;
pub use store::DebugCaptureStore;
