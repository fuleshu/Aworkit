import { useEffect, useRef } from "react";
import type { McpServerConfiguration } from "./configuration";

/** One Agent selection per MCP server; old individual bindings stay lossless. */
export function McpServerSelection({ server, selected, editable, onChange }: {
  readonly server: McpServerConfiguration;
  readonly selected: readonly string[];
  readonly editable: boolean;
  readonly onChange: (selected: string[]) => void;
}): React.JSX.Element {
  const binding = `mcp:${server.id}`;
  const prefix = `mcp://${server.id}/`;
  const explicit = selected.filter(id => id.startsWith(prefix));
  const checked = selected.includes(binding);
  const partial = !checked && explicit.length > 0;
  const input = useRef<HTMLInputElement>(null);
  const enabledCount = (server.tools ?? []).filter(tool => tool.enabled).length;
  const available = server.enabled && enabledCount > 0;
  useEffect(() => { if (input.current) input.current.indeterminate = partial; }, [partial]);
  return <label className="checkbox-row">
    <input ref={input} type="checkbox" checked={checked} aria-checked={partial ? "mixed" : checked}
      disabled={!editable || (!available && !checked && !partial)}
      title={partial ? `Replace the previous individual ${server.name} selections with all enabled functions`
        : `Use all enabled ${server.name} functions in this Agent`}
      onChange={() => {
        const rest = selected.filter(id => id !== binding && !id.startsWith(prefix));
        onChange(checked || (!available && partial) ? rest : [...rest, binding]);
      }} />
    <span>{server.name} <small>{!server.enabled ? "Disabled in Settings" : enabledCount === 0 ? "Setup required in Settings → MCP"
      : partial ? `${explicit.length} previously selected · select to use all ${enabledCount}`
      : `${enabledCount} ${enabledCount === 1 ? "function" : "functions"}`}</small></span>
  </label>;
}
