import {
  activationGateSummary,
  activationStatusMessage,
  buildOriginMessage,
  type ActivationGate,
} from "./repair";
import type {
  PlatformCapabilityReportV1,
  RepairCandidateV1,
} from "./types";
import "./activation-gate.css";

interface RepairActivationGateProps {
  readonly candidate: RepairCandidateV1;
  readonly report: PlatformCapabilityReportV1 | null;
  readonly gate: ActivationGate;
  readonly acknowledged: boolean;
  readonly pending: boolean;
  readonly onAcknowledgedChange: (acknowledged: boolean) => void;
  readonly onActivate: () => void;
  readonly onDefer: () => void;
  readonly onRequestEnrollment: () => void;
  readonly onRefreshCapability: () => void;
}

/** Decisive, keyboard-operable activation gate with no force path. */
export function RepairActivationGate({
  candidate,
  report,
  gate,
  acknowledged,
  pending,
  onAcknowledgedChange,
  onActivate,
  onDefer,
  onRequestEnrollment,
  onRefreshCapability,
}: RepairActivationGateProps): React.JSX.Element {
  const supported = report?.eligibility.code === "SupportedManagedLocal";
  const enrollmentRequired =
    report?.eligibility.code === "EnrollmentRequired";
  return (
    <section
      aria-labelledby="activation-gate-title"
      className={`repair-activation-gate ${supported ? "supported" : "unavailable"}`}
    >
      <div className="activation-summary">
        <p className="eyebrow">TRUSTED-CORE PROJECTION</p>
        <h2 id="activation-gate-title">Activation gate</h2>
        <ul>
          <li>{buildOriginMessage(report)}</li>
          <li>{activationStatusMessage(report)}</li>
          <li>
            Integrity: {report?.integrity ?? "Not reported"}
          </li>
          <li>{candidate.authority.summary}</li>
        </ul>
        {report !== null && (
          <details className="capability-report-details">
            <summary title="Show the exact capability report binding">
              Capability report binding
            </summary>
            <dl>
              <div>
                <dt>Candidate</dt>
                <dd>
                  {report.candidateId} version {report.candidateVersion}
                </dd>
              </div>
              <div>
                <dt>Generation</dt>
                <dd>{report.capabilityGeneration}</dd>
              </div>
              <div>
                <dt>Digest</dt>
                <dd>
                  <code>{report.capabilityDigest}</code>
                </dd>
              </div>
              <div>
                <dt>Freshness</dt>
                <dd>{report.freshness}</dd>
              </div>
            </dl>
          </details>
        )}
      </div>

      <div className="activation-decision">
        {supported ? (
          <fieldset>
            <legend>Explicit decision</legend>
            <label className="activation-acknowledgement">
              <input
                checked={acknowledged}
                disabled={pending}
                title={`Confirm review of candidate ${candidate.id} version ${candidate.version}; this does not grant or broaden authority`}
                type="checkbox"
                onChange={(event) =>
                  onAcknowledgedChange(event.currentTarget.checked)
                }
              />
              <span>
                I reviewed the complete disclosure and choose this exact
                candidate version.
                <small>
                  This acknowledgement does not grant or broaden authority.
                </small>
              </span>
            </label>
          </fieldset>
        ) : (
          <p className="activation-unavailable-reason" role="status">
            {activationStatusMessage(report)}
          </p>
        )}
        <p aria-live="polite" className="activation-gate-status" id="activation-gate-status">
          {pending ? "Repair command pending." : activationGateSummary(gate)}
        </p>
        <div className="activation-actions">
          <button
            title="Leave this version available for later review"
            type="button"
            onClick={onDefer}
          >
            Keep as candidate
          </button>
          {enrollmentRequired && (
            <button
              disabled={pending}
              title="Request bounded managed-local enrollment; this does not activate the candidate"
              type="button"
              onClick={onRequestEnrollment}
            >
              Enable local repair activation
            </button>
          )}
          {!supported && !enrollmentRequired && (
            <button
              disabled={pending}
              title="Request a fresh activation capability report from the trusted core"
              type="button"
              onClick={onRefreshCapability}
            >
              Refresh activation report
            </button>
          )}
          {supported && (
            <button
              aria-describedby="activation-gate-status"
              className="activate-restart-action"
              disabled={!gate.allowed}
              title={activationGateSummary(gate)}
              type="button"
              onClick={onActivate}
            >
              Activate and restart
            </button>
          )}
        </div>
        <small>
          Explicit confirmation · candidate {candidate.id} version {candidate.version}
        </small>
      </div>
    </section>
  );
}
