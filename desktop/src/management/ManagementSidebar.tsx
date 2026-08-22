import { useMemo } from "react";
import { nextManagementCommandId } from "./corePort";
import {
  activeCandidateForGroup,
  investigationProgress,
  repairCandidateKey,
} from "./repair";
import type {
  ErrorGroupV1,
  ManagementRepairCommandV1,
  ManagementRepairProjectionV1,
  RepairEvidenceV1,
  RepairInvestigationV1,
} from "./types";

interface ManagementSidebarProps {
  readonly snapshot: ManagementRepairProjectionV1;
  readonly selectedErrorId: string | null;
  readonly selectedCandidateKey: string | null;
  readonly selectedEvidenceId: string | null;
  readonly evidenceFilter: string;
  readonly disabled: boolean;
  readonly onSelectError: (id: string) => void;
  readonly onSelectCandidate: (id: string | null) => void;
  readonly onSelectEvidence: (id: string) => void;
  readonly onEvidenceFilter: (value: string) => void;
  readonly onCommand: (command: ManagementRepairCommandV1) => Promise<boolean>;
}

/** Master pane for recurring errors, bounded progress, candidates, and evidence. */
export function ManagementSidebar({
  snapshot,
  selectedErrorId,
  selectedCandidateKey,
  selectedEvidenceId,
  evidenceFilter,
  disabled,
  onSelectError,
  onSelectCandidate,
  onSelectEvidence,
  onEvidenceFilter,
  onCommand,
}: ManagementSidebarProps): React.JSX.Element {
  const selectedError = snapshot.errorGroups.find(
    ({ id }) => id === selectedErrorId,
  );
  const selectedCandidate = snapshot.candidates.find(
    (candidate) => repairCandidateKey(candidate) === selectedCandidateKey,
  );
  const evidence = useMemo(() => {
    const query = evidenceFilter.trim().toLocaleLowerCase();
    return snapshot.evidence.filter((record) => {
      const inScope =
        selectedError?.evidenceIds.includes(record.id) === true ||
        selectedCandidate !== undefined ||
        selectedError === undefined;
      return (
        inScope &&
        (query === "" ||
          `${record.title} ${record.kind} ${record.source} ${record.summary}`
            .toLocaleLowerCase()
            .includes(query))
      );
    });
  }, [evidenceFilter, selectedCandidate, selectedError, snapshot.evidence]);
  const selectedEvidence = snapshot.evidence.find(
    ({ id }) => id === selectedEvidenceId,
  );
  return (
    <aside aria-label="Recurring errors and repair evidence" className="management-master">
      <RecurringErrorList
        errors={snapshot.errorGroups}
        selectedId={selectedErrorId}
        onSelect={(id) => {
          onSelectError(id);
          const candidate = activeCandidateForGroup(snapshot.candidates, id);
          onSelectCandidate(
            candidate === undefined ? null : repairCandidateKey(candidate),
          );
        }}
      />
      <InvestigationProgress
        disabled={disabled}
        error={selectedError}
        investigation={snapshot.investigation}
        onCancel={(investigationId) =>
          void onCommand({
            type: "cancel_repair_task",
            commandId: nextManagementCommandId(),
            investigationId,
          })
        }
        onInvestigate={(errorGroupId) =>
          void onCommand({
            type: "investigate_and_fix",
            commandId: nextManagementCommandId(),
            errorGroupId,
          })
        }
      />
      <section aria-labelledby="repair-candidates-title" className="management-master-section">
        <h2 id="repair-candidates-title">Candidates</h2>
        <div className="master-button-list">
          {snapshot.candidates
            .filter(
              ({ errorGroupId }) =>
                selectedErrorId === null || errorGroupId === selectedErrorId,
            )
            .map((candidate) => (
              <button
                aria-pressed={repairCandidateKey(candidate) === selectedCandidateKey}
                key={repairCandidateKey(candidate)}
                title={`Review repair candidate ${candidate.id} version ${candidate.version}`}
                type="button"
                onClick={() => onSelectCandidate(repairCandidateKey(candidate))}
              >
                <span>
                  <strong>Repair candidate {candidate.id}</strong>
                  <small>Version {candidate.version}</small>
                </span>
                <span className={`status ${candidate.state === "ready" ? "ready" : "running"}`}>
                  {candidate.state.replaceAll("_", " ")}
                </span>
              </button>
            ))}
        </div>
      </section>
      <EvidenceSelection
        evidence={evidence}
        filter={evidenceFilter}
        selected={selectedEvidence}
        selectedId={selectedEvidenceId}
        onFilter={onEvidenceFilter}
        onSelect={onSelectEvidence}
      />
    </aside>
  );
}

function RecurringErrorList({
  errors,
  selectedId,
  onSelect,
}: {
  readonly errors: readonly ErrorGroupV1[];
  readonly selectedId: string | null;
  readonly onSelect: (id: string) => void;
}): React.JSX.Element {
  return (
    <section aria-labelledby="recurring-errors-title" className="management-master-section">
      <header>
        <h2 id="recurring-errors-title">Recurring errors</h2>
        <span>{errors.length}</span>
      </header>
      <div className="master-button-list">
        {errors.map((error) => (
          <button
            aria-pressed={error.id === selectedId}
            key={error.id}
            title={`Inspect recurring error ${error.id}: ${error.title}`}
            type="button"
            onClick={() => onSelect(error.id)}
          >
            <span>
              <strong>{error.id} · {error.title}</strong>
              <small>{error.occurrenceCount} occurrences in {error.chatCount} Chats</small>
              {error.lastRepairAt !== null && <small>Previously repaired</small>}
            </span>
            <span className={`status ${error.state === "regression" ? "failed" : "uncertain"}`}>
              {error.state}
            </span>
          </button>
        ))}
      </div>
    </section>
  );
}

function InvestigationProgress({
  error,
  investigation,
  disabled,
  onInvestigate,
  onCancel,
}: {
  readonly error: ErrorGroupV1 | undefined;
  readonly investigation: RepairInvestigationV1 | null;
  readonly disabled: boolean;
  readonly onInvestigate: (errorGroupId: string) => void;
  readonly onCancel: (investigationId: string) => void;
}): React.JSX.Element {
  const scoped = investigation?.errorGroupId === error?.id ? investigation : null;
  const progress = investigationProgress(scoped);
  return (
    <section aria-labelledby="investigation-title" className="management-master-section">
      <header>
        <h2 id="investigation-title">Investigation</h2>
        {scoped !== null && <span className="status running">{scoped.state.replaceAll("_", " ")}</span>}
      </header>
      {scoped === null ? (
        <button
          className="primary-action master-full-action"
          disabled={disabled || error === undefined}
          title="Start a user-requested bounded investigation using the frozen Management authority"
          type="button"
          onClick={() => error !== undefined && onInvestigate(error.id)}
        >
          Investigate and fix
        </button>
      ) : (
        <>
          <div
            aria-label="Investigation progress"
            aria-valuemax={progress.total}
            aria-valuemin={0}
            aria-valuenow={progress.completed}
            className="progress-track"
            role="progressbar"
          >
            <span style={{ width: `${(progress.completed / Math.max(1, progress.total)) * 100}%` }} />
          </div>
          <ol className="investigation-steps">
            {scoped.steps.map((step) => (
              <li className={step.state} key={step.id}>
                <span aria-hidden="true">{step.state === "completed" ? "✓" : step.state === "active" ? "•" : "○"}</span>
                {step.label}<span className="sr-only"> — {step.state}</span>
              </li>
            ))}
          </ol>
          <small>{scoped.boundedBy}</small>
          {scoped.state === "running" && (
            <button
              className="danger-action master-full-action"
              disabled={disabled}
              title="Cancel this bounded repair investigation; committed evidence remains"
              type="button"
              onClick={() => onCancel(scoped.id)}
            >
              Cancel investigation
            </button>
          )}
        </>
      )}
    </section>
  );
}

function EvidenceSelection({
  evidence,
  selected,
  selectedId,
  filter,
  onFilter,
  onSelect,
}: {
  readonly evidence: readonly RepairEvidenceV1[];
  readonly selected: RepairEvidenceV1 | undefined;
  readonly selectedId: string | null;
  readonly filter: string;
  readonly onFilter: (value: string) => void;
  readonly onSelect: (id: string) => void;
}): React.JSX.Element {
  return (
    <section aria-labelledby="repair-evidence-title" className="management-master-section evidence-selection">
      <header><h2 id="repair-evidence-title">Evidence</h2><span>{evidence.length}</span></header>
      <label>
        <span>Filter evidence</span>
        <input
          aria-label="Filter repair evidence"
          placeholder="Tests, capability, diff…"
          title="Filter the projected repair evidence by title, kind, source, or summary"
          type="search"
          value={filter}
          onChange={(event) => onFilter(event.currentTarget.value)}
        />
      </label>
      <div className="evidence-button-list">
        {evidence.map((record) => (
          <button
            aria-pressed={record.id === selectedId}
            key={record.id}
            title={`Inspect ${record.title}`}
            type="button"
            onClick={() => onSelect(record.id)}
          >
            <span className={`result-mark ${record.status}`} aria-hidden="true">
              {record.status === "passed"
                ? "✓"
                : record.status === "failed" || record.status === "corrupt"
                  ? "×"
                  : "!"}
            </span>
            <span><strong>{record.title}</strong><small>{record.kind}</small></span>
          </button>
        ))}
      </div>
      {selected !== undefined && (
        <article aria-live="polite" className="selected-repair-evidence">
          <strong>{selected.title}</strong><p>{selected.summary}</p>
          <dl>
            <div><dt>Source</dt><dd>{selected.source}</dd></div>
            <div><dt>Reference</dt><dd><code>{selected.rawReference}</code></dd></div>
          </dl>
        </article>
      )}
    </section>
  );
}
