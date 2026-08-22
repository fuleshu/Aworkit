//! Verified cursor reads over open and compressed diagnostic generations.

use std::{fs, path::Path};

use crate::bounded_codec::decompress;

use super::{
    model::{
        DiagnosticCursor, DiagnosticError, DiagnosticPage, DiagnosticRecord,
        DiagnosticSegmentMetadata, DiagnosticSegmentState, DiagnosticUnavailableRange,
    },
    writer::{WriterState, hash_bytes},
};

impl WriterState {
    pub(super) fn read_page(
        &mut self,
        cursor: Option<&DiagnosticCursor>,
        limit: u32,
    ) -> Result<DiagnosticPage, DiagnosticError> {
        let segments = self.segments();
        let start_index = cursor.map_or(Ok(0), |cursor| find_cursor_segment(&segments, cursor))?;
        let mut records = Vec::new();
        let mut unavailable_ranges = Vec::new();
        for segment in segments.iter().skip(start_index) {
            if records.len() >= usize::try_from(limit).map_err(|_| DiagnosticError::InvalidPage)? {
                break;
            }
            if matches!(
                segment.state,
                DiagnosticSegmentState::Expired | DiagnosticSegmentState::Corrupt
            ) {
                unavailable_ranges.push(DiagnosticUnavailableRange {
                    writer_generation: segment.writer_generation.clone(),
                    start_sequence: segment.start_sequence,
                    end_sequence: segment.end_sequence,
                    state: segment.state,
                    reason: segment
                        .unavailable_reason
                        .clone()
                        .unwrap_or_else(|| "unavailable".to_owned()),
                });
                continue;
            }
            let decoded = match read_segment(self.root(), segment) {
                Ok(decoded) => decoded,
                Err(_) if segment.state != DiagnosticSegmentState::Open => {
                    self.mark_corrupt(segment.segment_id)?;
                    unavailable_ranges.push(DiagnosticUnavailableRange {
                        writer_generation: segment.writer_generation.clone(),
                        start_sequence: segment.start_sequence,
                        end_sequence: segment.end_sequence,
                        state: DiagnosticSegmentState::Corrupt,
                        reason: "integrity_failure".to_owned(),
                    });
                    continue;
                }
                Err(error) => return Err(error),
            };
            for line in decoded
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
            {
                let record: DiagnosticRecord =
                    serde_json::from_slice(line).map_err(|_| DiagnosticError::CorruptSegment)?;
                if record.record_id.writer_generation != segment.writer_generation
                    || record.record_id.sequence < segment.start_sequence
                    || segment
                        .end_sequence
                        .is_some_and(|end| record.record_id.sequence > end)
                {
                    return Err(DiagnosticError::CorruptSegment);
                }
                if cursor.is_some_and(|cursor| {
                    record.record_id.writer_generation == cursor.writer_generation
                        && record.record_id.sequence <= cursor.sequence
                }) {
                    continue;
                }
                records.push(record);
                if records.len()
                    >= usize::try_from(limit).map_err(|_| DiagnosticError::InvalidPage)?
                {
                    break;
                }
            }
        }
        let next_cursor = records.last().map(|record| DiagnosticCursor {
            writer_generation: record.record_id.writer_generation.clone(),
            sequence: record.record_id.sequence,
        });
        Ok(DiagnosticPage {
            records,
            unavailable_ranges,
            next_cursor,
        })
    }
}

fn find_cursor_segment(
    segments: &[DiagnosticSegmentMetadata],
    cursor: &DiagnosticCursor,
) -> Result<usize, DiagnosticError> {
    segments
        .iter()
        .position(|segment| {
            segment.writer_generation == cursor.writer_generation
                && cursor.sequence >= segment.start_sequence
                && segment
                    .end_sequence
                    .is_none_or(|end| cursor.sequence <= end)
        })
        .ok_or(DiagnosticError::UnknownCursor)
}

fn read_segment(
    root: &Path,
    segment: &DiagnosticSegmentMetadata,
) -> Result<Vec<u8>, DiagnosticError> {
    let bytes = fs::read(root.join(&segment.file_name))?;
    let raw = match segment.state {
        DiagnosticSegmentState::Open => bytes,
        DiagnosticSegmentState::Available => decompress(
            &bytes,
            usize::try_from(segment.raw_byte_count)
                .map_err(|_| DiagnosticError::NumericOverflow)?,
        )?,
        DiagnosticSegmentState::Expired | DiagnosticSegmentState::Corrupt => {
            return Err(DiagnosticError::CorruptSegment);
        }
    };
    if u64::try_from(raw.len()).map_err(|_| DiagnosticError::NumericOverflow)?
        != segment.raw_byte_count
        || segment
            .content_hash
            .as_ref()
            .is_some_and(|expected| hash_bytes(&raw) != *expected)
    {
        return Err(DiagnosticError::CorruptSegment);
    }
    Ok(raw)
}
