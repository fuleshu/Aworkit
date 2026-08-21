//! Inert import validation and verified read-only projection pages.

use crate::{CanonicalCodec, PortableError, PortableEvent, PortablePaths};

/// Non-activating import result. Callers receive no capability or write surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportReport {
    pub accepted: bool,
    pub diagnostics: Vec<String>,
}
/// Bounded immutable page suitable only for disposable projections.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortablePage {
    pub events: Vec<PortableEvent>,
    pub next_ordinal: Option<u64>,
}
#[derive(Clone, Debug)]
pub struct ProjectionFeed {
    paths: PortablePaths,
    codec: CanonicalCodec,
}
impl ProjectionFeed {
    #[must_use]
    pub fn new(paths: PortablePaths) -> Self {
        Self {
            paths,
            codec: CanonicalCodec,
        }
    }
    pub fn validate_import(&self, segment_hash: &str) -> ImportReport {
        match self.paths.read("segments", segment_hash).and_then(|bytes| {
            self.codec
                .decode::<crate::PortableSegment>(&bytes)
                .map_err(|_| PortableError::CorruptObject)
        }) {
            Ok(_) => ImportReport {
                accepted: true,
                diagnostics: vec![],
            },
            Err(error) => ImportReport {
                accepted: false,
                diagnostics: vec![error.to_string()],
            },
        }
    }
    pub fn read_page(
        &self,
        segment_hash: &str,
        start: u64,
        limit: usize,
    ) -> Result<PortablePage, PortableError> {
        let bytes = self.paths.read("segments", segment_hash)?;
        let segment = self
            .codec
            .decode::<crate::PortableSegment>(&bytes)
            .map_err(|_| PortableError::CorruptObject)?;
        let events: Vec<_> = segment
            .events
            .into_iter()
            .filter(|event| event.ordinal >= start)
            .take(limit.min(256))
            .collect();
        let next_ordinal = events.last().map(|event| event.ordinal + 1);
        Ok(PortablePage {
            events,
            next_ordinal,
        })
    }
}
