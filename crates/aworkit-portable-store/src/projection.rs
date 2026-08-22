//! Inert import validation and verified read-only projection pages.

use std::collections::BTreeSet;

use crate::{
    ArtifactDescriptor, ArtifactStore, CanonicalCodec, ExportPolicy, OmissionFact, PortableError,
    PortableEvent, PortablePaths, digest,
};

const MAX_IMPORT_SEGMENTS: usize = 16_384;

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
    pub evidence: Vec<PortableProjectionEvidenceV1>,
    pub next_ordinal: Option<u64>,
    pub source_segment_hash: String,
    pub projection_token: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PortableProjectionEvidenceV1 {
    Omission(OmissionFact),
    ArtifactMetadata(ArtifactDescriptor),
}

#[derive(Clone, Debug)]
pub struct ProjectionFeed {
    paths: PortablePaths,
    codec: CanonicalCodec,
    policy: ExportPolicy,
}

impl ProjectionFeed {
    #[must_use]
    pub fn new(paths: PortablePaths) -> Self {
        Self {
            paths,
            codec: CanonicalCodec,
            policy: ExportPolicy,
        }
    }

    /// Validates an entire segment and its portable-data policy. Imported
    /// command-shaped payloads remain inert values; this type exposes no
    /// dispatch or authority interface.
    pub fn validate_import(&self, segment_hash: &str) -> ImportReport {
        match self.load_segment(segment_hash) {
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

    /// Validates the complete parent-linked branch, including cycle and
    /// ordinal continuity checks, before an imported tip is accepted.
    pub fn validate_lineage(&self, tip_hash: &str) -> ImportReport {
        let result = (|| -> Result<(), PortableError> {
            let mut seen = BTreeSet::new();
            let mut cursor = Some(tip_hash.to_owned());
            let mut child_first = None;
            while let Some(hash) = cursor {
                if seen.len() >= MAX_IMPORT_SEGMENTS || !seen.insert(hash.clone()) {
                    return Err(PortableError::CorruptObject);
                }
                let segment = self.load_segment(&hash)?;
                let segment_end = segment
                    .first_ordinal
                    .checked_add(u64::try_from(segment.events.len()).expect("bounded"))
                    .ok_or(PortableError::CorruptObject)?;
                if child_first.is_some_and(|first| first != segment_end) {
                    return Err(PortableError::CorruptObject);
                }
                child_first = Some(segment.first_ordinal);
                cursor = segment.parent_segment_hash;
            }
            Ok(())
        })();
        match result {
            Ok(()) => ImportReport {
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
        if !self.validate_lineage(segment_hash).accepted {
            return Err(PortableError::CorruptObject);
        }
        let segment = self.load_segment(segment_hash)?;
        let mut evidence = Vec::new();
        if let Some(context) = &segment.context {
            evidence.extend(
                context
                    .provenance
                    .omissions
                    .iter()
                    .cloned()
                    .map(PortableProjectionEvidenceV1::Omission),
            );
            evidence.extend(
                context
                    .provenance
                    .artifact_metadata
                    .iter()
                    .cloned()
                    .map(PortableProjectionEvidenceV1::ArtifactMetadata),
            );
        }
        let limit = limit.min(256);
        let eligible: Vec<_> = segment
            .events
            .into_iter()
            .filter(|event| event.ordinal >= start)
            .collect();
        let has_more = eligible.len() > limit;
        let events: Vec<_> = eligible.into_iter().take(limit).collect();
        let next_ordinal = if limit == 0 && has_more {
            Some(start)
        } else if has_more {
            events.last().and_then(|event| event.ordinal.checked_add(1))
        } else {
            None
        };
        Ok(PortablePage {
            events,
            evidence,
            next_ordinal,
            source_segment_hash: segment_hash.to_owned(),
            projection_token: digest(
                "portable-projection-page-v1",
                format!("{segment_hash}\0{start}\0{limit}").as_bytes(),
            ),
        })
    }

    fn load_segment(&self, segment_hash: &str) -> Result<crate::PortableSegment, PortableError> {
        let bytes = self.paths.read("segments", segment_hash)?;
        let segment = self
            .codec
            .decode_segment(&bytes)
            .map_err(|_| PortableError::CorruptObject)?;
        for event in &segment.events {
            let scrubbed = self
                .policy
                .scrub(&event.payload)
                .map_err(|_| PortableError::CorruptObject)?;
            if scrubbed.value != event.payload || !scrubbed.omissions.is_empty() {
                return Err(PortableError::CorruptObject);
            }
        }
        if let Some(context) = &segment.context {
            let artifacts = ArtifactStore::new(self.paths.clone());
            for descriptor in &context.provenance.artifact_metadata {
                artifacts
                    .read_verified(descriptor)
                    .map_err(|_| PortableError::CorruptObject)?;
            }
        }
        Ok(segment)
    }
}
