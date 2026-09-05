import { useState } from "react";
import "./approvals.css";
import type { ApprovalActionDetails } from "./approvals";

/** A decision stays local until a complete choice (and denial reason) is sent. */
export function ApprovalActions({ disabled, projectScope, onDecision }: {
  readonly disabled: boolean; readonly projectScope?: string;
  readonly onDecision: (details: ApprovalActionDetails) => void;
}): React.JSX.Element {
  const [denying, setDenying] = useState(false);
  const [reason, setReason] = useState("");
  return <div className="approval-actions">
    <div className="activity-actions">
      <button type="button" disabled={disabled} title="Approve only this invocation" onClick={() => onDecision({ choice: "approve_once" })}>Approve once</button>
      <button type="button" disabled={disabled || !projectScope} title={projectScope ?? "A selected project and a tool action are required"} onClick={() => onDecision({ choice: "always_approve_in_project" })}>Always approve in project</button>
      <button type="button" disabled={disabled} title="Deny this action and tell the agent why" onClick={() => setDenying(true)}>Deny and give reason</button>
    </div>
    {projectScope && <small className="approval-project-scope">Project approval: {projectScope.toLowerCase()}.</small>}
    {denying && <form className="approval-denial" onSubmit={event => { event.preventDefault(); if (reason.trim()) onDecision({ choice: "deny", reason: reason.trim() }); }}>
      <label>Reason for denial<textarea autoFocus aria-label="Reason for denial" title="The agent receives this reason and must respect the denial" maxLength={4096} value={reason} disabled={disabled} onChange={event => setReason(event.target.value)} /></label>
      <div className="activity-actions"><button type="submit" disabled={disabled || !reason.trim()} title="Send the denial and reason to the agent">Deny action</button>
      <button type="button" disabled={disabled} title="Return to the approval choices" onClick={() => setDenying(false)}>Cancel</button></div>
    </form>}
  </div>;
}
