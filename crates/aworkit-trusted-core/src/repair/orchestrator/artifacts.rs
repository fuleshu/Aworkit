//! Exact artifact integrity and readiness gates for repair transitions.

use aworkit_protocol::StableId;

use super::super::*;
use super::{
    error::RepairError,
    service::{RepairOrchestratorV1, port_error},
};

impl RepairOrchestratorV1 {
    pub(super) fn verify_occurrence_artifacts(
        &self,
        operation_id: &StableId,
        occurrence: &ErrorOccurrenceV1,
    ) -> Result<(), RepairError> {
        self.verify_artifacts(
            operation_id,
            RepairArtifactVerificationPurposeV1::ErrorOccurrence,
            occurrence.evidence.clone(),
        )
    }

    pub(super) fn verify_candidate_artifacts(
        &self,
        operation_id: &StableId,
        purpose: RepairArtifactVerificationPurposeV1,
        candidate: &RepairCandidateV1,
    ) -> Result<(), RepairError> {
        let mut artifacts = vec![
            candidate.build_bundle.artifact.clone(),
            candidate.disclosure.rollback_point.artifact.clone(),
        ];
        for disclosure in [
            &candidate.disclosure.source_diff,
            &candidate.disclosure.configuration_diff,
            &candidate.disclosure.tests,
            &candidate.disclosure.benchmarks,
        ] {
            if let RepairEvidenceDisclosureV1::Evidence {
                artifacts: evidence,
                ..
            } = disclosure
            {
                artifacts.extend(evidence.iter().cloned());
            }
        }
        self.verify_artifacts(operation_id, purpose, artifacts)
    }

    pub(super) fn verify_activation_artifacts(
        &self,
        operation_id: &StableId,
        candidate: &RepairCandidateV1,
        report: &PlatformCapabilityReportV1,
    ) -> Result<(), RepairError> {
        self.verify_candidate_artifacts(
            operation_id,
            RepairArtifactVerificationPurposeV1::Activation,
            candidate,
        )?;
        let mut artifacts = vec![report.current_build.artifact.clone()];
        if let Some(previous) = &report.previous_working_build {
            artifacts.push(previous.artifact.clone());
        }
        self.verify_artifacts(
            operation_id,
            RepairArtifactVerificationPurposeV1::Activation,
            artifacts,
        )
    }

    pub(super) fn verify_enrollment_artifacts(
        &self,
        operation_id: &StableId,
        request: &ManagedLocalEnrollmentRequestV1,
    ) -> Result<(), RepairError> {
        self.verify_artifacts(
            operation_id,
            RepairArtifactVerificationPurposeV1::Enrollment,
            vec![request.whole_bundle.artifact.clone()],
        )
    }

    pub(super) fn verify_result_artifacts(
        &self,
        operation_id: &StableId,
        result: &BootstrapResultKindV1,
    ) -> Result<(), RepairError> {
        let artifacts = match result {
            BootstrapResultKindV1::ActivatedVerified {
                focused_verification,
            } => focused_verification
                .results
                .iter()
                .flat_map(|result| result.evidence.iter().cloned())
                .collect(),
            BootstrapResultKindV1::RolledBack {
                rollback_evidence, ..
            } => rollback_evidence.clone(),
            BootstrapResultKindV1::Unsupported { .. }
            | BootstrapResultKindV1::ManualRecoveryRequired { .. } => Vec::new(),
        };
        self.verify_artifacts(
            operation_id,
            RepairArtifactVerificationPurposeV1::FocusedVerification,
            artifacts,
        )
    }

    pub(super) fn verify_focused_evidence_artifacts(
        &self,
        operation_id: &StableId,
        evidence: &FocusedVerificationEvidenceV1,
    ) -> Result<(), RepairError> {
        self.verify_artifacts(
            operation_id,
            RepairArtifactVerificationPurposeV1::FocusedVerification,
            evidence
                .results
                .iter()
                .flat_map(|result| result.evidence.iter().cloned())
                .collect(),
        )
    }

    fn verify_artifacts(
        &self,
        operation_id: &StableId,
        purpose: RepairArtifactVerificationPurposeV1,
        artifacts: Vec<RepairArtifactRefV1>,
    ) -> Result<(), RepairError> {
        for expected in artifacts {
            let readiness = self
                .artifacts
                .verify_ready(RepairArtifactVerificationRequestV1 {
                    operation_id: operation_id.clone(),
                    purpose,
                    artifact: expected.clone(),
                })
                .map_err(|source| port_error("repair artifact exact read", source))?;
            match readiness {
                RepairArtifactReadinessV1::Ready {
                    artifact_id,
                    observed_content_hash,
                    observed_byte_size,
                } if artifact_id == expected.artifact_id
                    && observed_content_hash == expected.content_hash
                    && observed_byte_size == expected.byte_size => {}
                RepairArtifactReadinessV1::Ready { artifact_id, .. } => {
                    return Err(RepairError::ArtifactNotReady {
                        artifact_id,
                        reason: "identity, content hash, or byte size mismatch",
                    });
                }
                RepairArtifactReadinessV1::Missing { artifact_id } => {
                    return Err(RepairError::ArtifactNotReady {
                        artifact_id,
                        reason: "missing",
                    });
                }
                RepairArtifactReadinessV1::Unavailable { artifact_id, .. } => {
                    return Err(RepairError::ArtifactNotReady {
                        artifact_id,
                        reason: "unavailable",
                    });
                }
            }
        }
        Ok(())
    }
}
