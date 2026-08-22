//! RFC 8785 canonical JSON and strict LF-framed immutable segments.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use aworkit_protocol::CommitBatchV1;

use crate::{ArtifactDescriptor, ExportPolicy, MAX_PORTABLE_ARTIFACT_BYTES, OmissionFact};

pub const MAX_RECORD_BYTES: usize = 1024 * 1024;
pub const MAX_SEGMENT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_SEGMENT_EVENTS: usize = 64;

/// One sanitized semantic event in a portable branch.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortableEvent {
    pub event_id: String,
    pub chat_id: String,
    pub branch_id: String,
    pub ordinal: u64,
    pub kind: String,
    pub payload: Value,
}

/// Logical view of a bounded immutable segment. Its byte representation is a
/// header record, one event record per event, and a trailer record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortableSegment {
    pub parent_segment_hash: Option<String>,
    pub base_checkpoint_hash: Option<String>,
    pub first_ordinal: u64,
    pub context: Option<PortableCommitContextV1>,
    pub events: Vec<PortableEvent>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortableCapabilityRequirementV1 {
    pub logical_id: String,
    pub version: String,
    pub configuration_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortableGitFactsV1 {
    pub head_commit: Option<String>,
    pub dirty_state_digest: Option<String>,
    pub worktree_identity: Option<String>,
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortableFrozenSnapshotV1 {
    pub schema_version: u16,
    pub snapshot_hash: String,
    pub workflow_hash: String,
    pub portable_snapshot: Value,
    pub requirements: Vec<PortableCapabilityRequirementV1>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortableProvenanceV1 {
    pub git: PortableGitFactsV1,
    pub workflow_revision_hash: String,
    pub configuration_revision_hashes: Vec<String>,
    pub redaction_profile_hash: String,
    pub artifact_refs: Vec<String>,
    #[serde(default)]
    pub artifact_metadata: Vec<ArtifactDescriptor>,
    pub omissions: Vec<OmissionFact>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortableCommitContextV1 {
    pub frozen_snapshot: Option<PortableFrozenSnapshotV1>,
    pub provenance: PortableProvenanceV1,
}

/// Rich process-neutral semantic record accepted by the portable port.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortableTransitionRecordV1 {
    pub batch: CommitBatchV1,
    pub context: PortableCommitContextV1,
}

/// A logical reducer checkpoint that never contains native runtime state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortableCheckpoint {
    pub last_event_id: Option<String>,
    pub aggregate_version: u64,
    pub reducer_version: String,
    pub snapshot_hash: Option<String>,
    pub state_hash: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SegmentHeaderV1 {
    record_type: SegmentRecordType,
    schema_version: u16,
    parent_segment_hash: Option<String>,
    base_checkpoint_hash: Option<String>,
    first_ordinal: u64,
    event_count: u16,
    context: Option<PortableCommitContextV1>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SegmentEventV1 {
    record_type: SegmentRecordType,
    event: PortableEvent,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SegmentTrailerV1 {
    record_type: SegmentRecordType,
    event_count: u16,
    last_ordinal: u64,
    body_hash: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SegmentRecordType {
    SegmentHeader,
    Event,
    SegmentTrailer,
}

/// Deterministic portable record encoder.
#[derive(Clone, Copy, Debug, Default)]
pub struct CanonicalCodec;

impl CanonicalCodec {
    /// Serializes one record as RFC 8785 UTF-8 JSON followed by exactly one LF.
    pub fn encode<T: Serialize>(&self, value: &T) -> Result<Vec<u8>, CodecError> {
        let mut line = canonical_json(&serde_json::to_value(value)?)?;
        if line.len() > MAX_RECORD_BYTES {
            return Err(CodecError::RecordTooLarge);
        }
        line.push(b'\n');
        Ok(line)
    }

    /// Decodes exactly one canonical record. CRLF, BOM, blank lines, trailing
    /// whitespace, duplicate data, and noncanonical numbers are rejected by
    /// byte-for-byte re-encoding.
    pub fn decode<T: DeserializeOwned>(&self, bytes: &[u8]) -> Result<T, CodecError> {
        if bytes.is_empty()
            || bytes.len() > MAX_RECORD_BYTES + 1
            || !bytes.ends_with(b"\n")
            || bytes[..bytes.len() - 1].contains(&b'\n')
            || bytes.contains(&b'\r')
        {
            return Err(CodecError::InvalidFraming);
        }
        let value: Value = serde_json::from_slice(&bytes[..bytes.len() - 1])?;
        if self.encode(&value)? != bytes {
            return Err(CodecError::NonCanonical);
        }
        Ok(serde_json::from_value(value)?)
    }

    /// Encodes the documented header/events/trailer JSONL grammar.
    pub fn encode_segment(&self, segment: &PortableSegment) -> Result<Vec<u8>, CodecError> {
        validate_segment(segment)?;
        let event_count =
            u16::try_from(segment.events.len()).map_err(|_| CodecError::TooManyEvents)?;
        let header = SegmentHeaderV1 {
            record_type: SegmentRecordType::SegmentHeader,
            schema_version: 1,
            parent_segment_hash: segment.parent_segment_hash.clone(),
            base_checkpoint_hash: segment.base_checkpoint_hash.clone(),
            first_ordinal: segment.first_ordinal,
            event_count,
            context: segment.context.clone(),
        };
        let mut body = self.encode(&header)?;
        for event in &segment.events {
            body.extend(self.encode(&SegmentEventV1 {
                record_type: SegmentRecordType::Event,
                event: event.clone(),
            })?);
        }
        let last_ordinal = segment
            .first_ordinal
            .checked_add(u64::try_from(segment.events.len() - 1).expect("bounded"))
            .ok_or(CodecError::OrdinalGap)?;
        let trailer = SegmentTrailerV1 {
            record_type: SegmentRecordType::SegmentTrailer,
            event_count,
            last_ordinal,
            body_hash: digest("segment-body", &body),
        };
        body.extend(self.encode(&trailer)?);
        if body.len() > MAX_SEGMENT_BYTES {
            return Err(CodecError::SegmentTooLarge);
        }
        Ok(body)
    }

    /// Validates every record and the body digest before yielding a logical
    /// segment. No prefix or partially valid page is ever accepted.
    pub fn decode_segment(&self, bytes: &[u8]) -> Result<PortableSegment, CodecError> {
        if bytes.is_empty()
            || bytes.len() > MAX_SEGMENT_BYTES
            || !bytes.ends_with(b"\n")
            || bytes.contains(&b'\r')
            || bytes.starts_with(&[0xef, 0xbb, 0xbf])
        {
            return Err(CodecError::InvalidFraming);
        }
        let lines: Vec<&[u8]> = bytes.split_inclusive(|byte| *byte == b'\n').collect();
        if lines.len() < 3 || lines.len() > MAX_SEGMENT_EVENTS + 2 {
            return Err(CodecError::InvalidSegmentGrammar);
        }
        let header: SegmentHeaderV1 = self.decode(lines[0])?;
        if header.record_type != SegmentRecordType::SegmentHeader || header.schema_version != 1 {
            return Err(CodecError::UnsupportedSchema);
        }
        let expected_count = usize::from(header.event_count);
        if expected_count == 0
            || expected_count > MAX_SEGMENT_EVENTS
            || lines.len() != expected_count + 2
        {
            return Err(CodecError::InvalidSegmentGrammar);
        }
        let mut events = Vec::with_capacity(expected_count);
        for line in &lines[1..=expected_count] {
            let record: SegmentEventV1 = self.decode(line)?;
            if record.record_type != SegmentRecordType::Event {
                return Err(CodecError::InvalidSegmentGrammar);
            }
            events.push(record.event);
        }
        let trailer: SegmentTrailerV1 = self.decode(lines[expected_count + 1])?;
        let body_length = lines[..=expected_count]
            .iter()
            .map(|line| line.len())
            .sum::<usize>();
        let expected_body_hash = digest("segment-body", &bytes[..body_length]);
        if trailer.record_type != SegmentRecordType::SegmentTrailer
            || usize::from(trailer.event_count) != expected_count
            || trailer.body_hash != expected_body_hash
        {
            return Err(CodecError::TrailerMismatch);
        }
        let segment = PortableSegment {
            parent_segment_hash: header.parent_segment_hash,
            base_checkpoint_hash: header.base_checkpoint_hash,
            first_ordinal: header.first_ordinal,
            context: header.context,
            events,
        };
        validate_segment(&segment)?;
        let last_ordinal = segment
            .first_ordinal
            .checked_add(u64::try_from(segment.events.len() - 1).expect("bounded"))
            .ok_or(CodecError::OrdinalGap)?;
        if trailer.last_ordinal != last_ordinal {
            return Err(CodecError::TrailerMismatch);
        }
        Ok(segment)
    }
}

fn validate_segment(segment: &PortableSegment) -> Result<(), CodecError> {
    if segment.events.is_empty() {
        return Err(CodecError::EmptySegment);
    }
    if segment.events.len() > MAX_SEGMENT_EVENTS {
        return Err(CodecError::TooManyEvents);
    }
    validate_hash_option(segment.parent_segment_hash.as_deref())?;
    validate_hash_option(segment.base_checkpoint_hash.as_deref())?;
    if let Some(context) = &segment.context {
        validate_context(context)?;
    }
    let first = &segment.events[0];
    validate_id(&first.chat_id)?;
    validate_id(&first.branch_id)?;
    for (index, event) in segment.events.iter().enumerate() {
        validate_id(&event.event_id)?;
        validate_id(&event.chat_id)?;
        validate_id(&event.branch_id)?;
        validate_kind(&event.kind)?;
        if event.chat_id != first.chat_id || event.branch_id != first.branch_id {
            return Err(CodecError::MixedLineage);
        }
        let expected = segment
            .first_ordinal
            .checked_add(u64::try_from(index).expect("bounded"))
            .ok_or(CodecError::OrdinalGap)?;
        if event.ordinal != expected {
            return Err(CodecError::OrdinalGap);
        }
        if canonical_json(&event.payload)?.len() > MAX_RECORD_BYTES / 2 {
            return Err(CodecError::RecordTooLarge);
        }
    }
    Ok(())
}

pub fn validate_context(context: &PortableCommitContextV1) -> Result<(), CodecError> {
    if let Some(snapshot) = &context.frozen_snapshot {
        let scrubbed = ExportPolicy
            .scrub(&snapshot.portable_snapshot)
            .map_err(|_| CodecError::InvalidProvenance)?;
        if snapshot.schema_version != 1
            || scrubbed.value != snapshot.portable_snapshot
            || !scrubbed.omissions.is_empty()
            || snapshot.snapshot_hash != portable_snapshot_hash(&snapshot.portable_snapshot)?
            || !valid_hash(&snapshot.workflow_hash)
            || snapshot.requirements.windows(2).any(|pair| {
                pair[0].logical_id >= pair[1].logical_id
                    || !valid_name(&pair[0].logical_id)
                    || !valid_name(&pair[0].version)
                    || !valid_hash(&pair[0].configuration_hash)
            })
            || snapshot.requirements.last().is_some_and(|value| {
                !valid_name(&value.logical_id)
                    || !valid_name(&value.version)
                    || !valid_hash(&value.configuration_hash)
            })
        {
            return Err(CodecError::InvalidProvenance);
        }
    }
    let provenance = &context.provenance;
    if let Some(reason) = &provenance.git.unavailable_reason {
        let scrubbed = ExportPolicy
            .scrub(&Value::String(reason.clone()))
            .map_err(|_| CodecError::InvalidProvenance)?;
        if scrubbed.value != Value::String(reason.clone()) || !scrubbed.omissions.is_empty() {
            return Err(CodecError::InvalidProvenance);
        }
    }
    if !valid_hash(&provenance.workflow_revision_hash)
        || !valid_hash(&provenance.redaction_profile_hash)
        || provenance
            .configuration_revision_hashes
            .iter()
            .chain(provenance.artifact_refs.iter())
            .any(|value| !valid_hash(value))
        || provenance
            .configuration_revision_hashes
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        || provenance
            .artifact_refs
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        || provenance
            .artifact_metadata
            .windows(2)
            .any(|pair| pair[0].digest >= pair[1].digest)
        || provenance.artifact_metadata.iter().any(|descriptor| {
            !matches!(
                descriptor.media_type.as_str(),
                "text/plain" | "text/markdown" | "text/csv" | "application/json"
            ) || descriptor.byte_length == 0
                || descriptor.byte_length > MAX_PORTABLE_ARTIFACT_BYTES as u64
                || !valid_hash(&descriptor.digest)
        })
        || provenance.artifact_refs
            != provenance
                .artifact_metadata
                .iter()
                .map(|descriptor| descriptor.digest.clone())
                .collect::<Vec<_>>()
        || provenance
            .omissions
            .iter()
            .any(|fact| !fact.pointer.starts_with('/') || fact.reason != "local_detailed_capture")
        || provenance
            .omissions
            .windows(2)
            .any(|pair| pair[0].pointer >= pair[1].pointer)
        || provenance
            .git
            .head_commit
            .as_deref()
            .is_some_and(|value| !valid_git_object_id(value))
        || provenance
            .git
            .dirty_state_digest
            .as_deref()
            .is_some_and(|value| !valid_hash(value))
        || provenance
            .git
            .worktree_identity
            .as_deref()
            .is_some_and(|value| !valid_hash(value))
        || provenance.git.head_commit.is_none()
            && provenance
                .git
                .unavailable_reason
                .as_deref()
                .is_none_or(str::is_empty)
    {
        return Err(CodecError::InvalidProvenance);
    }
    Ok(())
}

/// Exact identity of the scrubbed portable snapshot bytes carried in a
/// segment context.
pub fn portable_snapshot_hash(value: &Value) -> Result<String, CodecError> {
    Ok(digest(
        "portable-frozen-snapshot-v1",
        &canonical_json(value)?,
    ))
}

pub fn validate_checkpoint_record(checkpoint: &PortableCheckpoint) -> Result<(), CodecError> {
    if checkpoint.aggregate_version == 0
        || !valid_name(&checkpoint.reducer_version)
        || !valid_hash(&checkpoint.state_hash)
        || checkpoint
            .snapshot_hash
            .as_deref()
            .is_some_and(|value| !valid_hash(value))
        || checkpoint
            .last_event_id
            .as_deref()
            .is_none_or(|value| !valid_name(value))
    {
        Err(CodecError::InvalidCheckpoint)
    } else {
        Ok(())
    }
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn valid_git_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn validate_id(value: &str) -> Result<(), CodecError> {
    if !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        Ok(())
    } else {
        Err(CodecError::InvalidIdentity)
    }
}

fn validate_kind(value: &str) -> Result<(), CodecError> {
    let normalized: String = value
        .bytes()
        .filter(|byte| byte.is_ascii_alphanumeric())
        .map(char::from)
        .collect();
    let forbidden = normalized.starts_with("authority")
        || normalized.starts_with("approval")
        || normalized.starts_with("secretlease")
        || matches!(
            normalized.as_str(),
            "reasoningraw"
                | "rawreasoning"
                | "hiddenreasoning"
                | "chainofthought"
                | "debugcapture"
                | "rawprotocol"
                | "rawstream"
                | "executable"
                | "scriptbytes"
                | "pluginbytes"
        );
    if !forbidden
        && !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || matches!(byte, b'.' | b'-' | b'_'))
    {
        Ok(())
    } else {
        Err(CodecError::InvalidEventKind)
    }
}

fn validate_hash_option(value: Option<&str>) -> Result<(), CodecError> {
    if value.is_none_or(valid_hash) {
        Ok(())
    } else {
        Err(CodecError::InvalidIdentity)
    }
}

fn valid_hash(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

/// Produces RFC 8785 JCS bytes, including UTF-16 object-key ordering and
/// ECMAScript-compatible finite-number formatting.
pub fn canonical_json(value: &Value) -> Result<Vec<u8>, CodecError> {
    Ok(serde_jcs::to_vec(value)?)
}

/// Returns a domain-separated, explicit SHA-256 content identity.
#[must_use]
pub fn digest(domain: &str, bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(domain.as_bytes());
    hash.update([0]);
    hash.update(bytes);
    format!("sha256:{:x}", hash.finalize())
}

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("portable records must be exactly LF-terminated canonical JSON")]
    InvalidFraming,
    #[error("portable record is not RFC 8785 canonical JSON")]
    NonCanonical,
    #[error("portable record exceeds its bounded size")]
    RecordTooLarge,
    #[error("portable segment exceeds its bounded size")]
    SegmentTooLarge,
    #[error("portable segment must contain one to 64 events")]
    EmptySegment,
    #[error("portable segment exceeds 64 events")]
    TooManyEvents,
    #[error("portable event ordinals must be contiguous")]
    OrdinalGap,
    #[error("portable segment mixes chat or branch lineage")]
    MixedLineage,
    #[error("portable identifier or hash is malformed")]
    InvalidIdentity,
    #[error("portable event kind is malformed")]
    InvalidEventKind,
    #[error("portable segment JSONL grammar is invalid")]
    InvalidSegmentGrammar,
    #[error("portable segment schema is unsupported")]
    UnsupportedSchema,
    #[error("portable segment trailer does not authenticate its body")]
    TrailerMismatch,
    #[error("portable frozen snapshot or provenance is malformed")]
    InvalidProvenance,
    #[error("portable reducer checkpoint is malformed")]
    InvalidCheckpoint,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
