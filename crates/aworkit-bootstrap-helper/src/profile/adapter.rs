//! Deterministic managed-local origin, enrollment, and capability decisions.

use std::sync::{Arc, Mutex};

use aworkit_protocol::StableId;
use aworkit_trusted_core::{
    ActivationEligibilityV1, BuildBundleRefV1, BuildOriginV1, BuildProvenanceV1,
    ManagedLocalEnrollmentRequestV1, PlatformCapabilityReportV1, PlatformReasonV1,
    RepairActivationBatonV1,
};

use crate::journal::canonical_hash;
use crate::protocol::{
    BootstrapPreflightPortV1, EnrollmentPlanV1, LocalBuildEnrollmentStateV1, LocalBuildEnrollmentV1,
};
use crate::slots::SlotDataCompatibilityV1;

use super::error::ProfileError;
use super::model::{ManagedLocalLayoutV1, ProfileDecisionV1, ProfileRuntimeObservationsV1};
use super::ports::ProfileObservationPortV1;

#[derive(Clone)]
struct CachedQuery {
    provenance: BuildProvenanceV1,
    enrollment: LocalBuildEnrollmentV1,
    candidate: BuildBundleRefV1,
    previous: Option<BuildBundleRefV1>,
}

/// Fixed v1 profile. It has no packaged selector, force path, or elevation path.
pub struct ManagedLocalBuildProfileAdapter {
    observations: Arc<dyn ProfileObservationPortV1>,
    layout: ManagedLocalLayoutV1,
    cached_query: Mutex<Option<CachedQuery>>,
}

impl ManagedLocalBuildProfileAdapter {
    #[must_use]
    pub fn new(
        observations: Arc<dyn ProfileObservationPortV1>,
        layout: ManagedLocalLayoutV1,
    ) -> Self {
        Self {
            observations,
            layout,
            cached_query: Mutex::new(None),
        }
    }

    fn reason(code: &str, message: &str, next_step: &str) -> PlatformReasonV1 {
        PlatformReasonV1 {
            code: code.to_owned(),
            message: message.to_owned(),
            next_steps: vec![next_step.to_owned()],
        }
    }

    fn unsupported(
        origin: BuildOriginV1,
        enrollment: LocalBuildEnrollmentStateV1,
        eligibility: ActivationEligibilityV1,
        code: &str,
        message: &str,
    ) -> ProfileDecisionV1 {
        ProfileDecisionV1 {
            origin,
            enrollment,
            eligibility,
            reason: Self::reason(code, message, "export the candidate and update manually"),
        }
    }

    fn decide(
        &self,
        provenance: &BuildProvenanceV1,
        enrollment: &LocalBuildEnrollmentV1,
        previous: Option<&BuildBundleRefV1>,
        observed: &ProfileRuntimeObservationsV1,
    ) -> ProfileDecisionV1 {
        let origin = observed.detected_origin.clone();
        if observed.embedded_provenance_digest != provenance.provenance_hash {
            return Self::unsupported(
                BuildOriginV1::Mismatched {
                    detail: "embedded provenance digest differs".to_owned(),
                },
                enrollment.state,
                ActivationEligibilityV1::UnknownOrigin,
                "origin_unverifiable",
                "embedded build provenance does not match",
            );
        }
        match &origin {
            BuildOriginV1::PackagedDistribution { .. } => {
                return Self::unsupported(
                    origin,
                    enrollment.state,
                    ActivationEligibilityV1::PackagedDistribution,
                    "packaged_distribution",
                    "packaged installations cannot self-activate in v1",
                );
            }
            BuildOriginV1::Unknown | BuildOriginV1::Conflicting { .. } => {
                return Self::unsupported(
                    origin,
                    enrollment.state,
                    ActivationEligibilityV1::UnknownOrigin,
                    "origin_unverifiable",
                    "build origin cannot be verified",
                );
            }
            BuildOriginV1::Mismatched { .. } => {
                return Self::unsupported(
                    origin,
                    enrollment.state,
                    ActivationEligibilityV1::MismatchedEnrollment,
                    "enrollment_mismatch",
                    "build origin conflicts with managed enrollment",
                );
            }
            BuildOriginV1::SourceCheckout {
                projected_provenance_hash,
            } if enrollment.state == LocalBuildEnrollmentStateV1::NotEnrolled => {
                if projected_provenance_hash != &provenance.provenance_hash {
                    return Self::unsupported(
                        origin,
                        enrollment.state,
                        ActivationEligibilityV1::UnknownOrigin,
                        "origin_unverifiable",
                        "source-checkout provenance does not match the embedded build",
                    );
                }
                return ProfileDecisionV1 {
                    origin,
                    enrollment: enrollment.state,
                    eligibility: ActivationEligibilityV1::EnrollmentRequired,
                    reason: Self::reason(
                        "enrollment_required",
                        "local source build must be enrolled into a managed root",
                        "request explicit managed-local enrollment",
                    ),
                };
            }
            BuildOriginV1::ManagedLocal { .. }
                if enrollment.state == LocalBuildEnrollmentStateV1::Enrolled => {}
            _ => {
                return Self::unsupported(
                    origin,
                    enrollment.state,
                    ActivationEligibilityV1::MismatchedEnrollment,
                    "enrollment_mismatch",
                    "build origin and enrollment state do not form a managed-local installation",
                );
            }
        }

        if let BuildOriginV1::ManagedLocal {
            enrollment_digest,
            active_slot_hash,
        } = &origin
        {
            if enrollment_digest != &enrollment.enrollment_digest
                || active_slot_hash != &enrollment.active_slot_hash
            {
                return Self::unsupported(
                    origin,
                    enrollment.state,
                    ActivationEligibilityV1::MismatchedEnrollment,
                    "enrollment_mismatch",
                    "managed-local origin does not match the enrollment record",
                );
            }
        }

        let failure = if enrollment.installation_id != self.layout.installation_id
            || !observed.installation_identity_matches
        {
            Some(("enrollment_mismatch", "installation identity changed"))
        } else if enrollment.helper_identity_hash != self.layout.helper_identity_hash
            || !observed.helper_identity_matches
            || enrollment.launcher_identity_hash != self.layout.launcher_identity_hash
            || !observed.launcher_identity_matches
            || enrollment.journal_identity_hash != self.layout.journal_identity_hash
            || !observed.journal_identity_matches
            || enrollment.selector_identity_hash != self.layout.selector_identity_hash
            || !observed.selector_identity_matches
        {
            Some(("ownership_lost", "helper-controlled identity changed"))
        } else if enrollment.active_slot_hash != observed.active_selector_hash
            || enrollment.current_bundle_hash != observed.current_build.artifact.content_hash
        {
            Some((
                "active_slot_mismatch",
                "active selector or bundle differs from enrollment",
            ))
        } else if !observed.candidate_slot_verified
            || previous.is_none()
            || !observed.previous_slot_verified
        {
            Some((
                "slot_unverified",
                "candidate or previous-known-good slot is unavailable",
            ))
        } else if !enrollment.ownership.per_user_owned
            || !observed.per_user_owned
            || !observed.writable_without_elevation
        {
            Some((
                "ownership_lost",
                "managed root is not writable by its desktop user",
            ))
        } else if !enrollment.ownership.same_volume || !observed.same_local_durable_volume {
            Some((
                "unsupported_volume",
                "managed slots are not on one durable local volume",
            ))
        } else if !enrollment.ownership.selector_atomic || !observed.atomic_selector_supported {
            Some((
                "unsupported_selector",
                "atomic selector replacement is unavailable",
            ))
        } else if !enrollment.ownership.helper_survives_outside_slots
            || !observed.helper_survives_outside_slots
        {
            Some((
                "helper_not_stable",
                "helper or launcher is inside the swapped bundle",
            ))
        } else if !observed.complete_process_tree_cleanup {
            Some((
                "process_cleanup_unavailable",
                "complete process-tree cleanup is unavailable",
            ))
        } else if !observed.verification_only_launch {
            Some((
                "verification_launch_unavailable",
                "verification-only launch is unavailable",
            ))
        } else if observed.data_compatibility
            == SlotDataCompatibilityV1::ForwardOnlyMigrationRequired
        {
            Some((
                "forward_only_data",
                "candidate requires a forward-only data migration",
            ))
        } else if observed.capability_generation == 0
            || observed.valid_from_epoch_ms >= observed.expires_at_epoch_ms
        {
            Some((
                "capability_generation_invalid",
                "capability generation is not valid",
            ))
        } else {
            None
        };
        if let Some((code, message)) = failure {
            return Self::unsupported(
                origin,
                enrollment.state,
                ActivationEligibilityV1::Unsupported,
                code,
                message,
            );
        }
        ProfileDecisionV1 {
            origin,
            enrollment: enrollment.state,
            eligibility: ActivationEligibilityV1::SupportedManagedLocal,
            reason: Self::reason(
                "supported_managed_local",
                "all managed-local v1 guarantees are present",
                "activation may be explicitly approved",
            ),
        }
    }

    fn build_report(
        &self,
        provenance: &BuildProvenanceV1,
        enrollment: &LocalBuildEnrollmentV1,
        candidate: &BuildBundleRefV1,
        previous: Option<&BuildBundleRefV1>,
    ) -> Result<PlatformCapabilityReportV1, ProfileError> {
        let observed = self.observations.observe()?;
        let decision = self.decide(provenance, enrollment, previous, &observed);
        let report_seed = canonical_hash(&(
            &observed.candidate_id,
            observed.candidate_version,
            &observed.candidate_build_content_hash,
            &observed.capability_generation,
            &decision,
            &candidate.artifact.artifact_id,
        ))
        .map_err(|_| ProfileError::Invalid("capability report seed"))?;
        let report_id = StableId::parse(format!("capability.{}", &report_seed[7..39]))
            .map_err(|_| ProfileError::Invalid("capability report id"))?;
        let mut report = PlatformCapabilityReportV1 {
            schema_version: 1,
            report_id,
            candidate_id: observed.candidate_id,
            candidate_version: observed.candidate_version,
            candidate_hash: observed.candidate_build_content_hash,
            capability_generation: observed.capability_generation,
            build_origin: decision.origin,
            eligibility: decision.eligibility,
            reason: decision.reason,
            current_build: observed.current_build,
            previous_working_build: previous.cloned(),
            valid_from_epoch_ms: observed.valid_from_epoch_ms,
            expires_at_epoch_ms: observed.expires_at_epoch_ms,
            capability_digest: String::new(),
        };
        report.capability_digest = canonical_hash(&report)
            .map_err(|_| ProfileError::Invalid("capability report digest"))?;
        Ok(report)
    }
}

impl BootstrapPreflightPortV1 for ManagedLocalBuildProfileAdapter {
    fn capability_report(
        &self,
        provenance: &BuildProvenanceV1,
        enrollment: &LocalBuildEnrollmentV1,
        candidate: &BuildBundleRefV1,
        previous: Option<&BuildBundleRefV1>,
    ) -> Result<PlatformCapabilityReportV1, String> {
        let report = self
            .build_report(provenance, enrollment, candidate, previous)
            .map_err(|error| error.to_string())?;
        *self.cached_query.lock().expect("profile cache lock") = Some(CachedQuery {
            provenance: provenance.clone(),
            enrollment: enrollment.clone(),
            candidate: candidate.clone(),
            previous: previous.cloned(),
        });
        Ok(report)
    }

    fn revalidate_baton_binding(
        &self,
        baton: &RepairActivationBatonV1,
    ) -> Result<PlatformCapabilityReportV1, String> {
        let cached = self
            .cached_query
            .lock()
            .expect("profile cache lock")
            .clone()
            .ok_or_else(|| ProfileError::CapabilityDrift.to_string())?;
        if cached.candidate != baton.candidate_bundle
            || cached.previous.as_ref() != Some(&baton.previous_working_build)
            || cached.provenance.provenance_hash != baton.provenance_hash
            || cached.enrollment.enrollment_digest != baton.enrollment_digest
        {
            return Err(ProfileError::CapabilityDrift.to_string());
        }
        self.build_report(
            &cached.provenance,
            &cached.enrollment,
            &cached.candidate,
            cached.previous.as_ref(),
        )
        .map_err(|error| error.to_string())
    }

    fn enrollment_plan(
        &self,
        request: &ManagedLocalEnrollmentRequestV1,
    ) -> Result<EnrollmentPlanV1, String> {
        let observed = self
            .observations
            .observe()
            .map_err(|error| error.to_string())?;
        if !matches!(
            observed.detected_origin,
            BuildOriginV1::SourceCheckout { .. }
        ) || observed.embedded_provenance_digest != request.projected_provenance_hash
        {
            return Err(ProfileError::Unsupported(
                "enrollment requires matching local source provenance",
            )
            .to_string());
        }
        let plan_id_hash = canonical_hash(&(
            &request.request_id,
            &request.projected_provenance_hash,
            &request.whole_bundle.artifact.artifact_id,
            &request.whole_bundle.artifact.content_hash,
            &self.layout.installation_id,
        ))
        .map_err(|_| ProfileError::Invalid("enrollment plan id").to_string())?;
        let plan_id = StableId::parse(format!("enrollment.plan.{}", &plan_id_hash[7..39]))
            .map_err(|_| ProfileError::Invalid("enrollment plan id").to_string())?;
        let mut plan = EnrollmentPlanV1 {
            plan_id,
            installation_id: self.layout.installation_id.clone(),
            profile_version: 1,
            helper_root_identity_hash: self.layout.helper_root_identity_hash.clone(),
            initial_active_slot_root_hash: self.layout.initial_active_slot_root_hash.clone(),
            selector_identity_hash: self.layout.selector_identity_hash.clone(),
            journal_identity_hash: self.layout.journal_identity_hash.clone(),
            plan_hash: String::new(),
        };
        plan.plan_hash = canonical_hash(&plan)
            .map_err(|_| ProfileError::Invalid("enrollment plan hash").to_string())?;
        Ok(plan)
    }
}
