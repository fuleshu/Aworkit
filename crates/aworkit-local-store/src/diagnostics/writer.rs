//! Crash-tolerant writer and rotation state, owned by one background thread.

use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    bounded_codec::{compress, decompress},
    filesystem::write_and_sync_atomic,
};

use super::model::{
    DiagnosticError, DiagnosticLogConfig, DiagnosticRecord, DiagnosticRecordId,
    DiagnosticRetentionReport, DiagnosticSegmentMetadata, DiagnosticSegmentState,
    HARD_MAX_SEGMENT_BYTES, HARD_MAX_SEGMENTS, manifest_path, valid_id,
};

const MANIFEST_VERSION: u16 = 1;
const MAX_TOMBSTONES: usize = 256;
const MAX_MANIFEST_SEGMENTS: usize = HARD_MAX_SEGMENTS + MAX_TOMBSTONES + 1;
const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RotationManifest {
    schema_version: u16,
    next_segment_id: u64,
    segments: Vec<DiagnosticSegmentMetadata>,
}

pub(super) struct WriterState {
    root: PathBuf,
    config: DiagnosticLogConfig,
    manifest: RotationManifest,
    active: ActiveSegment,
}

struct ActiveSegment {
    segment_id: u64,
    path: PathBuf,
    file: File,
    next_sequence: u64,
    bytes: u64,
    last_epoch_ms: u64,
}

impl WriterState {
    pub(super) fn open(
        root: PathBuf,
        config: DiagnosticLogConfig,
        opened_at_epoch_ms: u64,
    ) -> Result<Self, DiagnosticError> {
        fs::create_dir_all(&root)?;
        let mut manifest = load_manifest(&root)?;
        recover_open_segments(&root, &mut manifest, config.max_segment_bytes)?;
        remove_terminal_payloads(&root, &manifest)?;
        compact_tombstones(&mut manifest);
        remove_orphan_segments(&root, &manifest)?;
        let next_sequence = manifest
            .segments
            .iter()
            .filter(|segment| segment.writer_generation == config.writer_generation)
            .filter_map(|segment| segment.end_sequence)
            .max()
            .map_or(0, |sequence| sequence.saturating_add(1));
        let active = create_active(
            &root,
            &config.writer_generation,
            &mut manifest,
            next_sequence,
            opened_at_epoch_ms,
        )?;
        save_manifest(&root, &manifest)?;
        Ok(Self {
            root,
            config,
            manifest,
            active,
        })
    }

    pub(super) fn write(
        &mut self,
        mut record: DiagnosticRecord,
    ) -> Result<DiagnosticRecordId, DiagnosticError> {
        record.record_id = DiagnosticRecordId {
            writer_generation: self.config.writer_generation.clone(),
            sequence: self.active.next_sequence,
        };
        let mut line = serde_json::to_vec(&record)?;
        line.push(b'\n');
        let line_bytes = u64::try_from(line.len()).map_err(|_| DiagnosticError::NumericOverflow)?;
        if self.active.bytes > 0
            && self.active.bytes.saturating_add(line_bytes) > self.config.max_segment_bytes
        {
            self.rotate(record.occurred_at_epoch_ms)?;
            record.record_id = DiagnosticRecordId {
                writer_generation: self.config.writer_generation.clone(),
                sequence: self.active.next_sequence,
            };
            line = serde_json::to_vec(&record)?;
            line.push(b'\n');
        }
        if u64::try_from(line.len()).map_err(|_| DiagnosticError::NumericOverflow)?
            > self.config.max_segment_bytes
        {
            return Err(DiagnosticError::InvalidRecord);
        }
        self.active.file.write_all(&line)?;
        self.active.bytes = self
            .active
            .bytes
            .checked_add(u64::try_from(line.len()).map_err(|_| DiagnosticError::NumericOverflow)?)
            .ok_or(DiagnosticError::NumericOverflow)?;
        self.active.last_epoch_ms = record.occurred_at_epoch_ms;
        self.active.next_sequence = self
            .active
            .next_sequence
            .checked_add(1)
            .ok_or(DiagnosticError::NumericOverflow)?;
        let active_bytes = self.active.bytes;
        let metadata = self.active_metadata_mut()?;
        metadata.end_sequence = Some(record.record_id.sequence);
        metadata.raw_byte_count = active_bytes;
        metadata.stored_byte_count = active_bytes;
        Ok(record.record_id)
    }

    pub(super) fn flush(&mut self, now_epoch_ms: u64) -> Result<(), DiagnosticError> {
        self.active.file.flush()?;
        self.active.file.sync_data()?;
        let active_bytes = self.active.bytes;
        let metadata = self.active_metadata_mut()?;
        metadata.raw_byte_count = active_bytes;
        metadata.stored_byte_count = active_bytes;
        save_manifest(&self.root, &self.manifest)?;
        let _ = self.enforce_retention(now_epoch_ms)?;
        Ok(())
    }

    pub(super) fn segments(&self) -> Vec<DiagnosticSegmentMetadata> {
        self.manifest.segments.clone()
    }

    pub(super) fn enforce_retention(
        &mut self,
        now_epoch_ms: u64,
    ) -> Result<DiagnosticRetentionReport, DiagnosticError> {
        remove_terminal_payloads(&self.root, &self.manifest)?;
        let mut available = self
            .manifest
            .segments
            .iter()
            .enumerate()
            .filter(|(_, segment)| segment.state == DiagnosticSegmentState::Available)
            .map(|(index, segment)| {
                (
                    index,
                    segment.created_at_epoch_ms,
                    segment.stored_byte_count,
                )
            })
            .collect::<Vec<_>>();
        available.sort_by_key(|(_, created, _)| *created);
        let mut total = available
            .iter()
            .map(|(_, _, bytes)| *bytes)
            .sum::<u64>()
            .saturating_add(self.active.bytes);
        let excess_count = available.len().saturating_sub(self.config.max_segments);
        let mut report = DiagnosticRetentionReport::default();
        let mut expired_paths = Vec::new();
        for (position, (index, created_at, bytes)) in available.into_iter().enumerate() {
            let age_expired = created_at.saturating_add(self.config.max_age_ms) <= now_epoch_ms;
            let quota_expired = total > self.config.max_total_bytes;
            if position >= excess_count && !age_expired && !quota_expired {
                continue;
            }
            let segment = &mut self.manifest.segments[index];
            expired_paths.push(self.root.join(&segment.file_name));
            segment.state = DiagnosticSegmentState::Expired;
            segment.unavailable_reason = Some(if age_expired {
                "age_retention".to_owned()
            } else if position < excess_count {
                "generation_retention".to_owned()
            } else {
                "global_quota".to_owned()
            });
            total = total.saturating_sub(bytes);
            report.expired_segments = report.expired_segments.saturating_add(1);
            report.removed_bytes = report.removed_bytes.saturating_add(bytes);
        }
        if report.expired_segments > 0 {
            // Publish unavailability before deleting bytes. Recovery can then
            // safely retry any interrupted physical cleanup.
            save_manifest(&self.root, &self.manifest)?;
            for path in expired_paths {
                remove_file_if_present(path)?;
            }
        }
        compact_tombstones(&mut self.manifest);
        save_manifest(&self.root, &self.manifest)?;
        Ok(report)
    }

    pub(super) fn mark_corrupt(&mut self, segment_id: u64) -> Result<(), DiagnosticError> {
        let segment = self
            .manifest
            .segments
            .iter_mut()
            .find(|segment| segment.segment_id == segment_id)
            .ok_or(DiagnosticError::CorruptManifest)?;
        if segment.state == DiagnosticSegmentState::Open {
            return Err(DiagnosticError::CorruptSegment);
        }
        let path = self.root.join(&segment.file_name);
        segment.state = DiagnosticSegmentState::Corrupt;
        segment.unavailable_reason = Some("integrity_failure".to_owned());
        save_manifest(&self.root, &self.manifest)?;
        remove_file_if_present(path)?;
        compact_tombstones(&mut self.manifest);
        save_manifest(&self.root, &self.manifest)
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    fn rotate(&mut self, closed_at_epoch_ms: u64) -> Result<(), DiagnosticError> {
        self.active.file.flush()?;
        self.active.file.sync_all()?;
        let raw = fs::read(&self.active.path)?;
        if u64::try_from(raw.len()).map_err(|_| DiagnosticError::NumericOverflow)?
            != self.active.bytes
        {
            return Err(DiagnosticError::CorruptSegment);
        }
        let encoded = compress(&raw);
        let closed_name = format!("segment-{:020}.rle", self.active.segment_id);
        let closed_path = self.root.join(&closed_name);
        write_and_sync_atomic(&closed_path, &encoded)?;
        let prior_open_path = self.active.path.clone();
        let raw_hash = hash_bytes(&raw);
        let stored = u64::try_from(encoded.len()).map_err(|_| DiagnosticError::NumericOverflow)?;
        let active_bytes = self.active.bytes;
        let metadata = self.active_metadata_mut()?;
        metadata.closed_at_epoch_ms = Some(closed_at_epoch_ms);
        metadata.raw_byte_count = active_bytes;
        metadata.stored_byte_count = stored;
        metadata.content_hash = Some(raw_hash);
        metadata.state = DiagnosticSegmentState::Available;
        metadata.file_name = closed_name;

        let next_sequence = self.active.next_sequence;
        self.active = create_active(
            &self.root,
            &self.config.writer_generation,
            &mut self.manifest,
            next_sequence,
            closed_at_epoch_ms,
        )?;
        save_manifest(&self.root, &self.manifest)?;
        match fs::remove_file(prior_open_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let _ = self.enforce_retention(closed_at_epoch_ms)?;
        Ok(())
    }

    fn active_metadata_mut(&mut self) -> Result<&mut DiagnosticSegmentMetadata, DiagnosticError> {
        self.manifest
            .segments
            .iter_mut()
            .find(|segment| segment.segment_id == self.active.segment_id)
            .ok_or(DiagnosticError::CorruptManifest)
    }
}

fn load_manifest(root: &Path) -> Result<RotationManifest, DiagnosticError> {
    let path = manifest_path(root);
    if !path.exists() {
        return Ok(RotationManifest {
            schema_version: MANIFEST_VERSION,
            next_segment_id: 0,
            segments: Vec::new(),
        });
    }
    if fs::metadata(&path)?.len() > MAX_MANIFEST_BYTES {
        return Err(DiagnosticError::CorruptManifest);
    }
    let manifest: RotationManifest = serde_json::from_slice(&fs::read(path)?)?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn save_manifest(root: &Path, manifest: &RotationManifest) -> Result<(), DiagnosticError> {
    validate_manifest(manifest)?;
    write_and_sync_atomic(&manifest_path(root), &serde_jcs::to_vec(manifest)?)?;
    Ok(())
}

fn validate_manifest(manifest: &RotationManifest) -> Result<(), DiagnosticError> {
    if manifest.schema_version != MANIFEST_VERSION
        || manifest.segments.len() > MAX_MANIFEST_SEGMENTS
    {
        return Err(DiagnosticError::CorruptManifest);
    }
    let mut ids = BTreeSet::new();
    for segment in &manifest.segments {
        if !ids.insert(segment.segment_id)
            || !valid_id(&segment.writer_generation)
            || segment
                .end_sequence
                .is_some_and(|end| end < segment.start_sequence)
            || segment.raw_byte_count > HARD_MAX_SEGMENT_BYTES
            || segment.stored_byte_count > HARD_MAX_SEGMENT_BYTES.saturating_mul(2)
            || segment
                .content_hash
                .as_deref()
                .is_some_and(|hash| !valid_hash(hash))
        {
            return Err(DiagnosticError::CorruptManifest);
        }
        let open_name = segment_file_name(segment.segment_id, "jsonl");
        let closed_name = segment_file_name(segment.segment_id, "rle");
        let valid_state = match segment.state {
            DiagnosticSegmentState::Open => {
                segment.file_name == open_name
                    && segment.closed_at_epoch_ms.is_none()
                    && segment.content_hash.is_none()
                    && segment.unavailable_reason.is_none()
            }
            DiagnosticSegmentState::Available => {
                segment.file_name == closed_name
                    && segment.closed_at_epoch_ms.is_some()
                    && segment.content_hash.is_some()
                    && segment.unavailable_reason.is_none()
            }
            DiagnosticSegmentState::Expired => {
                segment.file_name == closed_name && segment.unavailable_reason.is_some()
            }
            DiagnosticSegmentState::Corrupt => {
                (segment.file_name == open_name || segment.file_name == closed_name)
                    && segment.unavailable_reason.is_some()
            }
        };
        if !valid_state {
            return Err(DiagnosticError::CorruptManifest);
        }
    }
    let expected_next = ids.last().copied().map_or(0, |id| id.saturating_add(1));
    if manifest.next_segment_id != expected_next {
        return Err(DiagnosticError::CorruptManifest);
    }
    Ok(())
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 71
        && hash.starts_with("sha256:")
        && hash[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn segment_file_name(segment_id: u64, extension: &str) -> String {
    format!("segment-{segment_id:020}.{extension}")
}

fn create_active(
    root: &Path,
    writer_generation: &str,
    manifest: &mut RotationManifest,
    next_sequence: u64,
    created_at_epoch_ms: u64,
) -> Result<ActiveSegment, DiagnosticError> {
    let segment_id = manifest.next_segment_id;
    manifest.next_segment_id = manifest
        .next_segment_id
        .checked_add(1)
        .ok_or(DiagnosticError::NumericOverflow)?;
    let file_name = format!("segment-{segment_id:020}.jsonl");
    let path = root.join(&file_name);
    let file = OpenOptions::new()
        .create_new(true)
        .append(true)
        .open(&path)?;
    manifest.segments.push(DiagnosticSegmentMetadata {
        segment_id,
        writer_generation: writer_generation.to_owned(),
        start_sequence: next_sequence,
        end_sequence: None,
        created_at_epoch_ms,
        closed_at_epoch_ms: None,
        raw_byte_count: 0,
        stored_byte_count: 0,
        content_hash: None,
        state: DiagnosticSegmentState::Open,
        unavailable_reason: None,
        file_name,
    });
    Ok(ActiveSegment {
        segment_id,
        path,
        file,
        next_sequence,
        bytes: 0,
        last_epoch_ms: created_at_epoch_ms,
    })
}

fn recover_open_segments(
    root: &Path,
    manifest: &mut RotationManifest,
    max_segment_bytes: u64,
) -> Result<(), DiagnosticError> {
    let mut obsolete_paths = Vec::new();
    for segment in manifest
        .segments
        .iter_mut()
        .filter(|segment| segment.state == DiagnosticSegmentState::Open)
    {
        let open_path = root.join(&segment.file_name);
        let closed_name = segment_file_name(segment.segment_id, "rle");
        let closed_path = root.join(&closed_name);
        let raw = if open_path.exists() {
            read_bounded(&open_path, max_segment_bytes)?
        } else if closed_path.exists() {
            let encoded = read_bounded(&closed_path, max_segment_bytes.saturating_mul(2))?;
            decompress(
                &encoded,
                usize::try_from(max_segment_bytes).map_err(|_| DiagnosticError::NumericOverflow)?,
            )?
        } else {
            segment.state = DiagnosticSegmentState::Corrupt;
            segment.unavailable_reason = Some("missing_open_segment".to_owned());
            continue;
        };
        let Ok((last, closed_at_epoch_ms)) = validate_open_records(&raw, segment) else {
            segment.state = DiagnosticSegmentState::Corrupt;
            segment.unavailable_reason = Some("partial_open_segment".to_owned());
            obsolete_paths.push(open_path);
            obsolete_paths.push(closed_path);
            continue;
        };
        let encoded = compress(&raw);
        write_and_sync_atomic(&closed_path, &encoded)?;
        obsolete_paths.push(open_path);
        segment.end_sequence = last;
        segment.closed_at_epoch_ms = Some(closed_at_epoch_ms);
        segment.raw_byte_count =
            u64::try_from(raw.len()).map_err(|_| DiagnosticError::NumericOverflow)?;
        segment.stored_byte_count =
            u64::try_from(encoded.len()).map_err(|_| DiagnosticError::NumericOverflow)?;
        segment.content_hash = Some(hash_bytes(&raw));
        segment.state = DiagnosticSegmentState::Available;
        segment.file_name = closed_name;
    }
    save_manifest(root, manifest)?;
    for path in obsolete_paths {
        remove_file_if_present(path)?;
    }
    compact_tombstones(manifest);
    save_manifest(root, manifest)
}

fn validate_open_records(
    raw: &[u8],
    segment: &DiagnosticSegmentMetadata,
) -> Result<(Option<u64>, u64), DiagnosticError> {
    let mut expected = segment.start_sequence;
    let mut last = None;
    let mut closed_at_epoch_ms = segment.created_at_epoch_ms;
    for line in BufReader::new(raw).lines() {
        let line = line.map_err(|_| DiagnosticError::CorruptSegment)?;
        let record: DiagnosticRecord =
            serde_json::from_str(&line).map_err(|_| DiagnosticError::CorruptSegment)?;
        if record.record_id.writer_generation != segment.writer_generation
            || record.record_id.sequence != expected
        {
            return Err(DiagnosticError::CorruptSegment);
        }
        expected = expected
            .checked_add(1)
            .ok_or(DiagnosticError::NumericOverflow)?;
        last = Some(record.record_id.sequence);
        closed_at_epoch_ms = closed_at_epoch_ms.max(record.occurred_at_epoch_ms);
    }
    Ok((last, closed_at_epoch_ms))
}

fn read_bounded(path: &Path, max_bytes: u64) -> Result<Vec<u8>, DiagnosticError> {
    if fs::metadata(path)?.len() > max_bytes {
        return Err(DiagnosticError::CorruptSegment);
    }
    Ok(fs::read(path)?)
}

fn remove_file_if_present(path: impl AsRef<Path>) -> Result<(), DiagnosticError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn remove_terminal_payloads(
    root: &Path,
    manifest: &RotationManifest,
) -> Result<(), DiagnosticError> {
    for segment in manifest.segments.iter().filter(|segment| {
        matches!(
            segment.state,
            DiagnosticSegmentState::Expired | DiagnosticSegmentState::Corrupt
        )
    }) {
        remove_file_if_present(root.join(&segment.file_name))?;
    }
    Ok(())
}

fn remove_orphan_segments(root: &Path, manifest: &RotationManifest) -> Result<(), DiagnosticError> {
    let referenced = manifest
        .segments
        .iter()
        .map(|segment| segment.file_name.as_str())
        .collect::<BTreeSet<_>>();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if parse_segment_name(name).is_some() && !referenced.contains(name) {
            remove_file_if_present(entry.path())?;
        }
    }
    Ok(())
}

fn parse_segment_name(name: &str) -> Option<(u64, &'static str)> {
    let remainder = name.strip_prefix("segment-")?;
    let (digits, extension) = remainder.rsplit_once('.')?;
    if digits.len() != 20 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let extension = match extension {
        "jsonl" => "jsonl",
        "rle" => "rle",
        _ => return None,
    };
    Some((digits.parse().ok()?, extension))
}

fn compact_tombstones(manifest: &mut RotationManifest) {
    let excess = manifest
        .segments
        .iter()
        .filter(|segment| {
            matches!(
                segment.state,
                DiagnosticSegmentState::Expired | DiagnosticSegmentState::Corrupt
            )
        })
        .count()
        .saturating_sub(MAX_TOMBSTONES);
    if excess == 0 {
        return;
    }
    let mut removed = 0;
    manifest.segments.retain(|segment| {
        let terminal = matches!(
            segment.state,
            DiagnosticSegmentState::Expired | DiagnosticSegmentState::Corrupt
        );
        if terminal && removed < excess {
            removed += 1;
            false
        } else {
            true
        }
    });
}

pub(super) fn hash_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tombstone_compaction_keeps_manifest_history_hard_bounded() {
        let mut manifest = RotationManifest {
            schema_version: MANIFEST_VERSION,
            next_segment_id: 300,
            segments: (0..300)
                .map(|segment_id| DiagnosticSegmentMetadata {
                    segment_id,
                    writer_generation: "writer.1".to_owned(),
                    start_sequence: segment_id,
                    end_sequence: Some(segment_id),
                    created_at_epoch_ms: segment_id,
                    closed_at_epoch_ms: Some(segment_id),
                    raw_byte_count: 1,
                    stored_byte_count: 1,
                    content_hash: Some(format!("sha256:{}", "0".repeat(64))),
                    state: DiagnosticSegmentState::Expired,
                    unavailable_reason: Some("age_retention".to_owned()),
                    file_name: segment_file_name(segment_id, "rle"),
                })
                .collect(),
        };
        compact_tombstones(&mut manifest);
        assert_eq!(manifest.segments.len(), MAX_TOMBSTONES);
        assert_eq!(manifest.segments[0].segment_id, 44);
        validate_manifest(&manifest).expect("bounded manifest");
    }
}
