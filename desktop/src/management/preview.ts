import type {
  ActivationEligibilityCode,
  ManagementRepairProjectionV1,
  RepairAuthoritySummaryV1,
  RepairCandidateV1,
} from "./types";

export interface ManagementRepairPreviewOptions {
  readonly eligibility?: ActivationEligibilityCode;
  readonly freshness?: "fresh" | "stale";
  readonly reportCandidateVersion?: number;
  readonly authorityDecision?: RepairAuthoritySummaryV1["decision"];
  readonly disclosureComplete?: boolean;
  readonly dataCompatibility?: RepairCandidateV1["dataCompatibility"];
}

/** Deterministic accepted-mockup fixture, also reusable by contract tests. */
export function createManagementRepairPreviewProjection(
  options: ManagementRepairPreviewOptions = {},
): ManagementRepairProjectionV1 {
  const eligibility = options.eligibility ?? "SupportedManagedLocal";
  const candidate = candidateFixture(options);
  const buildOrigin =
    eligibility === "PackagedDistribution"
      ? "PackagedDistribution"
      : eligibility === "UnknownOrigin"
        ? "Unknown"
        : eligibility === "ConflictingOrigin"
          ? "Conflicting"
          : "LocalSourceBuild";
  return {
    version: 12,
    lastSequence: 8,
    events: eventHistory,
    chat: {
      id: "management.chat",
      title: "Management Chat",
      scope: "Application-wide",
      maintainerTier: "Management Maintainer · tier:quality",
    },
    errorGroups: [
      {
        id: "ER-17",
        fingerprint: "sha256:extension-host-uncertain-exit",
        title: "Extension host restart loop",
        occurrenceCount: 6,
        chatCount: 3,
        firstSeenAt: "2026-08-09T08:31:00Z",
        lastSeenAt: "2026-08-21T09:14:00Z",
        lastRepairAt: "2026-08-09T12:42:00Z",
        state: "regression",
        evidenceIds: ["EV-OCC-17", "EV-DIAG-17"],
      },
      {
        id: "ER-12",
        fingerprint: "sha256:mcp-timeout-burst",
        title: "MCP timeout burst",
        occurrenceCount: 2,
        chatCount: 1,
        firstSeenAt: "2026-08-20T15:05:00Z",
        lastSeenAt: "2026-08-20T15:08:00Z",
        lastRepairAt: null,
        state: "open",
        evidenceIds: ["EV-OCC-12"],
      },
    ],
    investigation: {
      id: "INV-104",
      errorGroupId: "ER-17",
      state: "awaiting_review",
      boundedBy: "20 minutes · frozen Management authority",
      startedAt: "2026-08-21T09:14:00Z",
      steps: [
        { id: "reproduce", label: "Reproduced", state: "completed" },
        {
          id: "root-cause",
          label: "Root cause isolated",
          state: "completed",
        },
        {
          id: "candidate-tested",
          label: "Candidate tested",
          state: "completed",
        },
        {
          id: "review",
          label: "Awaiting your review",
          state: "active",
        },
      ],
    },
    candidates: [candidate],
    capabilityReports: [
      {
        id: "PCR-104-3",
        reportVersion: 7,
        freshness: options.freshness ?? "fresh",
        candidateId: candidate.id,
        candidateVersion: options.reportCandidateVersion ?? candidate.version,
        candidateHash: candidate.candidateHash,
        disclosureHash: candidate.disclosure.hash,
        capabilityGeneration: 18,
        capabilityDigest: "sha256:capability-generation-18",
        activationProfile:
          eligibility === "SupportedManagedLocal" ?
            "ManagedLocalBuildProfileV1"
          : null,
        buildOrigin,
        enrollment:
          eligibility === "EnrollmentRequired" ? "required" :
          eligibility === "SupportedManagedLocal" ? "enrolled" :
          "not_applicable",
        integrity: "Same-user hash/ownership; not publisher verified",
        eligibility: {
          code: eligibility,
          reason: eligibilityReason(eligibility),
        },
      },
    ],
    evidence: [
      {
        id: "EV-OCC-17",
        kind: "occurrence",
        title: "Recurring failure group ER-17",
        status: "failed",
        source: "Recurring-error ledger",
        createdAt: "2026-08-21T09:14:00Z",
        summary: "6 matching occurrences across 3 Chats; prior repair regressed.",
        rawReference: "ledger://error-groups/ER-17",
      },
      {
        id: "EV-DIAG-17",
        kind: "diagnosis",
        title: "Root-cause diagnosis",
        status: "passed",
        source: "Bounded investigation INV-104",
        createdAt: "2026-08-21T09:22:00Z",
        summary:
          "Unknown child-exit acknowledgement was interpreted as definite failure.",
        rawReference: "artifact://repair/R-104/diagnosis.json",
      },
      {
        id: "EV-DIFF-104",
        kind: "diff",
        title: "Complete candidate diff",
        status: "passed",
        source: "Repair candidate R-104 version 3",
        createdAt: "2026-08-21T09:29:00Z",
        summary: "18 source lines and 2 configuration values changed.",
        rawReference: "artifact://repair/R-104/v3/diff.patch",
      },
      {
        id: "EV-TEST-104",
        kind: "test",
        title: "Workspace and restart verification",
        status: "passed",
        source: "Frozen toolchain run",
        createdAt: "2026-08-21T09:37:00Z",
        summary: "428 tests, restart loop, uncertain outcome, and rollback passed.",
        rawReference: "artifact://repair/R-104/v3/tests.json",
      },
      {
        id: "EV-BENCH-104",
        kind: "benchmark",
        title: "Startup benchmark",
        status: "passed",
        source: "Frozen toolchain run",
        createdAt: "2026-08-21T09:39:00Z",
        summary: "Startup median changed by +1.3%, within the 5% threshold.",
        rawReference: "artifact://repair/R-104/v3/benchmarks.json",
      },
      {
        id: "EV-CAP-104",
        kind: "capability",
        title: "Managed-local activation capability",
        status:
          eligibility === "SupportedManagedLocal" ? "passed" : "uncertain",
        source: "Trusted core capability generation 18",
        createdAt: "2026-08-21T09:42:00Z",
        summary: eligibilityReason(eligibility),
        rawReference: "core://repair/capability/PCR-104-3",
      },
    ],
    restartRecovery: null,
  };
}

function candidateFixture(
  options: ManagementRepairPreviewOptions,
): RepairCandidateV1 {
  return {
    id: "R-104",
    version: 3,
    errorGroupId: "ER-17",
    title: "Bound extension-host restarts after uncertain exit",
    state: "ready",
    artifactId: "ART-R-104-V3",
    candidateHash: "sha256:candidate-r104-v3",
    artifactHash: "sha256:whole-build-r104-v3",
    provenanceHash: "sha256:local-source-g81d1a2f",
    dataCompatibility: options.dataCompatibility ?? "rollback_compatible",
    authority: {
      decision: options.authorityDecision ?? "frozen",
      manifestDigest: "sha256:frozen-management-authority",
      summary:
        options.authorityDecision === "blocked_broadening"
          ? "Candidate requests authority outside the frozen Management manifest."
          : "No authority broadening; investigation used the frozen Management manifest.",
    },
    disclosure: {
      contractComplete: true,
      complete: options.disclosureComplete ?? true,
      hash: "sha256:repair-disclosure-r104-v3",
      diagnosis:
        "Unknown child-exit acknowledgement was treated as definite failure, causing immediate restarts and an unbounded loop.",
      sourceDiffEvidence: {
        state: "loaded_verified",
        explanation:
          "Exact source diff bytes matched the core-projected artifact hash.",
        artifactIds: ["EV-DIFF-104"],
      },
      sourceDiffs: [
        {
          id: "diff-extension-supervisor",
          path: "extension_supervisor.rs",
          language: "rust",
          linesChanged: 18,
          lines: [
            {
              id: "line-188",
              oldLine: 188,
              newLine: 188,
              kind: "context",
              content: "match child.wait_result() {",
            },
            {
              id: "line-189-old",
              oldLine: 189,
              newLine: null,
              kind: "removed",
              content: "Err(_) => restart_now(id),",
            },
            {
              id: "line-189-new",
              oldLine: null,
              newLine: 189,
              kind: "added",
              content: "Err(e) => record_uncertain(id, e),",
            },
            {
              id: "line-190-new",
              oldLine: null,
              newLine: 190,
              kind: "added",
              content: "schedule_bounded_backoff(id),",
            },
            {
              id: "line-191",
              oldLine: 190,
              newLine: 191,
              kind: "context",
              content: "}",
            },
          ],
        },
      ],
      configurationDiffEvidence: {
        state: "loaded_verified",
        explanation:
          "Exact configuration diff bytes matched the core-projected artifact hash.",
        artifactIds: ["EV-CONFIG-104"],
      },
      configurationDiffs: [
        {
          id: "config-restart-limit",
          key: "restart_limit",
          before: null,
          after: "5",
          consequence: "Stops after five restart attempts.",
        },
        {
          id: "config-restart-window",
          key: "restart_window",
          before: null,
          after: "10m",
          consequence: "Bounds retry accounting to ten minutes.",
        },
      ],
      testEvidence: {
        state: "loaded_verified",
        explanation:
          "Exact test result bytes matched the core-projected artifact hash.",
        artifactIds: ["EV-TEST-104"],
      },
      tests: [
        {
          id: "test-workspace",
          label: "428 workspace tests passed",
          status: "passed",
          platform: "Linux + Windows",
          evidenceId: "EV-TEST-104",
        },
        {
          id: "test-loop",
          label: "Restart loop test passed",
          status: "passed",
          platform: "Linux + Windows",
          evidenceId: "EV-TEST-104",
        },
        {
          id: "test-uncertain",
          label: "Uncertain outcome not retried",
          status: "passed",
          platform: "Linux + Windows",
          evidenceId: "EV-TEST-104",
        },
        {
          id: "test-rollback",
          label: "Rollback smoke test passed",
          status: "passed",
          platform: "Linux + Windows",
          evidenceId: "EV-TEST-104",
        },
      ],
      benchmarkEvidence: {
        state: "loaded_verified",
        explanation:
          "Exact benchmark result bytes matched the core-projected artifact hash.",
        artifactIds: ["EV-BENCH-104"],
      },
      benchmarks: [
        {
          id: "benchmark-startup",
          label: "Startup median",
          baseline: "842 ms",
          candidate: "853 ms",
          delta: "+1.3%",
          threshold: "Within 5% threshold",
          status: "passed",
          evidenceId: "EV-BENCH-104",
        },
      ],
      consequences: [
        {
          id: "consequence-bounded",
          label: "Bounded restart",
          detail: "Stops after 5 restart attempts.",
        },
        {
          id: "consequence-review",
          label: "Uncertain exit",
          detail: "Uncertain exits require review.",
        },
        {
          id: "consequence-capability",
          label: "Capability",
          detail: "No capability broadening.",
        },
        {
          id: "consequence-authority",
          label: "Authority",
          detail: "No authority broadening.",
        },
      ],
      uncertainty: [
        {
          id: "uncertainty-macos",
          label: "macOS focused restart test",
          detail: "Not run on this machine; Linux and Windows verified.",
        },
      ],
      removals: [],
      disables: [],
      broadenings: [],
      replacements: [],
    },
    rollbackPoint: {
      build: "0.4.0-dev+g81d1a2f",
      artifactHash: "sha256:previous-working-g81d1a2f",
      description:
        "Previous working build; checkpointed Management Chat resumes after focused verification.",
    },
  };
}

function eligibilityReason(code: ActivationEligibilityCode): string {
  switch (code) {
    case "SupportedManagedLocal":
      return "ManagedLocalBuildProfileV1 is supported for this exact candidate and capability generation.";
    case "EnrollmentRequired":
      return "Local build is not enrolled. Set up a managed local installation, then restart from its launcher.";
    case "PackagedDistribution":
      return "Self-activation is unavailable for this packaged build. Export the candidate and update manually.";
    case "UnknownOrigin":
    case "ConflictingOrigin":
      return "Build origin could not be verified. Self-activation is disabled.";
    case "MismatchedCandidate":
      return "The activation report does not match this exact candidate version.";
    case "MissingCheckout":
      return "The frozen source checkout is unavailable. Export the candidate or open rebuild instructions.";
    case "MissingToolchain":
      return "The frozen build toolchain is unavailable. Open rebuild instructions and update manually.";
    case "IncompatibleData":
      return "Candidate data changes are not rollback compatible or deferred.";
    case "IpcDegraded":
      return "Bootstrap helper IPC is degraded. The current build remains running.";
    case "Unsupported":
      return "Self-activation is unsupported for this installation.";
  }
}

const eventHistory = [
  "recurring_failure_recorded",
  "regression_detected",
  "investigation_started",
  "diagnosis_recorded",
  "candidate_registered",
  "tests_completed",
  "capability_reported",
  "candidate_ready_for_review",
].map((kind, index) => ({
  sequence: index + 1,
  kind,
  occurredAt: `2026-08-21T09:${String(14 + index * 4).padStart(2, "0")}:00Z`,
  subjectId: index < 2 ? "ER-17" : "R-104",
}));
