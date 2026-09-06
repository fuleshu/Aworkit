/** A discovered MCP tool has the same editable instructions/policy as native tools. */
import type { McpServerConfiguration, McpToolConfiguration } from "../configuration";
import { ToolOptionsEditor } from "./ToolOptionsEditor";

export function McpToolOptions({ server, onChange }: {
  readonly server: McpServerConfiguration;
  readonly onChange: (tools: McpToolConfiguration[]) => void;
}): React.JSX.Element {
  const tools = server.tools ?? [];
  return <div className="settings-section-stack">
    {tools.length > 0 && <h4>Tools</h4>}
    {tools.map((tool, index) => <details key={tool.name} className="settings-record">
      <summary>{tool.name}{!tool.enabled && " (disabled)"}</summary>
      <p>{tool.description}</p>
      <label className="checkbox-row"><input type="checkbox" checked={tool.enabled} title="Make this tool available to workflow nodes when the server is enabled"
        onChange={event => onChange(tools.map((entry, i) => i === index ? { ...entry, enabled: event.target.checked } : entry))} />Enabled</label>
      <ToolOptionsEditor id={`${server.id}-${tool.name}`} execution="mcp" value={tool.options} defaultInstructions={tool.description}
        onChange={options => onChange(tools.map((entry, i) => i === index ? { ...entry, options } : entry))} />
    </details>)}
  </div>;
}
