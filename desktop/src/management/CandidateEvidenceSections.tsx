import { SourceDiffViewer } from "./SourceDiffViewer";
import type {
  RepairCandidateV1,
  RepairDisclosureEvidenceV1,
  RepairDisclosureItemV1,
  RepairEvidenceStatus,
} from "./types";

interface CandidateEvidenceSectionsProps {
  readonly candidate: RepairCandidateV1;
  readonly onSelectEvidence: (id: string) => void;
}

/** Complete candidate disclosure, including explicit empty change classes. */
export function CandidateEvidenceSections({
  candidate,
  onSelectEvidence,
}: CandidateEvidenceSectionsProps): React.JSX.Element {
  const disclosure = candidate.disclosure;
  const passedTests = disclosure.tests.filter(
    ({ status }) => status === "passed",
  ).length;
  const uncertainTests = disclosure.tests.filter(
    ({ status }) => status === "uncertain",
  ).length;
  return (
    <div className="candidate-evidence-sections">
      <section aria-labelledby="repair-diagnosis-title" className="repair-card">
        <header>
          <h2 id="repair-diagnosis-title">Diagnosis</h2>
          <span className={`status ${disclosure.complete ? "ready" : "failed"}`}>
            {disclosure.complete
              ? "Complete disclosure"
              : disclosure.contractComplete
                ? "Review evidence unavailable"
                : "Incomplete contract"}
          </span>
        </header>
        <p>{disclosure.diagnosis}</p>
        {!disclosure.complete && (
          <p className="repair-evidence-degraded" role="status">
            Activation is blocked until every required artifact is loaded,
            hash-verified, and parsed into exact review evidence.
          </p>
        )}
      </section>

      <section aria-labelledby="repair-diff-title" className="repair-card diff-card">
        <header>
          <div>
            <p className="eyebrow">DISCLOSURE ARTIFACTS</p>
            <h2 id="repair-diff-title">Complete source &amp; configuration diff</h2>
          </div>
          <span>{evidenceStateLabel(disclosure.sourceDiffEvidence)}</span>
        </header>
        {disclosure.sourceDiffEvidence.state === "loaded_verified" ? (
          <SourceDiffViewer files={disclosure.sourceDiffs} />
        ) : (
          <EvidenceStateNotice
            evidence={disclosure.sourceDiffEvidence}
            title="Source diff"
          />
        )}
        <div className="configuration-diff" aria-label="Configuration changes">
          <h3>Configuration</h3>
          {disclosure.configurationDiffEvidence.state !== "loaded_verified" ? (
            <EvidenceStateNotice
              evidence={disclosure.configurationDiffEvidence}
              title="Configuration diff"
            />
          ) : disclosure.configurationDiffs.length === 0 ? (
            <p>No configuration changes were present in the verified artifact.</p>
          ) : (
            <dl>
              {disclosure.configurationDiffs.map((change) => (
                <div key={change.id}>
                  <dt>
                    <code>{change.key}</code>
                  </dt>
                  <dd>
                    <code>{change.before ?? "not set"}</code>
                    <span aria-hidden="true"> → </span>
                    <span className="sr-only"> changed to </span>
                    <code>{change.after ?? "removed"}</code>
                    <small>{change.consequence}</small>
                  </dd>
                </div>
              ))}
            </dl>
          )}
        </div>
      </section>

      <div className="repair-review-grid">
        <section aria-labelledby="repair-tests-title" className="repair-card">
          <header>
            <h2 id="repair-tests-title">Tests &amp; benchmarks</h2>
            <span>
              {disclosure.tests.length === 0
                ? "No test results projected"
                : `${passedTests} passed · ${uncertainTests} uncertain`}
            </span>
          </header>
          {disclosure.testEvidence.state === "loaded_verified" ? (
            <ul className="repair-result-list">
              {disclosure.tests.map((test) => (
                <li key={test.id}>
                  <ResultMark status={test.status} />
                  <button
                    className="link-button"
                    title={`Inspect evidence for ${test.label}`}
                    type="button"
                    onClick={() => onSelectEvidence(test.evidenceId)}
                  >
                    {test.label}
                  </button>
                  <small>{test.platform}</small>
                </li>
              ))}
            </ul>
          ) : (
            <EvidenceStateNotice evidence={disclosure.testEvidence} title="Tests" />
          )}
          {disclosure.benchmarkEvidence.state === "loaded_verified" ? (
            <dl className="benchmark-list">
              {disclosure.benchmarks.map((benchmark) => (
                <div key={benchmark.id}>
                  <dt>{benchmark.label}</dt>
                  <dd>
                    <button
                      className="benchmark-value"
                      title={`Inspect benchmark evidence for ${benchmark.label}`}
                      type="button"
                      onClick={() => onSelectEvidence(benchmark.evidenceId)}
                    >
                      <ResultMark status={benchmark.status} /> {benchmark.delta}
                    </button>
                    <small>
                      {benchmark.baseline} → {benchmark.candidate} · {benchmark.threshold}
                    </small>
                  </dd>
                </div>
              ))}
            </dl>
          ) : (
            <EvidenceStateNotice
              evidence={disclosure.benchmarkEvidence}
              title="Benchmarks"
            />
          )}
        </section>

        <DisclosureList
          id="repair-consequences-title"
          title="Behavioral consequences"
          items={disclosure.consequences}
        />
        <DisclosureList
          id="repair-uncertainty-title"
          title="Unresolved uncertainty"
          items={disclosure.uncertainty}
          uncertain
        />
        <section aria-labelledby="repair-rollback-title" className="repair-card">
          <header>
            <h2 id="repair-rollback-title">Rollback point</h2>
            <span className="status ready">Core-bound artifact</span>
          </header>
          <strong>{candidate.rollbackPoint.build}</strong>
          <p>{candidate.rollbackPoint.description}</p>
          <code>{candidate.rollbackPoint.artifactHash}</code>
        </section>
      </div>

      <section aria-labelledby="repair-change-scope-title" className="repair-card">
        <header>
          <h2 id="repair-change-scope-title">Removal and authority scope</h2>
          <span
            className={`status ${
              candidate.authority.decision === "blocked_broadening"
                ? "failed"
                : "ready"
            }`}
          >
            {candidate.authority.decision === "frozen"
              ? "Frozen authority"
              : candidate.authority.decision.replaceAll("_", " ")}
          </span>
        </header>
        <p>{candidate.authority.summary}</p>
        <div className="scope-disclosures">
          <DisclosureColumn title="Removed" items={disclosure.removals} />
          <DisclosureColumn title="Disabled" items={disclosure.disables} />
          <DisclosureColumn title="Broadened" items={disclosure.broadenings} />
          <DisclosureColumn title="Replaced" items={disclosure.replacements} />
        </div>
      </section>
    </div>
  );
}

function EvidenceStateNotice({
  evidence,
  title,
}: {
  readonly evidence: RepairDisclosureEvidenceV1;
  readonly title: string;
}): React.JSX.Element {
  return (
    <div className={`repair-evidence-state ${evidence.state}`} role="status">
      <strong>
        {title}: {evidenceStateLabel(evidence)}
      </strong>
      <p>{evidence.explanation}</p>
      {evidence.artifactIds.length > 0 && (
        <small>Artifact references: {evidence.artifactIds.join(", ")}</small>
      )}
    </div>
  );
}

function evidenceStateLabel(evidence: RepairDisclosureEvidenceV1): string {
  switch (evidence.state) {
    case "loaded_verified":
      return "Loaded and hash-verified";
    case "none_declared":
      return "None declared";
    case "not_performed":
      return "Not performed";
    case "unloaded":
      return "Not loaded";
    case "unavailable":
      return "Unavailable";
    case "corrupt":
      return "Hash or parse validation failed";
  }
}

function ResultMark({ status }: { readonly status: RepairEvidenceStatus }): React.JSX.Element {
  return (
    <span aria-label={status} className={`result-mark ${status}`} role="img">
      {evidenceStatusMark(status)}
    </span>
  );
}

function evidenceStatusMark(status: RepairEvidenceStatus): string {
  if (status === "passed") return "✓";
  if (status === "failed" || status === "corrupt") return "×";
  return "!";
}

function DisclosureList({
  id,
  title,
  items,
  uncertain = false,
}: {
  readonly id: string;
  readonly title: string;
  readonly items: readonly RepairDisclosureItemV1[];
  readonly uncertain?: boolean;
}): React.JSX.Element {
  return (
    <section aria-labelledby={id} className="repair-card">
      <header>
        <h2 id={id}>{title}</h2>
        {uncertain && <span className="status uncertain">Uncertain</span>}
      </header>
      <ul className="disclosure-list">
        {items.length === 0 ? (
          <li>None</li>
        ) : (
          items.map((item) => (
            <li key={item.id}>
              <strong>{item.label}</strong>
              <span>{item.detail}</span>
            </li>
          ))
        )}
      </ul>
    </section>
  );
}

function DisclosureColumn({
  title,
  items,
}: {
  readonly title: string;
  readonly items: readonly RepairDisclosureItemV1[];
}): React.JSX.Element {
  return (
    <section>
      <h3>{title}</h3>
      {items.length === 0 ? (
        <p>None</p>
      ) : (
        <ul>
          {items.map((item) => (
            <li key={item.id}>{item.detail}</li>
          ))}
        </ul>
      )}
    </section>
  );
}
