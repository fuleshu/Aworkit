import { useState } from "react";
import "./approvals.css";
import { filesystemRequestSchema, type ApprovalActionDetails, type FilesystemSelection } from "./approvals";

/** A decision stays local until a complete choice (and denial reason) is sent. */
export function ApprovalActions({ disabled, projectScope, filesystem, onDecision }: {
  readonly disabled: boolean; readonly projectScope?: string;
  readonly filesystem?: unknown;
  readonly onDecision: (details: ApprovalActionDetails) => void;
}): React.JSX.Element {
  const [denying, setDenying] = useState(false);
  const [reason, setReason] = useState("");
  const request = filesystemRequestSchema.safeParse(filesystem);
  const [access, setAccess] = useState<"read" | "write">(request.success ? request.data.access : "read");
  const [location, setLocation] = useState<"directory" | "all_external">("directory");
  const [directory, setDirectory] = useState(request.success ? displayPath(request.data.directory) : "");
  const selection: FilesystemSelection | undefined = request.success ? { access, location, ...(location === "directory" ? { directory: directory.trim() } : {}) } : undefined;
  const reusableDisabled = disabled || (request.success && location === "directory" && !directory.trim());
  return <div className="approval-actions">
    {request.success && <fieldset className="approval-filesystem" disabled={disabled}>
      <legend>Reusable filesystem permission</legend>
      <p>Read access covers listing, reading, searching, grep and images. Read and write access also covers creating and editing files.</p>
      <label>Access<select aria-label="Filesystem access" title="Choose which file operations to allow" value={access} onChange={event => setAccess(event.target.value as "read" | "write")}>
        <option value="read" disabled={request.data.access === "write"}>Read</option><option value="write">Read and write</option>
      </select></label>
      <label>Location<select aria-label="Filesystem location" title="Choose a folder or explicitly allow every external location" value={location} onChange={event => setLocation(event.target.value as "directory" | "all_external")}>
        <option value="directory">This folder and its subfolders</option><option value="all_external">All locations outside the project folder</option>
      </select></label>
      {location === "directory" && <label>Folder<input aria-label="Permission folder" title="Absolute path to the reference folder; it must contain the requested path" value={directory} onChange={event => setDirectory(event.target.value)} /></label>}
      <small>Saved permissions apply to future matching file operations. Revoke them in Settings → Approvals.</small>
    </fieldset>}
    <div className="activity-actions">
      <button type="button" disabled={disabled} title="Approve only this invocation" onClick={() => onDecision({ choice: "approve_once" })}>Approve once</button>
      {selection && <button type="button" disabled={reusableDisabled} title="Save the selected filesystem permission for this chat, including after a restart" onClick={() => onDecision({ choice: "approve_for_chat", filesystem: selection })}>Allow for this chat</button>}
      <button type="button" disabled={reusableDisabled || !projectScope} title={projectScope ?? "A selected project and a tool action are required"} onClick={() => onDecision({ choice: "always_approve_in_project", ...(selection ? { filesystem: selection } : {}) })}>Always approve in project</button>
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

function displayPath(path: string): string {
  if (path.startsWith("\\\\?\\UNC\\")) return "\\\\" + path.slice(8);
  return path.startsWith("\\\\?\\") ? path.slice(4) : path;
}
