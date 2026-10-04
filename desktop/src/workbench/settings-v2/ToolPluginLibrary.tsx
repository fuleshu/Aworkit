/**
 * The one plugin surface: folder-sourced packages that add tools to Aworkit.
 *
 * A plugin is an ordinary folder in one folder Aworkit watches. Copying a
 * folder in only *sources* it: the plugin is listed, turned off, and never run
 * until the user adds it and turns it on. Refresh, open the folder, install a
 * copy from somewhere else, and remove a plugin all live here in plain words.
 */
import { useState } from "react";
import type { McpServerConfiguration, SettingsV2Snapshot } from "../configuration";

export function ToolPluginLibrary({ snapshot, servers, onAdd, onRefresh, onInstall, onOpenFolder, onRemove }: {
  readonly snapshot: SettingsV2Snapshot;
  readonly servers: readonly McpServerConfiguration[];
  readonly onAdd: (server: McpServerConfiguration) => void;
  readonly onRefresh: () => Promise<void>;
  readonly onInstall?: () => Promise<void>;
  readonly onOpenFolder?: () => Promise<void>;
  readonly onRemove?: (key: string) => Promise<void>;
}): React.JSX.Element {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!snapshot.toolPluginDirectory) return <></>;
  const run = (operation: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    void operation().catch(failure => setError(String(failure))).finally(() => setBusy(false));
  };
  return <section className="settings-record">
    <div className="settings-record-heading">
      <h3>Plugins</h3>
      <button type="button" disabled={busy}
        title="Look in the plugin folder again. Nothing is run."
        onClick={() => run(onRefresh)}>{busy ? "Looking…" : "Refresh"}</button>
      {onOpenFolder && <button type="button" disabled={busy}
        title="Show the plugin folder in your file manager"
        onClick={() => run(onOpenFolder)}>Open plugin folder</button>}
      {onInstall && <button type="button" disabled={busy}
        title="Copy a plugin from another folder into the plugin folder"
        onClick={() => run(onInstall)}>Install plugin…</button>}
    </div>
    <p>Each plugin is a folder that contains a tool-plugin.json file. A plugin you copy in appears here turned off; add it, then configure and turn it on below.</p>
    <p className="settings-field-help">{snapshot.toolPluginDirectory}</p>
    {error && <p role="alert">{error}</p>}
    {(snapshot.toolPlugins ?? []).map(plugin => {
      const existing = plugin.server
        ? servers.find(server => server.id === plugin.server?.id)
        : undefined;
      const updated = existing?.plugin && plugin.server?.plugin
        && existing.plugin.manifestPath === plugin.server.plugin.manifestPath
        && existing.plugin.contentHash !== plugin.server.plugin.contentHash;
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
      const remove = () => {
        if (!onRemove) return;
        const key = plugin.server?.id ?? plugin.path;
        run(() => onRemove(key));
      };
      return <div key={plugin.path} className="settings-record">
        {plugin.server ? <>
          <strong>{plugin.server.name}</strong>
          <p>
            Version {plugin.server.plugin?.version} · {plugin.server.transport.transport === "stdio" ? "Runs a command (MCP)" : "Connects to a server (MCP)"}
            {existing ? (existing.enabled ? " · On" : " · Added, turned off") : " · Not added yet"}
          </p>
          <button type="button" disabled={Boolean(existing) && !updated}
            title="Copy this plugin's settings into the draft with the plugin turned off; check its connection and tools before turning it on"
            onClick={add}>{updated ? "Use updated plugin" : existing ? "Added" : "Add plugin"}</button>
          {onRemove && <button type="button" disabled={busy}
            title="Delete this plugin folder. Workflows that used it keep running and are told it is missing."
            onClick={remove}>Remove</button>}
        </> : <>
          <p role="alert">{plugin.path}: {plugin.error}</p>
          {onRemove && <button type="button" disabled={busy}
            title="Delete this plugin folder"
            onClick={remove}>Remove</button>}
        </>}
      </div>;
    })}
  </section>;
}
