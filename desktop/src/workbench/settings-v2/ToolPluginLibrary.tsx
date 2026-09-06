/** Inert folder discoveries become saved, disabled MCP plugin configurations. */
import { useState } from "react";
import type { McpServerConfiguration, SettingsV2Snapshot } from "../configuration";

export function ToolPluginLibrary({ snapshot, servers, onAdd, onRefresh }: {
  readonly snapshot: SettingsV2Snapshot;
  readonly servers: readonly McpServerConfiguration[];
  readonly onAdd: (server: McpServerConfiguration) => void;
  readonly onRefresh: () => Promise<void>;
}): React.JSX.Element {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!snapshot.toolPluginDirectory) return <></>;
  return <section className="settings-record">
    <div className="settings-record-heading"><h3>Tool plugins</h3><button type="button" disabled={busy}
      title="Read tool-plugin.json manifests in the plugin folder without running code" onClick={() => {
        setBusy(true); setError(null); void onRefresh().catch(e => setError(String(e))).finally(() => setBusy(false));
      }}>{busy ? "Discovering…" : "Refresh plugins"}</button></div>
    <p>Place each plugin in its own folder containing tool-plugin.json. Executable and server plugins use MCP; configure them in the MCP tab after adding.</p>
    <p className="settings-field-help">{snapshot.toolPluginDirectory}</p>
    {error && <p role="alert">{error}</p>}
    {(snapshot.toolPlugins ?? []).map(plugin => {
      const existing = servers.find(server => server.id === plugin.server?.id);
      const update = existing?.plugin && existing.plugin.manifestPath === plugin.server?.plugin?.manifestPath
        && existing.plugin.contentHash !== plugin.server?.plugin?.contentHash;
      const add = () => {
        if (!plugin.server) return;
        const previous = new Map((existing?.tools ?? []).map(tool => [tool.name, tool]));
        const seeded = plugin.server.tools ?? [];
        const declaredNames = new Set(seeded.map(tool => tool.name));
        onAdd({ ...plugin.server, enabled: false, tools: [
          ...seeded.map(tool => ({ ...tool, enabled: previous.get(tool.name)?.enabled ?? tool.enabled,
            options: previous.get(tool.name)?.options ?? tool.options })),
          ...(existing?.tools ?? []).filter(tool => !declaredNames.has(tool.name)),
        ] });
      };
      return <div key={plugin.path} className="settings-record">
      {plugin.server ? <><strong>{plugin.server.name}</strong><p>Version {plugin.server.plugin?.version} · {plugin.server.transport.transport === "stdio" ? "Executable (MCP)" : "Server (MCP)"}</p>
        <button type="button" disabled={Boolean(existing) && !update} title="Load this manifest into the draft with the plugin disabled; review its connection and tools before enabling"
          onClick={add}>{update ? "Load updated plugin" : existing ? "Added" : "Add plugin"}</button></>
        : <p role="alert">{plugin.path}: {plugin.error}</p>}
    </div>; })}
  </section>;
}
