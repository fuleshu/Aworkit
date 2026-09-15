import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { z } from "zod";

const grantsSchema = z.array(z.object({
  id: z.string(), access: z.enum(["read", "write"]),
  owner: z.discriminatedUnion("scope", [
    z.object({ scope: z.literal("project"), projectKey: z.string(), projectName: z.string() }),
    z.object({ scope: z.literal("chat"), chatId: z.string() }),
  ]),
  directory: z.object({ root: z.string() }).passthrough().nullable(),
}));

/** Live core-owned grants are independent of the Settings draft and tool rules. */
export function FilesystemPermissions(): React.JSX.Element {
  const [grants, setGrants] = useState<z.infer<typeof grantsSchema>>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const refresh = useCallback(async () => {
    if ("__TAURI_INTERNALS__" in window) setGrants(grantsSchema.parse(await invoke("approval_filesystem_grants")));
  }, []);
  useEffect(() => { void refresh().catch(error => setError(String(error))); }, [refresh]);
  return <section className="settings-section-stack" aria-label="Filesystem permissions">
    <h3>Filesystem permissions</h3>
    <p className="settings-field-help">Shared by all enabled file tools. Project permissions apply to every chat bound to that project and workspace. Chat permissions apply only to their chat. Permissions survive restarts.</p>
    {error && <p role="alert">{error}</p>}
    {grants.length === 0 ? <p>No saved filesystem permissions.</p> : grants.map(grant => <div className="settings-record" key={grant.id}>
      <strong>{grant.owner.scope === "project" ? `Project: ${grant.owner.projectName}` : `Chat: ${grant.owner.chatId}`}</strong>
      <p>{grant.access === "write" ? "Read and write" : "Read"} · {grant.directory ? "Folder and subfolders" : "All locations outside the project folder"}</p>
      {grant.directory && <pre className="approval-grant-summary">{grant.directory.root}</pre>}
      <button type="button" disabled={busy} title="Revoke this filesystem permission; future matching operations will need approval again" onClick={() => {
        setBusy(true); setError(null);
        void invoke("approval_revoke_filesystem_grant", { id: grant.id }).then(refresh).catch(error => setError(String(error))).finally(() => setBusy(false));
      }}>Revoke permission</button>
    </div>)}
  </section>;
}
