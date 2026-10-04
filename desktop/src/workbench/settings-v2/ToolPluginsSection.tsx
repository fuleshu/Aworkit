/**
 * The Tool Plugins settings tab.
 *
 * One place owns everything about a plugin: the folder it was sourced from, its
 * install/refresh/remove actions, and — once added — its complete configuration,
 * including the MCP command or server address, credentials, functions and
 * approval choices. The general MCP servers tab never shows a plugin.
 */
import type {
  CredentialMetadataConfiguration,
  McpServerConfiguration,
  SettingsV2Snapshot,
} from "../configuration";
import { McpServersSection, type IntegrationProbeResult } from "./IntegrationSections";
import { ToolPluginLibrary } from "./ToolPluginLibrary";

export function ToolPluginsSection({
  snapshot,
  servers,
  credentials,
  onPickCommand,
  onChange,
  onProbe,
  onAdd,
  onRefresh,
  onInstall,
  onOpenFolder,
  onRemove,
}: {
  readonly snapshot: SettingsV2Snapshot;
  /** Every saved MCP connection; plugin-backed ones are edited here. */
  readonly servers: readonly McpServerConfiguration[];
  readonly credentials: readonly CredentialMetadataConfiguration[];
  readonly onPickCommand: () => Promise<string | null>;
  /** Receives the complete plugin-backed server list after an edit. */
  readonly onChange: (servers: readonly McpServerConfiguration[]) => void;
  readonly onProbe: (server: McpServerConfiguration) => Promise<IntegrationProbeResult>;
  readonly onAdd: (server: McpServerConfiguration) => void;
  readonly onRefresh: () => Promise<void>;
  readonly onInstall?: () => Promise<void>;
  readonly onOpenFolder?: () => Promise<void>;
  readonly onRemove?: (key: string) => Promise<void>;
}): React.JSX.Element {
  const plugins = servers.filter((server) => server.plugin !== undefined);
  return (
    <div className="settings-section-stack">
      <ToolPluginLibrary
        snapshot={snapshot}
        servers={servers}
        onAdd={onAdd}
        onRefresh={onRefresh}
        onInstall={onInstall}
        onOpenFolder={onOpenFolder}
        onRemove={onRemove}
      />
      {plugins.length > 0 && (
        <McpServersSection
          servers={plugins}
          credentials={credentials}
          onPickCommand={onPickCommand}
          onChange={onChange}
          onProbe={onProbe}
          allowAdd={false}
          allowRemove={false}
          emptyText=""
          intro="Configure an added plugin here: its command or server address, its arguments — including any executable paths it needs — credentials and limits. Connect and enable it once you have checked them, then save."
        />
      )}
    </div>
  );
}
