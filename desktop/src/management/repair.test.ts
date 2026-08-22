import { describe, expect, it } from "vitest";
import {
  createManagementRepairCorePort,
  PreviewManagementRepairCorePort,
} from "./corePort";
import { createManagementRepairPreviewProjection } from "./preview";
import {
  activeCandidateForGroup,
  activationStatusMessage,
  evaluateActivationGate,
  latestCapabilityReport,
} from "./repair";
import type { ManagementRepairCommandV1 } from "./types";

describe("Management repair activation contract", () => {
  it("never fabricates repair facts when the native trusted core is absent", async () => {
    await expect(createManagementRepairCorePort().snapshot(0)).rejects.toThrow(
      "browser preview facts are disabled",
    );
  });

  it("requires a fresh exact Supported report, frozen authority, and explicit decision", () => {
    const projection = createManagementRepairPreviewProjection();
    const candidate = projection.candidates[0];
    const report = latestCapabilityReport(
      candidate.id,
      candidate.version,
      projection.capabilityReports,
    );

    expect(
      evaluateActivationGate({
        candidate,
        report,
        projectionStale: false,
        explicitDecision: false,
        commandPending: false,
      }),
    ).toEqual({
      allowed: false,
      blockers: ["explicit_decision_required"],
    });
    expect(
      evaluateActivationGate({
        candidate,
        report,
        projectionStale: false,
        explicitDecision: true,
        commandPending: false,
      }),
    ).toEqual({ allowed: true, blockers: [] });
  });

  it("never crosses capability reports between duplicate candidate IDs", () => {
    const projection = duplicateVersionProjection();

    expect(
      latestCapabilityReport("R-104", 2, projection.capabilityReports),
    ).toMatchObject({
      candidateVersion: 2,
      candidateHash: "sha256:candidate-r104-v2",
      capabilityDigest: "sha256:capability-generation-17",
    });
    expect(
      latestCapabilityReport("R-104", 3, projection.capabilityReports),
    ).toMatchObject({
      candidateVersion: 3,
      candidateHash: "sha256:candidate-r104-v3",
      capabilityDigest: "sha256:capability-generation-18",
    });
  });

  it("selects the active candidate within a group without comparing unrelated version counters", () => {
    const current = createManagementRepairPreviewProjection().candidates[0];
    const superseded = {
      ...current,
      id: "candidate.old-lineage",
      errorGroupId: "group.selection",
      version: 99,
      state: "superseded" as const,
    };
    const active = {
      ...current,
      id: "candidate.new-lineage",
      errorGroupId: "group.selection",
      version: 1,
    };

    expect(
      activeCandidateForGroup([superseded, active], "group.selection"),
    ).toBe(active);
  });

  it("blocks stale projections, report/version/hash drift, and pending commands", () => {
    const projection = createManagementRepairPreviewProjection({
      freshness: "stale",
      reportCandidateVersion: 2,
    });
    const candidate = projection.candidates[0];
    const original = projection.capabilityReports[0];
    const report = {
      ...original,
      candidateHash: "sha256:changed-candidate",
      disclosureHash: "sha256:changed-disclosure",
    };
    expect(
      evaluateActivationGate({
        candidate,
        report,
        projectionStale: true,
        explicitDecision: true,
        commandPending: true,
      }).blockers,
    ).toEqual([
      "projection_stale",
      "report_stale",
      "candidate_version_mismatch",
      "candidate_hash_mismatch",
      "disclosure_hash_mismatch",
      "command_pending",
    ]);
  });

  it("cannot use review acknowledgement to activate or broaden authority", () => {
    const projection = createManagementRepairPreviewProjection({
      authorityDecision: "blocked_broadening",
    });
    const candidate = projection.candidates[0];
    const gate = evaluateActivationGate({
      candidate,
      report: projection.capabilityReports[0],
      projectionStale: false,
      explicitDecision: true,
      commandPending: false,
    });
    expect(gate.allowed).toBe(false);
    expect(gate.blockers).toContain("authority_not_frozen");
  });

  it("rejects incomplete disclosure and rollback-incompatible data independently", () => {
    const projection = createManagementRepairPreviewProjection({
      disclosureComplete: false,
      dataCompatibility: "incompatible",
    });
    const gate = evaluateActivationGate({
      candidate: projection.candidates[0],
      report: projection.capabilityReports[0],
      projectionStale: false,
      explicitDecision: true,
      commandPending: false,
    });
    expect(gate.blockers).toContain("disclosure_incomplete");
    expect(gate.blockers).toContain("data_incompatible");
  });

  it.each([
    [
      "EnrollmentRequired",
      "Local build is not enrolled. Set up a managed local installation, then restart from its launcher.",
    ],
    [
      "PackagedDistribution",
      "Self-activation is unavailable for this packaged build. Export the candidate and update manually.",
    ],
    [
      "UnknownOrigin",
      "Build origin could not be verified. Self-activation is disabled.",
    ],
    [
      "ConflictingOrigin",
      "Build origin could not be verified. Self-activation is disabled.",
    ],
  ] as const)("uses exact %s unavailable copy", (eligibility, expected) => {
    const projection = createManagementRepairPreviewProjection({ eligibility });
    expect(activationStatusMessage(projection.capabilityReports[0])).toBe(
      expected,
    );
  });

  it("binds activation to candidate version and capability digest with no authority mutation", async () => {
    const projection = createManagementRepairPreviewProjection();
    const port = new PreviewManagementRepairCorePort(projection);
    const command: ManagementRepairCommandV1 = {
      type: "activate_repair_and_restart",
      commandId: "desktop.management.test.activate",
      candidateId: "R-104",
      expectedCandidateVersion: 3,
      expectedCapabilityDigest: "sha256:capability-generation-18",
    };
    expect(Object.keys(command).sort()).toEqual([
      "candidateId",
      "commandId",
      "expectedCandidateVersion",
      "expectedCapabilityDigest",
      "type",
    ]);
    const accepted = await port.command(command, projection.version);
    expect(accepted).toMatchObject({ accepted: true });
    await expect(
      port.command(command, accepted.currentVersion),
    ).resolves.toEqual(accepted);
    const after = await port.snapshot(projection.lastSequence);
    expect(after.restartRecovery?.state).toBe("checkpointed");
    expect(after.restartRecovery?.checkpoint).toMatchObject({
      candidateId: "R-104",
      candidateVersion: 3,
    });
  });

  it("revalidates the decisive gate in the preview core boundary", async () => {
    const projection = createManagementRepairPreviewProjection({
      eligibility: "PackagedDistribution",
    });
    const port = new PreviewManagementRepairCorePort(projection);
    await expect(
      port.command(
        {
          type: "activate_repair_and_restart",
          commandId: "desktop.management.test.reject",
          candidateId: "R-104",
          expectedCandidateVersion: 3,
          expectedCapabilityDigest: "sha256:capability-generation-18",
        },
        projection.version,
      ),
    ).resolves.toMatchObject({
      accepted: false,
      reason: expect.stringContaining("eligibility_unsupported"),
    });
  });
});

function duplicateVersionProjection() {
  const projection = createManagementRepairPreviewProjection();
  const current = projection.candidates[0];
  const currentReport = projection.capabilityReports[0];
  const older = {
    ...current,
    version: 2,
    title: "Earlier bounded restart candidate",
    candidateHash: "sha256:candidate-r104-v2",
    disclosure: {
      ...current.disclosure,
      hash: "sha256:disclosure-r104-v2",
    },
  };
  const olderReport = {
    ...currentReport,
    id: "CAP-R104-V2",
    reportVersion: currentReport.reportVersion - 1,
    candidateVersion: 2,
    candidateHash: older.candidateHash,
    disclosureHash: older.disclosure.hash,
    capabilityGeneration: 17,
    capabilityDigest: "sha256:capability-generation-17",
  };
  return {
    ...projection,
    candidates: [older, current],
    capabilityReports: [olderReport, currentReport],
  };
}
