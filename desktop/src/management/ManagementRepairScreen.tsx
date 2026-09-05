import { useEffect, useRef, useState } from "react";
import { useProjectedNotification } from "../notifications/NotificationContext";
import type { ManagementRepairCorePort } from "./corePort";
import { ManagementSidebar } from "./ManagementSidebar";
import { activeCandidateForGroup, repairCandidateKey } from "./repair";
import { RepairCandidateReview } from "./RepairCandidateReview";
import type { BootstrapResultV1 } from "./types";
import { useManagementRepair } from "./useManagementRepair";
import "./management-layout.css";

interface ManagementRepairScreenProps {
  readonly active?: boolean;
  readonly corePort?: ManagementRepairCorePort;
  readonly pollIntervalMs?: number;
  readonly confirmDecision?: (title: string, body: string) => Promise<boolean>;
}

/** Pinned application-wide Management scope backed only by core projections. */
export function ManagementRepairScreen({
  corePort,
  pollIntervalMs,
  confirmDecision = async () => false,
  active = true,
}: ManagementRepairScreenProps): React.JSX.Element {
  const runtime = useManagementRepair(corePort, pollIntervalMs);
  const [selectedErrorId, setSelectedErrorId] = useState<string | null>(null);
  const [selectedCandidateKey, setSelectedCandidateKey] = useState<string | null>(
    null,
  );
  const [selectedEvidenceId, setSelectedEvidenceId] = useState<string | null>(
    null,
  );
  const [evidenceFilter, setEvidenceFilter] = useState("");
  const detailRef = useRef<HTMLElement>(null);
  const priorRecoveryState = useRef<string | null>(null);
  const snapshot = runtime.snapshot;
  useProjectedNotification("Management", "management", "connection", !runtime.stale ? null : {
    route: "management", summary: "Management projection disconnected.", detail: "The last known state remains visible. Changes are disabled until resynchronized.", severity: "warning", lifetime: { kind: "condition", conditionId: "management-connection" },
    action: { label: "Resync", disabled: runtime.pendingCommandIds.size > 0, run: () => void runtime.resynchronize() },
  });
  useProjectedNotification("Management", "management", "command-error", runtime.stale || runtime.error === null ? null : {
    route: "management", summary: runtime.error, severity: "error", lifetime: { kind: "transient" },
  });
  useProjectedNotification("Management", "management", "command", runtime.pendingCommandIds.size === 0 || runtime.stale ? null : {
    route: "management", summary: "Updating Management…", severity: "progress", lifetime: { kind: "operation", operationId: [...runtime.pendingCommandIds].join(":") },
  });

  useEffect(() => {
    if (snapshot === null) return;
    const nextErrorId = snapshot.errorGroups.some(
      ({ id }) => id === selectedErrorId,
    )
      ? selectedErrorId
      : (snapshot.errorGroups[0]?.id ?? null);
    setSelectedErrorId(nextErrorId);
    setSelectedCandidateKey((current) => {
      if (
        snapshot.candidates.some(
          (candidate) => repairCandidateKey(candidate) === current,
        )
      )
        return current;
      const active = activeCandidateForGroup(
        snapshot.candidates,
        nextErrorId,
      );
      return active === undefined ? null : repairCandidateKey(active);
    });
    setSelectedEvidenceId((current) =>
      snapshot.evidence.some(({ id }) => id === current)
        ? current
        : (snapshot.evidence[0]?.id ?? null),
    );
  }, [selectedErrorId, snapshot]);

  useEffect(() => {
    const state = snapshot?.restartRecovery?.state ?? null;
    const terminal =
      state === "activated_verified" ||
      state === "rolled_back" ||
      state === "unsupported" ||
      state === "manual_recovery_required";
    if (active && terminal && state !== priorRecoveryState.current)
      window.requestAnimationFrame(() => detailRef.current?.focus());
    priorRecoveryState.current = state;
  }, [snapshot?.restartRecovery?.state, active]);

  const selectedCandidate = snapshot?.candidates.find(
    (candidate) => repairCandidateKey(candidate) === selectedCandidateKey,
  );

  if (runtime.loading && snapshot === null)
    return (
      <section className="route-loading" role="status">
        Connecting Management repair review to the trusted core…
      </section>
    );
  if (snapshot === null)
    return (
      <section className="route-error" role="alert">
        <h2>Management repair projection unavailable</h2>
        <p>{runtime.error}</p>
        <button
          title="Retry the trusted-core Management projection query"
          type="button"
          onClick={() => void runtime.resynchronize()}
        >
          Retry
        </button>
      </section>
    );

  const pending = runtime.pendingCommandIds.size > 0;
  return (
    <section className="management-repair-screen">
      <header className="surface-toolbar">
        <div>
          <p className="eyebrow">{snapshot.chat.scope.toUpperCase()} · PINNED</p>
          <div className="management-title-line">
            <h1>{snapshot.chat.title}</h1>
            <span>{snapshot.chat.maintainerTier}</span>
          </div>
        </div>
        <div className="toolbar-actions">
          <span>
            {snapshot.errorGroups.filter(({ state }) => state !== "verified").length} open issue(s)
          </span>
          <span className="status uncertain">
            {snapshot.errorGroups.filter(({ state }) => state === "regression").length} regression
          </span>
          <button
            disabled={pending}
            title="Request a fresh complete Management projection from the trusted core"
            type="button"
            onClick={() => void runtime.resynchronize()}
          >
            Refresh
          </button>
        </div>
      </header>

      {snapshot.restartRecovery !== null && (
        <RestartRecoveryStatus recovery={snapshot.restartRecovery} />
      )}

      <div className="management-repair-body">
        <ManagementSidebar
          disabled={pending || runtime.stale}
          evidenceFilter={evidenceFilter}
          selectedCandidateKey={selectedCandidateKey}
          selectedErrorId={selectedErrorId}
          selectedEvidenceId={selectedEvidenceId}
          snapshot={snapshot}
          onCommand={runtime.dispatch}
          onEvidenceFilter={setEvidenceFilter}
          onSelectCandidate={setSelectedCandidateKey}
          onSelectError={setSelectedErrorId}
          onSelectEvidence={setSelectedEvidenceId}
        />

        <main className="management-detail" ref={detailRef} tabIndex={-1}>
          {selectedCandidate === undefined ? (
            <section className="management-empty-state">
              <h2>No repair candidate selected</h2>
              <p>
                Select a recurring error to inspect its evidence or explicitly
                start a bounded investigation.
              </p>
            </section>
          ) : (
            <RepairCandidateReview
              candidate={selectedCandidate}
              capabilityReports={snapshot.capabilityReports}
              commandPending={pending}
              confirmDecision={confirmDecision}
              projectionStale={runtime.stale}
              onCommand={runtime.dispatch}
              onSelectEvidence={setSelectedEvidenceId}
            />
          )}
        </main>
      </div>
    </section>
  );
}

function RestartRecoveryStatus({
  recovery,
}: {
  readonly recovery: BootstrapResultV1;
}): React.JSX.Element {
  return (
    <section aria-live="assertive" className={`restart-recovery ${recovery.state}`} role="status">
      <strong>Repair restart: {recovery.state.replaceAll("_", " ")}</strong>
      <span>{recovery.detail}</span>
      <small>
        Checkpoint {recovery.checkpoint.id} · candidate {recovery.checkpoint.candidateId} version {recovery.checkpoint.candidateVersion}
      </small>
    </section>
  );
}
