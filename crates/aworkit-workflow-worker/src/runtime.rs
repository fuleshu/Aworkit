//! Bounded framed-stdio workflow-worker service.

use std::io::{Read, Write};

use aworkit_protocol::{
    MAX_FRAME_BYTES, StableId, WorkerControlEnvelopeV1, WorkerControlKindV1, WorkerHandshakeV1,
    WorkerHeartbeatV1, WorkerOutputEnvelopeV1, WorkerOutputKindV1, WorkerProposalEnvelopeV1,
    WorkerProposalKindV1, decode_frame, encode_frame,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    gateway::{AdmissionV1, CoreGatewayV1, GatewayError},
    plan::{ExecutionPlanV1, PlanError},
    suspension::{RecoveryErrorV1, RehydratorV1},
};

#[derive(Debug, Error)]
pub enum WorkerRuntimeError {
    #[error("core gateway rejected message: {0}")]
    Gateway(#[from] GatewayError),
    #[error("frozen execution plan is invalid: {0}")]
    Plan(#[from] PlanError),
    #[error("rehydration envelope is invalid: {0}")]
    Recovery(#[from] RecoveryErrorV1),
    #[error("framed protocol error: {0}")]
    Protocol(#[from] aworkit_protocol::ProtocolError),
    #[error("stdio transport error: {0}")]
    Io(#[from] std::io::Error),
    #[error("worker received a control before start or restore")]
    NotStarted,
    #[error("truncated frame on worker stdin")]
    TruncatedFrame,
}

#[derive(Clone, Debug, Default)]
pub struct ServiceResultV1 {
    pub handshake: Option<WorkerHandshakeV1>,
    pub proposals: Vec<WorkerProposalEnvelopeV1>,
    pub heartbeat: Option<WorkerHeartbeatV1>,
    pub shutdown_ack: Option<StableId>,
    pub shutdown: bool,
}

#[derive(Debug, Default)]
pub struct WorkerServiceV1 {
    gateway: CoreGatewayV1,
    plan: Option<ExecutionPlanV1>,
    heartbeat_sequence: u64,
}

impl WorkerServiceV1 {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn handle(
        &mut self,
        envelope: WorkerControlEnvelopeV1,
    ) -> Result<ServiceResultV1, WorkerRuntimeError> {
        // Validate immutable start/restore material before mutating the gateway
        // identity fence so a rejected envelope cannot poison a fresh worker.
        let prepared = match &envelope.control {
            WorkerControlKindV1::Start(snapshot) => Some((
                ExecutionPlanV1::compile(snapshot.clone(), &envelope.snapshot_hash)?,
                None,
            )),
            WorkerControlKindV1::Restore(rehydration) => {
                let restored = RehydratorV1::restore(rehydration.clone())?;
                Some((
                    ExecutionPlanV1::compile(
                        rehydration.snapshot.clone(),
                        &envelope.snapshot_hash,
                    )?,
                    Some(restored.checkpoint.checkpoint_hash),
                ))
            }
            _ => None,
        };
        let admission = self.gateway.admit_control(&envelope)?;
        if admission == AdmissionV1::Duplicate {
            return Ok(ServiceResultV1::default());
        }
        let mut result = ServiceResultV1::default();
        match envelope.control.clone() {
            WorkerControlKindV1::Start(_) => {
                let (plan, _) = prepared.expect("start plan prepared");
                let fingerprint = plan.fingerprint().to_owned();
                result.handshake = Some(self.handshake(&envelope, &fingerprint));
                self.plan = Some(plan);
                result
                    .proposals
                    .push(self.gateway.emit(WorkerProposalKindV1::Ready {
                        plan_fingerprint: fingerprint,
                    })?);
            }
            WorkerControlKindV1::Restore(_) => {
                let (plan, checkpoint_hash) = prepared.expect("restore plan prepared");
                result.handshake = Some(self.handshake(&envelope, plan.fingerprint()));
                self.plan = Some(plan);
                result.proposals.push(self.gateway.emit(
                    WorkerProposalKindV1::RehydrationReady {
                        checkpoint_hash: checkpoint_hash.expect("restore hash prepared"),
                    },
                )?);
            }
            WorkerControlKindV1::Input { input_id, .. } => {
                self.require_plan()?;
                result
                    .proposals
                    .push(self.gateway.emit(WorkerProposalKindV1::Health {
                        facts: json!({"acceptedInputId": input_id}),
                    })?);
            }
            WorkerControlKindV1::Approval {
                approval_id,
                outcome,
            } => {
                self.require_plan()?;
                result
                    .proposals
                    .push(self.gateway.emit(WorkerProposalKindV1::Health {
                        facts: json!({"acceptedApprovalId": approval_id, "outcome": outcome}),
                    })?);
            }
            WorkerControlKindV1::Pause { control_id, scope } => {
                self.require_plan()?;
                result.proposals.push(self.gateway.emit_reserved(
                    WorkerProposalKindV1::Suspension {
                        suspension_id: control_id,
                        state: json!({"state": "paused", "scope": scope}),
                    },
                )?);
            }
            WorkerControlKindV1::Resume { control_id, scope } => {
                self.require_plan()?;
                result.proposals.push(self.gateway.emit_reserved(
                    WorkerProposalKindV1::Health {
                        facts: json!({"resumedControlId": control_id, "scope": scope}),
                    },
                )?);
            }
            WorkerControlKindV1::Cancel { control_id, scope } => {
                self.require_plan()?;
                result.proposals.push(self.gateway.emit_reserved(
                    WorkerProposalKindV1::Terminal {
                        outcome: "cancelled".to_owned(),
                        facts: json!({"controlId": control_id, "scope": scope}),
                    },
                )?);
            }
            WorkerControlKindV1::CapabilityOutcome(outcome) => {
                self.require_plan()?;
                result
                    .proposals
                    .push(self.gateway.emit(WorkerProposalKindV1::Health {
                        facts: json!({
                            "acceptedOutcomeId": outcome.outcome_id,
                            "invocationId": outcome.invocation_id,
                            "class": outcome.class,
                        }),
                    })?);
            }
            WorkerControlKindV1::CommittedAck { .. } => {}
            WorkerControlKindV1::Shutdown { control_id } => {
                result.shutdown_ack = Some(control_id);
                result.shutdown = true;
            }
        }
        self.heartbeat_sequence = self.heartbeat_sequence.saturating_add(1);
        result.heartbeat = Some(WorkerHeartbeatV1 {
            sequence: self.heartbeat_sequence,
            monotonic_time_ms: self.heartbeat_sequence,
            active: !result.shutdown,
            quiescent: result.proposals.is_empty(),
        });
        Ok(result)
    }

    #[must_use]
    pub fn pending_proposals(&self) -> Vec<WorkerProposalEnvelopeV1> {
        self.gateway.retransmit_pending()
    }

    fn require_plan(&self) -> Result<&ExecutionPlanV1, WorkerRuntimeError> {
        self.plan.as_ref().ok_or(WorkerRuntimeError::NotStarted)
    }

    fn handshake(
        &self,
        envelope: &WorkerControlEnvelopeV1,
        plan_fingerprint: &str,
    ) -> WorkerHandshakeV1 {
        WorkerHandshakeV1 {
            protocol_version: 1,
            worker_version: env!("CARGO_PKG_VERSION").to_owned(),
            chat_id: envelope.chat_id.clone(),
            run_id: envelope.run_id.clone(),
            generation: envelope.generation,
            snapshot_hash: envelope.snapshot_hash.clone(),
            plan_fingerprint: plan_fingerprint.to_owned(),
            executable_identity: std::env::current_exe()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| "aworkit-workflow-worker".to_owned()),
        }
    }
}

/// Runs until clean EOF or an explicit shutdown control. Every input and output
/// is one bounded four-byte-length-prefixed JSON frame; stdout contains no logs.
pub fn serve_stdio<R: Read, W: Write>(
    mut input: R,
    mut output: W,
) -> Result<(), WorkerRuntimeError> {
    let mut service = WorkerServiceV1::new();
    loop {
        let Some(frame) = read_one_frame(&mut input)? else {
            return Ok(());
        };
        let control: WorkerControlEnvelopeV1 = decode_frame(&frame)?;
        let generation = control.generation;
        let request_id = control.message_id.clone();
        let response = match service.handle(control) {
            Ok(response) => response,
            Err(error) => {
                let error_output = WorkerOutputEnvelopeV1 {
                    message_id: output_id(&format!("error:{request_id}"))?,
                    generation,
                    output: WorkerOutputKindV1::Error {
                        code: "worker_control_rejected".to_owned(),
                        message: error.to_string().chars().take(4_096).collect(),
                    },
                };
                output.write_all(&encode_frame(&error_output)?)?;
                output.flush()?;
                continue;
            }
        };
        if let Some(handshake) = response.handshake {
            write_output(
                &mut output,
                WorkerOutputEnvelopeV1 {
                    message_id: output_id(&format!("handshake:{request_id}"))?,
                    generation,
                    output: WorkerOutputKindV1::Handshake(handshake),
                },
            )?;
        }
        for proposal in response.proposals {
            write_output(
                &mut output,
                WorkerOutputEnvelopeV1 {
                    message_id: proposal.proposal_id.clone(),
                    generation,
                    output: WorkerOutputKindV1::Proposal(proposal),
                },
            )?;
        }
        if let Some(heartbeat) = response.heartbeat {
            write_output(
                &mut output,
                WorkerOutputEnvelopeV1 {
                    message_id: output_id(&format!(
                        "heartbeat:{generation:?}:{}",
                        heartbeat.sequence
                    ))?,
                    generation,
                    output: WorkerOutputKindV1::Heartbeat(heartbeat),
                },
            )?;
        }
        if let Some(control_id) = response.shutdown_ack {
            write_output(
                &mut output,
                WorkerOutputEnvelopeV1 {
                    message_id: output_id(&format!("shutdown:{control_id}"))?,
                    generation,
                    output: WorkerOutputKindV1::ShutdownAck { control_id },
                },
            )?;
        }
        output.flush()?;
        if response.shutdown {
            return Ok(());
        }
    }
}

fn write_output<W: Write>(
    output: &mut W,
    envelope: WorkerOutputEnvelopeV1,
) -> Result<(), WorkerRuntimeError> {
    output.write_all(&encode_frame(&envelope)?)?;
    Ok(())
}

fn output_id(material: &str) -> Result<StableId, WorkerRuntimeError> {
    let digest = format!("{:x}", Sha256::digest(material.as_bytes()));
    StableId::parse(format!("worker.output.{}", &digest[..48]))
        .map_err(|_| GatewayError::InvalidIdentifier.into())
}

fn read_one_frame<R: Read>(input: &mut R) -> Result<Option<Vec<u8>>, WorkerRuntimeError> {
    let mut prefix = [0_u8; 4];
    let mut read = 0;
    while read < prefix.len() {
        let count = input.read(&mut prefix[read..])?;
        if count == 0 {
            return if read == 0 {
                Ok(None)
            } else {
                Err(WorkerRuntimeError::TruncatedFrame)
            };
        }
        read += count;
    }
    let body_len = u32::from_be_bytes(prefix) as usize;
    if body_len > MAX_FRAME_BYTES {
        return Err(aworkit_protocol::ProtocolError::FrameTooLarge.into());
    }
    let mut frame = Vec::with_capacity(4 + body_len);
    frame.extend_from_slice(&prefix);
    frame.resize(4 + body_len, 0);
    input.read_exact(&mut frame[4..]).map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            WorkerRuntimeError::TruncatedFrame
        } else {
            WorkerRuntimeError::Io(error)
        }
    })?;
    Ok(Some(frame))
}
