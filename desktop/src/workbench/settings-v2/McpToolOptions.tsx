/** Per-function guidance and explicit MCP approval choices. */
import { useEffect, useState } from "react";
import type { McpServerConfiguration, McpToolConfiguration } from "../configuration";
import { ToolOptionsEditor } from "./ToolOptionsEditor";
import { mcpDraftFingerprint } from "./settingsDraft";
import { McpAutoApproveDialog } from "./McpAutoApproveDialog";

export function McpToolOptions({ server, onChange }: {
  readonly server: McpServerConfiguration;
  readonly onChange: (tools: McpToolConfiguration[]) => void;
}): React.JSX.Element {
  const tools = server.tools ?? [];
  const [pending, setPending] = useState<{ name: string; fingerprint: string } | null>(null);
  const fingerprint = mcpDraftFingerprint(server);
  const confirmation = pending?.fingerprint === fingerprint ? pending : null;
  useEffect(() => {
    setPending(current => current?.fingerprint === fingerprint ? current : null);
  }, [fingerprint]);
  const setAutoApprove = (name: string, checked: boolean) => onChange(tools.map(entry => {
    if (entry.name !== name) return entry;
    // Match native serialization so Settings can verify the saved document.
    const options = { ...entry.options };
    if (checked) options.autoApprove = true;
    else delete options.autoApprove;
    const updated = { ...entry };
    if (Object.keys(options).length > 0) updated.options = options;
    else delete updated.options;
    return updated;
  }));
  return <div className="settings-section-stack">
    {tools.length > 0 && <h4>Function settings</h4>}
    {tools.map((tool, index) => <details key={tool.name} className="settings-record">
      <summary>{tool.name}{!tool.enabled && " (disabled)"}</summary>
      <p>{tool.description}</p>
      <div className="mcp-function-permissions">
      <label className="checkbox-row"><input type="checkbox" checked={tool.enabled} title="Make this tool available to workflow nodes when the server is enabled"
        onChange={event => onChange(tools.map((entry, i) => i === index ? { ...entry, enabled: event.target.checked } : entry))} />Enabled</label>
      <label className="checkbox-row"><input type="checkbox" checked={tool.options?.autoApprove ?? false}
        title="Skip all approval prompts and automatic review for this MCP tool. Enabling requires confirmation; saving affects new Chats."
        onChange={event => {
          if (event.target.checked) setPending({ name: tool.name, fingerprint: mcpDraftFingerprint(server) });
          else setAutoApprove(tool.name, false);
        }} />Auto approve</label>
      <small>When off, approval is skipped only if the server explicitly declares this function read-only and non-destructive. Otherwise, the approval mode below applies.</small>
      </div>
      <ToolOptionsEditor id={`${server.id}-${tool.name}`} execution="mcp" value={tool.options} defaultInstructions={tool.description}
        onChange={options => onChange(tools.map((entry, i) => i === index ? { ...entry, options } : entry))} />
    </details>)}
    {confirmation && <McpAutoApproveDialog tool={confirmation.name} server={server.name} onDecision={accepted => {
      setPending(null);
      if (accepted) setAutoApprove(confirmation.name, true);
    }} />}
  </div>;
}
