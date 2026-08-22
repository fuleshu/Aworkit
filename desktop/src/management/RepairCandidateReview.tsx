import { useEffect, useState } from "react";
import { nextManagementCommandId } from "./corePort";
import { CandidateEvidenceSections } from "./CandidateEvidenceSections";
import {
  evaluateActivationGate,
  latestCapabilityReport,
} from "./repair";
import { RepairActivationGate } from "./RepairActivationGate";
import type {
  ManagementRepairCommandV1,
  PlatformCapabilityReportV1,
  RepairCandidateV1,
} from "./types";
import "./candidate-review.css";

interface RepairCandidateReviewProps {
  readonly candidate: RepairCandidateV1;
  readonly capabilityReports: readonly PlatformCapabilityReportV1[];
  readonly projectionStale: boolean;
  readonly commandPending: boolean;
  readonly onSelectEvidence: (id: string) => void;
  readonly onCommand: (command: ManagementRepairCommandV1) => Promise<boolean>;
  readonly confirmDecision: (title: string, body: string) => Promise<boolean>;
}

/** Candidate review composes disclosure and the exact version-bound gate. */
export function RepairCandidateReview({
  candidate,
  capabilityReports,
  projectionStale,
  commandPending,
  onSelectEvidence,
  onCommand,
  confirmDecision,
}: RepairCandidateReviewProps): React.JSX.Element {
  const report = latestCapabilityReport(
    candidate.id,
    candidate.version,
    capabilityReports,
  );
  const [acknowledged, setAcknowledged] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const pending = commandPending || confirming || projectionStale;
  useEffect(() => {
    setAcknowledged(false);
  }, [candidate.id, candidate.version, report?.capabilityDigest]);
  const gate = evaluateActivationGate({
    candidate,
    report,
    projectionStale,
    explicitDecision: acknowledged,
    commandPending: pending,
  });

  const send = async (command: ManagementRepairCommandV1) => {
    const accepted = await onCommand(command);
    if (accepted) setAcknowledged(false);
  };
  const activate = async () => {
    if (!gate.allowed || report === null) return;
    setConfirming(true);
    try {
      const confirmed = await confirmDecision(
        `Activate repair ${candidate.id} version ${candidate.version}?`,
        "Aworkit will checkpoint this Management Chat, hand the exact candidate and capability digest to the bootstrap helper, restart, verify, and roll back on failure.",
      );
      if (!confirmed) return;
      await send({
        type: "activate_repair_and_restart",
        commandId: nextManagementCommandId(),
        candidateId: candidate.id,
        expectedCandidateVersion: candidate.version,
        expectedCapabilityDigest: report.capabilityDigest,
      });
    } finally {
      setConfirming(false);
    }
  };

  return (
    <article className="repair-candidate-review">
      <header className="repair-candidate-header">
        <div>
          <p className="eyebrow">
            REPAIR CANDIDATE {candidate.id} · VERSION {candidate.version}
          </p>
          <h1>{candidate.title}</h1>
        </div>
        <div className="candidate-header-actions">
          <span className={`status ${candidate.state === "ready" ? "ready" : "running"}`}>
            {candidate.state === "ready"
              ? "Candidate ready"
              : candidate.state.replaceAll("_", " ")}
          </span>
          <button
            disabled={pending}
            title="Export the complete source patch without activating it"
            type="button"
            onClick={() =>
              void send({
                type: "export_patch",
                commandId: nextManagementCommandId(),
                candidateId: candidate.id,
                expectedCandidateVersion: candidate.version,
              })
            }
          >
            Export patch
          </button>
          <button
            disabled={pending}
            title="Export the complete candidate bundle for manual review or update"
            type="button"
            onClick={() =>
              void send({
                type: "export_candidate",
                commandId: nextManagementCommandId(),
                candidateId: candidate.id,
                expectedCandidateVersion: candidate.version,
              })
            }
          >
            Export candidate
          </button>
          <button
            disabled={pending}
            title="Open deterministic manual rebuild and update instructions"
            type="button"
            onClick={() =>
              void send({
                type: "open_rebuild_instructions",
                commandId: nextManagementCommandId(),
                candidateId: candidate.id,
                expectedCandidateVersion: candidate.version,
              })
            }
          >
            Rebuild instructions
          </button>
          <button
            className="danger-action"
            disabled={pending || candidate.state === "rejected"}
            title="Reject this exact candidate version; evidence remains available"
            type="button"
            onClick={() => {
              void confirmDecision(
                `Reject repair ${candidate.id} version ${candidate.version}?`,
                "The candidate will not be activated. Its diagnosis and evidence remain in the recurring-error ledger.",
              ).then((confirmed) => {
                if (confirmed)
                  void send({
                    type: "reject_candidate",
                    commandId: nextManagementCommandId(),
                    candidateId: candidate.id,
                    expectedCandidateVersion: candidate.version,
                  });
              });
            }}
          >
            Reject
          </button>
        </div>
      </header>

      <div className="repair-candidate-scroll">
        <CandidateEvidenceSections
          candidate={candidate}
          onSelectEvidence={onSelectEvidence}
        />
      </div>

      <RepairActivationGate
        acknowledged={acknowledged}
        candidate={candidate}
        gate={gate}
        pending={pending}
        report={report}
        onAcknowledgedChange={setAcknowledged}
        onActivate={() => void activate()}
        onDefer={() => setAcknowledged(false)}
        onRefreshCapability={() =>
          void send({
            type: "refresh_activation_capability",
            commandId: nextManagementCommandId(),
            candidateId: candidate.id,
            expectedCandidateVersion: candidate.version,
          })
        }
        onRequestEnrollment={() =>
          void send({
            type: "request_managed_local_enrollment",
            commandId: nextManagementCommandId(),
            candidateId: candidate.id,
            expectedCandidateVersion: candidate.version,
            expectedArtifactHash: candidate.artifactHash,
          })
        }
      />
    </article>
  );
}
