/** Editable tool guidance and execution policy, independent of tool arguments. */
import type { ToolOptions } from "../configuration";

export function ToolOptionsEditor({ id, value = {}, defaultInstructions, execution, onChange, onPickCommand }: {
  readonly id: string;
  readonly value?: ToolOptions;
  readonly defaultInstructions: string;
  readonly execution: string;
  readonly onChange: (value: ToolOptions) => void;
  readonly onPickCommand?: () => Promise<string | null>;
}): React.JSX.Element {
  const patch = (next: Partial<ToolOptions>) => onChange({ ...value, ...next });
  return <div className="settings-section-stack">
    <label className="settings-field" htmlFor={`${id}-instructions`}>Tool instructions
      <textarea id={`${id}-instructions`} rows={5} title="Instructions included in the system prompt only when this tool is selected. Saving affects new chats."
        value={value.instructions ?? defaultInstructions} onChange={event => patch({ instructions: event.target.value })} />
    </label>
    {value.instructions !== undefined && <button type="button" title={execution === "mcp" ? "Use the server's tool description as its instructions" : "Use the plugin's original instructions"} onClick={() => {
      const next = { ...value }; delete next.instructions; onChange(next);
    }}>{execution === "mcp" ? "Use server description" : "Restore plugin instructions"}</button>}
    <label className="settings-field" htmlFor={`${id}-approval-mode`}>Approval mode
      <select id={`${id}-approval-mode`} title="Override the chat approval mode for this tool, or inherit it. Authority boundaries still apply."
        value={value.approvalMode ?? ""} onChange={event => patch({ approvalMode: event.target.value === "" ? undefined : event.target.value as ToolOptions["approvalMode"] })}>
        <option value="">Use Chat approval mode</option><option value="ask_for_approval">Ask for approval</option>
        <option value="approve_for_me">Approve for me</option><option value="full_access">Full access</option>
      </select>
    </label>
    {(execution === "shell" || execution === "python") && <div className="settings-field">
      <label htmlFor={`${id}-executable`}>{execution === "python" ? "Python executable" : "Shell executable"}</label>
      <input id={`${id}-executable`} title="Absolute executable path. Leave empty to use automatic executable resolution. The resolved path is frozen for the chat."
        placeholder="Automatic" value={value.executable ?? ""} onChange={event => patch({ executable: event.target.value || undefined })} />
      {onPickCommand && <button type="button" title="Choose an executable file" onClick={() => { void onPickCommand().then(path => { if (path) patch({ executable: path }); }); }}>Browse…</button>}
      <small>Runs with your operating-system permissions. This is not a sandbox.</small>
    </div>}
  </div>;
}
