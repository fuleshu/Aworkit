import type {
  PlatformCapabilityReportV1,
  RepairCandidateV1,
  RepairInvestigationV1,
} from "./types";

export type ActivationGateBlocker =
  | "projection_stale"
  | "candidate_not_ready"
  | "disclosure_incomplete"
  | "authority_not_frozen"
  | "data_incompatible"
  | "report_missing"
  | "report_stale"
  | "candidate_mismatch"
  | "candidate_version_mismatch"
  | "candidate_hash_mismatch"
  | "disclosure_hash_mismatch"
  | "capability_digest_missing"
  | "profile_unsupported"
  | "eligibility_unsupported"
  | "explicit_decision_required"
  | "command_pending";

export interface ActivationGate {
  readonly allowed: boolean;
  readonly blockers: readonly ActivationGateBlocker[];
}

/**
 * Evaluates only trusted-core projection fields plus the transient explicit
 * review decision. It never infers origin, integrity, eligibility, or authority
 * from a filesystem path, signature, or UI state.
 */
export function evaluateActivationGate({
  candidate,
  report,
  projectionStale,
  explicitDecision,
  commandPending,
}: {
  readonly candidate: RepairCandidateV1;
  readonly report: PlatformCapabilityReportV1 | null;
  readonly projectionStale: boolean;
  readonly explicitDecision: boolean;
  readonly commandPending: boolean;
}): ActivationGate {
  const blockers: ActivationGateBlocker[] = [];
  if (projectionStale) blockers.push("projection_stale");
  if (candidate.state !== "ready") blockers.push("candidate_not_ready");
  if (!candidate.disclosure.complete) blockers.push("disclosure_incomplete");
  if (candidate.authority.decision !== "frozen")
    blockers.push("authority_not_frozen");
  if (
    candidate.dataCompatibility !== "rollback_compatible" &&
    candidate.dataCompatibility !== "deferred"
  )
    blockers.push("data_incompatible");
  if (report === null) {
    blockers.push("report_missing");
  } else {
    if (report.freshness !== "fresh") blockers.push("report_stale");
    if (report.candidateId !== candidate.id)
      blockers.push("candidate_mismatch");
    if (report.candidateVersion !== candidate.version)
      blockers.push("candidate_version_mismatch");
    if (report.candidateHash !== candidate.candidateHash)
      blockers.push("candidate_hash_mismatch");
    if (report.disclosureHash !== candidate.disclosure.hash)
      blockers.push("disclosure_hash_mismatch");
    if (report.capabilityDigest.trim() === "")
      blockers.push("capability_digest_missing");
    if (report.activationProfile !== "ManagedLocalBuildProfileV1")
      blockers.push("profile_unsupported");
    if (report.eligibility.code !== "SupportedManagedLocal")
      blockers.push("eligibility_unsupported");
  }
  if (!explicitDecision) blockers.push("explicit_decision_required");
  if (commandPending) blockers.push("command_pending");
  return { allowed: blockers.length === 0, blockers };
}

export function latestCapabilityReport(
  candidateId: string,
  candidateVersion: number,
  reports: readonly PlatformCapabilityReportV1[],
): PlatformCapabilityReportV1 | null {
  return (
    [...reports]
      .filter(
        (report) =>
          report.candidateId === candidateId &&
          report.candidateVersion === candidateVersion,
      )
      .sort((left, right) => right.reportVersion - left.reportVersion)[0] ??
    null
  );
}

/** Stable presentation identity for one exact immutable candidate revision. */
export function repairCandidateKey(candidate: {
  readonly id: string;
  readonly version: number;
}): string {
  return `${candidate.id}\u0000${candidate.version}`;
}

/** Selects the core-projected active revision inside one repair group.
 * Candidate version counters are scoped to a candidate identity and therefore
 * must never be compared across unrelated groups or candidate IDs.
 */
export function activeCandidateForGroup(
  candidates: readonly RepairCandidateV1[],
  errorGroupId: string | null,
): RepairCandidateV1 | undefined {
  const scoped = candidates.filter(
    (candidate) =>
      errorGroupId === null || candidate.errorGroupId === errorGroupId,
  );
  return (
    scoped.find((candidate) => candidate.state !== "superseded") ??
    scoped.at(-1)
  );
}

/** Exact unavailable copy is centralized so every presentation state agrees. */
export function activationStatusMessage(
  report: PlatformCapabilityReportV1 | null,
): string {
  if (report === null)
    return "Activation capability has not been reported. Refresh the report before activation.";
  if (report.freshness === "stale")
    return "Activation capability report is stale. Refresh it before activation.";
  switch (report.eligibility.code) {
    case "SupportedManagedLocal":
      return "Repair activation: Available — unprivileged managed slots";
    case "EnrollmentRequired":
      return "Local build is not enrolled. Set up a managed local installation, then restart from its launcher.";
    case "PackagedDistribution":
      return "Self-activation is unavailable for this packaged build. Export the candidate and update manually.";
    case "UnknownOrigin":
    case "ConflictingOrigin":
      return "Build origin could not be verified. Self-activation is disabled.";
    default:
      return report.eligibility.reason;
  }
}

export function buildOriginMessage(
  report: PlatformCapabilityReportV1 | null,
): string {
  if (report === null) return "Build origin: Not reported";
  switch (report.buildOrigin) {
    case "LocalSourceBuild":
      return "Build origin: Local source build";
    case "PackagedDistribution":
      return "Build origin: Packaged distribution";
    case "Conflicting":
      return "Build origin: Conflicting reports";
    case "Unknown":
      return "Build origin: Unknown";
  }
}

export function activationGateSummary(gate: ActivationGate): string {
  if (gate.allowed) return "This exact candidate is ready for explicit activation.";
  const blocker = gate.blockers[0];
  switch (blocker) {
    case "projection_stale":
      return "The Management projection is stale. Resynchronize before making a decision.";
    case "candidate_not_ready":
      return "The candidate is not ready for activation.";
    case "disclosure_incomplete":
      return "The candidate disclosure is incomplete.";
    case "authority_not_frozen":
      return "The candidate is not bound to the frozen authority manifest. Review cannot broaden authority.";
    case "data_incompatible":
      return "The candidate contains rollback-incompatible data changes.";
    case "report_missing":
      return "A trusted-core activation capability report is required.";
    case "report_stale":
      return "The activation capability report is stale.";
    case "candidate_mismatch":
    case "candidate_version_mismatch":
    case "candidate_hash_mismatch":
    case "disclosure_hash_mismatch":
      return "The capability report does not match this exact candidate version and disclosure.";
    case "capability_digest_missing":
      return "The capability generation digest is missing.";
    case "profile_unsupported":
    case "eligibility_unsupported":
      return "The trusted core reports that self-activation is unavailable.";
    case "explicit_decision_required":
      return "Review and acknowledge this exact candidate before activation.";
    case "command_pending":
      return "A repair command is already pending.";
    default:
      return "Activation is unavailable.";
  }
}

export function investigationProgress(
  investigation: RepairInvestigationV1 | null,
): { readonly completed: number; readonly total: number } {
  if (investigation === null) return { completed: 0, total: 0 };
  return {
    completed: investigation.steps.filter((step) => step.state === "completed")
      .length,
    total: investigation.steps.length,
  };
}
