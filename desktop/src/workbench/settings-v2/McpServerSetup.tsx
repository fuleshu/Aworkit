import { useEffect, useRef, useState } from "react";
import type { McpServerConfiguration } from "../configuration";
import type { IntegrationProbeResult } from "./IntegrationSections";
import { McpToolOptions } from "./McpToolOptions";
import { mcpDraftFingerprint } from "./settingsDraft";

/** Enabling a server connects and loads its catalog as one explicit action. */
export function McpServerSetup({ server, onChange, onProbe }: {
  readonly server: McpServerConfiguration;
  readonly onChange: (server: McpServerConfiguration) => void;
  readonly onProbe: (server: McpServerConfiguration) => Promise<IntegrationProbeResult>;
}): React.JSX.Element {
  const latest = useRef({ server, onChange });
  latest.current = { server, onChange };
  const sequence = useRef(0);
  useEffect(() => () => { sequence.current += 1; }, []);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ fingerprint: string; message: string; ok: boolean }>();
  const tools = server.tools ?? [];
  const enabledCount = tools.filter(tool => tool.enabled).length;
  const ready = server.enabled && tools.length > 0;
  const currentResult = result?.fingerprint === mcpDraftFingerprint(server) ? result : undefined;

  const connect = async (enable: boolean) => {
    const request = ++sequence.current;
    const fingerprint = mcpDraftFingerprint(server);
    setBusy(true);
    setResult(undefined);
    const isCurrent = () => request === sequence.current &&
      fingerprint === mcpDraftFingerprint(latest.current.server);
    try {
      const probe = await onProbe(server);
      if (!isCurrent()) return;
      if (!probe.ok || !probe.tools?.length) {
        setResult({ fingerprint, ok: false, message: probe.ok
          ? "This server returned no functions. Check its configuration and try again."
          : probe.message });
        return;
      }
      const previous = new Map(tools.map(tool => [tool.name, tool]));
      const next = { ...latest.current.server, enabled: enable || server.enabled,
        tools: probe.tools.map(tool => ({ ...tool,
          enabled: previous.get(tool.name)?.enabled ?? true,
          options: previous.get(tool.name)?.options })) };
      latest.current.onChange(next);
      setResult({ fingerprint: mcpDraftFingerprint(next), ok: true,
        message: "Connection successful. Save configuration to use these functions in workflows." });
    } catch (error) {
      if (isCurrent()) setResult({ fingerprint, ok: false,
        message: error instanceof Error ? error.message : String(error) });
    } finally {
      if (request === sequence.current) setBusy(false);
    }
  };

  return <div className="settings-section-stack">
    <label className="checkbox-row">
      <input type="checkbox" checked={ready} disabled={busy}
        title="Connect and load this server's functions when enabling it; save configuration to make it available to workflows"
        onChange={event => {
          if (event.target.checked) void connect(true);
          else onChange({ ...server, enabled: false });
        }} />Enable MCP server
    </label>
    <p className="settings-field-help" role="status">{busy ? "Connecting and loading functions…"
      : tools.length === 0 ? "Setup required — connect to load this server's functions."
      : !server.enabled ? "Disabled — enable this server to use it in workflows."
      : enabledCount === 0 ? "No functions enabled — enable functions below to use this server."
      : `Available to workflows · ${enabledCount} ${enabledCount === 1 ? "function" : "functions"}`}</p>
    <div className="provider-actions">
      <button type="button" disabled={busy}
        title={ready ? "Reconnect and refresh the function catalog, preserving function settings" : "Connect, load functions, and enable this MCP server in one step"}
        onClick={() => void connect(!ready)}>{busy ? "Connecting…" : ready ? "Refresh functions" : "Connect and enable"}</button>
      {currentResult && <p className={`provider-detail ${currentResult.ok ? "diagnostic" : "error"}`} role="status">{currentResult.message}</p>}
    </div>
    <McpToolOptions server={server} onChange={tools => onChange({ ...server, tools })} />
  </div>;
}
