//! Process-neutral portable history port implemented by the repository.

use aworkit_protocol::{
    CheckpointV1, CommitBatchV1, HistoryBackendV1, PortableCanonicalCommitPort,
    PortableCommitReceiptV1, PortablePortErrorV1, PortablePrepareV1, PortablePreparedV1, StableId,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    CanonicalCodec, CommitError, PortableCheckpoint, PortableCommit, PortableEvent,
    PortableRepository, PortableTransitionRecordV1, PreparedCommit, ProjectReference,
    canonical_json, validate_context,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProtocolPreparedClaimV1 {
    request: PortablePrepareV1,
    prepared: PreparedCommit,
}

/// Hash used on the process boundary for canonical semantic values. It is
/// deliberately distinct from repository object IDs, which hash framed bytes.
pub fn protocol_value_hash(domain: &str, value: &Value) -> Result<String, crate::CodecError> {
    let mut hash = Sha256::new();
    hash.update(domain.as_bytes());
    hash.update([0]);
    hash.update(canonical_json(value)?);
    Ok(format!("sha256:{:x}", hash.finalize()))
}

impl PortableCanonicalCommitPort for PortableRepository {
    fn prepare(
        &self,
        request: &PortablePrepareV1,
    ) -> Result<PortablePreparedV1, PortablePortErrorV1> {
        prepare_port(self, request).map_err(port_error)
    }

    fn publish(
        &self,
        prepared: &PortablePreparedV1,
    ) -> Result<PortableCommitReceiptV1, PortablePortErrorV1> {
        publish_port(self, prepared).map_err(port_error)
    }

    fn verify(
        &self,
        receipt: &PortableCommitReceiptV1,
    ) -> Result<PortableCommitReceiptV1, PortablePortErrorV1> {
        verify_port(self, receipt).map_err(port_error)
    }

    fn read_head(
        &self,
        branch_id: &StableId,
    ) -> Result<Option<PortableCommitReceiptV1>, PortablePortErrorV1> {
        let branch = self.read_branch(branch_id.as_str()).map_err(port_error)?;
        let Some(commit_id) = branch.commit_id else {
            return Ok(None);
        };
        let operation_id =
            StableId::parse(commit_id).map_err(|_| port_error(CommitError::VerificationFailed))?;
        let claim = load_claim(self, &operation_id).map_err(port_error)?;
        let receipt = receipt_from_claim(self, &claim).map_err(port_error)?;
        Ok(Some(receipt))
    }
}

fn prepare_port(
    repository: &PortableRepository,
    request: &PortablePrepareV1,
) -> Result<PortablePreparedV1, CommitError> {
    if protocol_value_hash("portable-record-v1", &request.record)? != request.record_hash
        || protocol_value_hash(
            "portable-checkpoint-v1",
            &request.checkpoint.clone().unwrap_or(Value::Null),
        )? != request.checkpoint_hash
    {
        return Err(CommitError::InvalidPreparedClaim);
    }
    let (batch, context) =
        match serde_json::from_value::<PortableTransitionRecordV1>(request.record.clone()) {
            Ok(transition) => {
                validate_context(&transition.context)?;
                (transition.batch, Some(transition.context))
            }
            Err(_) => (
                serde_json::from_value::<CommitBatchV1>(request.record.clone())?,
                None,
            ),
        };
    if batch.chat_id != request.chat_id
        || batch.branch_id != request.branch_id
        || batch.expected_head != request.expected_next_ordinal
        || !matches!(batch.backend, HistoryBackendV1::PortableProject { .. })
        || batch.events.is_empty()
    {
        return Err(CommitError::EventLineage);
    }
    let batch_checkpoint = batch
        .checkpoint
        .as_ref()
        .map(serde_json::to_value)
        .transpose()?;
    if batch_checkpoint != request.checkpoint {
        return Err(CommitError::CheckpointMismatch);
    }
    let current = repository.read_branch(request.branch_id.as_str())?;
    if current.generation != request.expected_generation
        || current.next_ordinal != request.expected_next_ordinal
        || current.head_segment_hash != request.expected_head_hash
    {
        return Err(CommitError::HeadConflict {
            expected: request.expected_generation,
            actual: current.generation,
        });
    }
    let checkpoint = batch
        .checkpoint
        .as_ref()
        .map(|checkpoint| portable_checkpoint(checkpoint, &batch, context.as_ref()));
    let commit = PortableCommit {
        branch_id: request.branch_id.as_str().to_owned(),
        expected_generation: request.expected_generation,
        commit_id: request.operation_id.as_str().to_owned(),
        context,
        events: batch
            .events
            .iter()
            .enumerate()
            .map(|(index, event)| PortableEvent {
                event_id: event.event_id.as_str().to_owned(),
                chat_id: batch.chat_id.as_str().to_owned(),
                branch_id: batch.branch_id.as_str().to_owned(),
                ordinal: request.expected_next_ordinal
                    + u64::try_from(index).expect("bounded event count"),
                kind: event.kind.clone(),
                payload: event.payload.clone(),
            })
            .collect(),
        checkpoint,
    };
    let prepared = repository.prepare(&commit)?;
    if prepared.previous_head_hash != request.expected_head_hash {
        return Err(CommitError::VerificationFailed);
    }
    let claim = ProtocolPreparedClaimV1 {
        request: request.clone(),
        prepared: prepared.clone(),
    };
    let reference = claim_reference(&request.operation_id)?;
    repository
        .paths()
        .publish_relative_immutable(&reference, &CanonicalCodec.encode(&claim)?)?;
    Ok(PortablePreparedV1 {
        operation_id: request.operation_id.clone(),
        commit_id: request.operation_id.clone(),
        object_hash: prepared.segment_hash,
        expected_generation: prepared.expected_generation,
    })
}

fn publish_port(
    repository: &PortableRepository,
    prepared: &PortablePreparedV1,
) -> Result<PortableCommitReceiptV1, CommitError> {
    if prepared.commit_id != prepared.operation_id {
        return Err(CommitError::InvalidPreparedClaim);
    }
    let claim = load_claim(repository, &prepared.operation_id)?;
    if claim.prepared.segment_hash != prepared.object_hash
        || claim.prepared.expected_generation != prepared.expected_generation
        || claim.request.operation_id != prepared.operation_id
    {
        return Err(CommitError::InvalidPreparedClaim);
    }
    let _ = repository.publish(&claim.prepared)?;
    receipt_from_claim(repository, &claim)
}

fn verify_port(
    repository: &PortableRepository,
    receipt: &PortableCommitReceiptV1,
) -> Result<PortableCommitReceiptV1, CommitError> {
    if receipt.commit_id != receipt.operation_id {
        return Err(CommitError::VerificationFailed);
    }
    let claim = load_claim(repository, &receipt.operation_id)?;
    let verified = receipt_from_claim(repository, &claim)?;
    if &verified == receipt {
        Ok(verified)
    } else {
        Err(CommitError::VerificationFailed)
    }
}

fn receipt_from_claim(
    repository: &PortableRepository,
    claim: &ProtocolPreparedClaimV1,
) -> Result<PortableCommitReceiptV1, CommitError> {
    let receipt = repository.verify(&claim.prepared)?;
    Ok(PortableCommitReceiptV1 {
        operation_id: claim.request.operation_id.clone(),
        commit_id: claim.request.operation_id.clone(),
        branch_id: claim.request.branch_id.clone(),
        previous_head_hash: receipt.previous_head_hash,
        published_head_hash: receipt.head_segment_hash,
        generation: receipt.generation,
        checkpoint_hash: claim.request.checkpoint_hash.clone(),
    })
}

fn load_claim(
    repository: &PortableRepository,
    operation_id: &StableId,
) -> Result<ProtocolPreparedClaimV1, CommitError> {
    let reference = claim_reference(operation_id)?;
    let bytes = repository
        .paths()
        .read_relative(&reference, 2 * 1024 * 1024)?;
    let claim: ProtocolPreparedClaimV1 = CanonicalCodec.decode(&bytes)?;
    if claim.request.operation_id != *operation_id
        || claim.prepared.commit_id != operation_id.as_str()
    {
        return Err(CommitError::InvalidPreparedClaim);
    }
    Ok(claim)
}

fn claim_reference(operation_id: &StableId) -> Result<ProjectReference, CommitError> {
    Ok(ProjectReference::parse(format!(
        ".aworkit/portable/prepared/{}.json",
        operation_id.as_str()
    ))?)
}

fn portable_checkpoint(
    checkpoint: &CheckpointV1,
    batch: &CommitBatchV1,
    context: Option<&crate::PortableCommitContextV1>,
) -> PortableCheckpoint {
    PortableCheckpoint {
        last_event_id: batch
            .events
            .last()
            .map(|event| event.event_id.as_str().to_owned()),
        aggregate_version: batch
            .expected_aggregate_version
            .saturating_add(u64::try_from(batch.events.len()).expect("bounded")),
        reducer_version: checkpoint.reducer_version.clone(),
        snapshot_hash: context
            .and_then(|value| value.frozen_snapshot.as_ref())
            .map(|value| value.snapshot_hash.clone()),
        state_hash: checkpoint.state_hash.clone(),
    }
}

fn port_error(error: CommitError) -> PortablePortErrorV1 {
    let uncertain_publication = matches!(error, CommitError::PublicationUncertain);
    let retryable = matches!(
        error,
        CommitError::PublicationUncertain | CommitError::InjectedFault(_) | CommitError::Io(_)
    );
    let code = match &error {
        CommitError::HeadConflict { .. } => "portable_head_conflict",
        CommitError::CommitIdentityConflict => "portable_identity_conflict",
        CommitError::PublicationUncertain => "portable_publication_uncertain",
        CommitError::VerificationFailed | CommitError::InvalidPreparedClaim => {
            "portable_verification_failed"
        }
        CommitError::ExportRejected => "portable_export_rejected",
        _ => "portable_commit_failed",
    };
    PortablePortErrorV1 {
        code: code.to_owned(),
        message: error.to_string(),
        retryable,
        uncertain_publication,
    }
}
