/**
 * Aworkit-owned Management repair contracts. Widget and framework types never
 * cross this boundary; every privileged operation remains a versioned command
 * for the trusted core.
 */

export type RepairEvidenceStatus =
  | "passed"
  | "failed"
  | "uncertain"
  | "unloaded"
  | "unavailable"
  | "corrupt";

export interface ErrorGroupV1 {
  readonly id: string;
  readonly fingerprint: string;
  readonly title: string;
  readonly occurrenceCount: number;
  readonly chatCount: number;
  readonly firstSeenAt: string;
  readonly lastSeenAt: string;
  readonly lastRepairAt: string | null;
  readonly state:
    | "open"
    | "investigating"
    | "candidate_ready"
    | "verified"
    | "regression";
  readonly evidenceIds: readonly string[];
}

export interface InvestigationStepV1 {
  readonly id: string;
  readonly label: string;
  readonly state: "pending" | "active" | "completed" | "failed";
}

export interface RepairInvestigationV1 {
  readonly id: string;
  readonly errorGroupId: string;
  readonly state: "running" | "awaiting_review" | "cancelled" | "completed";
  readonly boundedBy: string;
  readonly startedAt: string;
  readonly steps: readonly InvestigationStepV1[];
}

export interface RepairDiffLineV1 {
  readonly id: string;
  readonly oldLine: number | null;
  readonly newLine: number | null;
  readonly kind: "context" | "added" | "removed";
  readonly content: string;
}

export interface RepairSourceDiffV1 {
  readonly id: string;
  readonly path: string;
  readonly language: string;
  readonly linesChanged: number;
  readonly lines: readonly RepairDiffLineV1[];
}

export interface RepairConfigurationDiffV1 {
  readonly id: string;
  readonly key: string;
  readonly before: string | null;
  readonly after: string | null;
  readonly consequence: string;
}

export interface RepairTestEvidenceV1 {
  readonly id: string;
  readonly label: string;
  readonly status: RepairEvidenceStatus;
  readonly platform: string;
  readonly evidenceId: string;
}

export interface RepairBenchmarkV1 {
  readonly id: string;
  readonly label: string;
  readonly baseline: string;
  readonly candidate: string;
  readonly delta: string;
  readonly threshold: string;
  readonly status: RepairEvidenceStatus;
  readonly evidenceId: string;
}

export interface RepairDisclosureItemV1 {
  readonly id: string;
  readonly label: string;
  readonly detail: string;
}

export type RepairDisclosureEvidenceState =
  | "loaded_verified"
  | "none_declared"
  | "not_performed"
  | "unloaded"
  | "unavailable"
  | "corrupt";

export interface RepairDisclosureEvidenceV1 {
  /** Exact artifact-read state projected by the native boundary. */
  readonly state: RepairDisclosureEvidenceState;
  readonly explanation: string;
  readonly artifactIds: readonly string[];
}

export interface RepairDisclosureV1 {
  /** Core contract shape/hash validity, independent of artifact availability. */
  readonly contractComplete: boolean;
  /** True only when every required artifact was read, hash-verified, and parsed. */
  readonly complete: boolean;
  readonly hash: string;
  readonly diagnosis: string;
  readonly sourceDiffEvidence: RepairDisclosureEvidenceV1;
  readonly sourceDiffs: readonly RepairSourceDiffV1[];
  readonly configurationDiffEvidence: RepairDisclosureEvidenceV1;
  readonly configurationDiffs: readonly RepairConfigurationDiffV1[];
  readonly testEvidence: RepairDisclosureEvidenceV1;
  readonly tests: readonly RepairTestEvidenceV1[];
  readonly benchmarkEvidence: RepairDisclosureEvidenceV1;
  readonly benchmarks: readonly RepairBenchmarkV1[];
  readonly consequences: readonly RepairDisclosureItemV1[];
  readonly uncertainty: readonly RepairDisclosureItemV1[];
  readonly removals: readonly RepairDisclosureItemV1[];
  readonly disables: readonly RepairDisclosureItemV1[];
  readonly broadenings: readonly RepairDisclosureItemV1[];
  readonly replacements: readonly RepairDisclosureItemV1[];
}

export interface RepairAuthoritySummaryV1 {
  /** This is a trusted-core projection; the presentation never grants it. */
  readonly decision: "frozen" | "reduced" | "blocked_broadening";
  readonly manifestDigest: string;
  readonly summary: string;
}

export interface RepairCandidateV1 {
  readonly id: string;
  readonly version: number;
  readonly errorGroupId: string;
  readonly title: string;
  readonly state:
    | "building"
    | "testing"
    | "ready"
    | "deferred"
    | "superseded"
    | "rejected"
    | "activating"
    | "verified"
    | "rolled_back";
  readonly artifactId: string;
  /** Hash of the complete candidate contract, including disclosure bindings. */
  readonly candidateHash: string;
  /** Hash of the whole-build artifact used by managed-local enrollment. */
  readonly artifactHash: string;
  readonly provenanceHash: string;
  readonly dataCompatibility:
    | "rollback_compatible"
    | "deferred"
    | "incompatible";
  readonly disclosure: RepairDisclosureV1;
  readonly authority: RepairAuthoritySummaryV1;
  readonly rollbackPoint: {
    readonly build: string;
    readonly artifactHash: string;
    readonly description: string;
  };
}

export type ActivationEligibilityCode =
  | "SupportedManagedLocal"
  | "EnrollmentRequired"
  | "PackagedDistribution"
  | "UnknownOrigin"
  | "ConflictingOrigin"
  | "MismatchedCandidate"
  | "MissingCheckout"
  | "MissingToolchain"
  | "IncompatibleData"
  | "IpcDegraded"
  | "Unsupported";

export interface ActivationEligibilityV1 {
  readonly code: ActivationEligibilityCode;
  /** Exact core-projected explanation; never inferred from paths or signing. */
  readonly reason: string;
}

export interface PlatformCapabilityReportV1 {
  readonly id: string;
  readonly reportVersion: number;
  readonly freshness: "fresh" | "stale";
  readonly candidateId: string;
  readonly candidateVersion: number;
  readonly candidateHash: string;
  readonly disclosureHash: string;
  readonly capabilityGeneration: number;
  readonly capabilityDigest: string;
  readonly activationProfile: "ManagedLocalBuildProfileV1" | null;
  readonly buildOrigin:
    | "LocalSourceBuild"
    | "PackagedDistribution"
    | "Unknown"
    | "Conflicting";
  readonly enrollment: "enrolled" | "required" | "not_applicable";
  readonly integrity: string;
  readonly eligibility: ActivationEligibilityV1;
}

export interface ManagementCheckpointRefV1 {
  readonly id: string;
  readonly chatId: string;
  readonly candidateId: string;
  readonly candidateVersion: number;
  readonly createdAt: string;
}

export interface BootstrapResultV1 {
  readonly activationId: string;
  readonly state:
    | "checkpointed"
    | "handed_off"
    | "verifying"
    | "activated_verified"
    | "rolled_back"
    | "unsupported"
    | "manual_recovery_required";
  readonly detail: string;
  readonly receiptHash: string | null;
  readonly checkpoint: ManagementCheckpointRefV1;
}

export interface RepairEvidenceV1 {
  readonly id: string;
  readonly kind:
    | "occurrence"
    | "diagnosis"
    | "diff"
    | "test"
    | "benchmark"
    | "capability"
    | "checkpoint"
    | "bootstrap";
  readonly title: string;
  readonly status: RepairEvidenceStatus;
  readonly source: string;
  readonly createdAt: string;
  readonly summary: string;
  readonly rawReference: string;
}

export interface RepairEventV1 {
  readonly sequence: number;
  readonly kind: string;
  readonly occurredAt: string;
  readonly subjectId: string;
}

export interface ManagementRepairProjectionV1 {
  readonly version: number;
  readonly lastSequence: number;
  /** Ordered delta after the sequence requested from the port. */
  readonly events: readonly RepairEventV1[];
  readonly chat: {
    /** Null until a committed investigation supplies an exact Chat identity. */
    readonly id: string | null;
    readonly title: string;
    readonly scope: string;
    readonly maintainerTier: string;
  };
  readonly errorGroups: readonly ErrorGroupV1[];
  readonly investigation: RepairInvestigationV1 | null;
  readonly candidates: readonly RepairCandidateV1[];
  readonly capabilityReports: readonly PlatformCapabilityReportV1[];
  readonly evidence: readonly RepairEvidenceV1[];
  readonly restartRecovery: BootstrapResultV1 | null;
}

interface RepairCommandBase {
  readonly commandId: string;
}

export type ManagementRepairCommandV1 =
  | (RepairCommandBase & {
      readonly type: "investigate_and_fix";
      readonly errorGroupId: string;
    })
  | (RepairCommandBase & {
      readonly type: "cancel_repair_task";
      readonly investigationId: string;
    })
  | (RepairCommandBase & {
      readonly type: "export_patch" | "export_candidate";
      readonly candidateId: string;
      readonly expectedCandidateVersion: number;
    })
  | (RepairCommandBase & {
      readonly type: "open_rebuild_instructions";
      readonly candidateId: string;
      readonly expectedCandidateVersion: number;
    })
  | (RepairCommandBase & {
      readonly type: "reject_candidate";
      readonly candidateId: string;
      readonly expectedCandidateVersion: number;
    })
  | (RepairCommandBase & {
      readonly type: "request_managed_local_enrollment";
      readonly candidateId: string;
      readonly expectedCandidateVersion: number;
      readonly expectedArtifactHash: string;
    })
  | (RepairCommandBase & {
      readonly type: "refresh_activation_capability";
      readonly candidateId: string;
      readonly expectedCandidateVersion: number;
    })
  | (RepairCommandBase & {
      readonly type: "activate_repair_and_restart";
      readonly candidateId: string;
      readonly expectedCandidateVersion: number;
      readonly expectedCapabilityDigest: string;
    });

export interface ManagementRepairReceiptV1 {
  readonly commandId: string;
  readonly accepted: boolean;
  readonly currentVersion: number;
  readonly reason: string | null;
}
