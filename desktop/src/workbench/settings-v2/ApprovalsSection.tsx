import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { z } from "zod";
import { ApprovalModeSelect } from "../../chat/ApprovalModeSelect";
import { approvalModes, type ApprovalMode } from "../../chat/approvals";

const grantSchema = z.object({
  id: z.string(), projectKey: z.string(), projectName: z.string(), capabilityId: z.string(),
  scope: z.string(), actionSummary: z.string(), bindingHash: z.string(), actionHash: z.string(),
}).strict();

/** Default mode uses ordinary Settings Save; revocation removes a live grant. */
export function ApprovalsSection({ mode, onChange }: { readonly mode: ApprovalMode; readonly onChange: (mode: ApprovalMode) => void }): React.JSX.Element {
  const [grants, setGrants] = useState<z.infer<typeof grantSchema>[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const refresh = useCallback(async () => {
    if (!("__TAURI_INTERNALS__" in window)) return;
    setGrants(z.array(grantSchema).parse(await invoke("approval_project_grants")));
  }, []);
  useEffect(() => { void refresh().catch(error => setError(String(error))); }, [refresh]);
  return <div className="settings-section-stack">
    <ApprovalModeSelect label="Default approval mode" value={mode} onChange={onChange} />
    <p className="settings-field-help">{approvalModes.find(option => option.value === mode)?.description} Applies to new chats. Change an existing chat’s mode below its conversation.</p>
    {mode === "approve_for_me" && <p className="settings-field-help">Uses the chat’s configured model in a separate review request. Review rationale and token usage appear in the run’s evidence.</p>}
    <h3>Saved project approvals</h3>
    {error && <p role="alert">{error}</p>}
    {grants.length === 0 ? <p>No saved project approvals.</p> : grants.map(grant => <div className="settings-record" key={grant.id}>
      <strong>{grant.projectName}</strong><p>{grant.capabilityId} · {grant.scope}</p>
      <pre className="approval-grant-summary">{grant.actionSummary}</pre>
      <button type="button" title="Revoke this saved approval immediately; future matching actions will be reviewed again" disabled={busy} onClick={() => {
        setBusy(true); setError(null);
        void invoke("approval_revoke_project_grant", { id: grant.id }).then(refresh).catch(error => setError(String(error))).finally(() => setBusy(false));
      }}>Revoke approval</button>
    </div>)}
  </div>;
}
