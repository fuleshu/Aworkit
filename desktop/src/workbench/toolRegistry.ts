/** Shared plugin manifests drive tool discovery, defaults, and labelled forms. */
import bundled from "../../tool-plugins/aworkit-native/tool-plugin.json";
import type { McpServerConfiguration, BuiltInToolConfiguration } from "./configuration";

export interface ToolSettingField {
  readonly key: string;
  readonly label: string;
  readonly help: string;
  readonly kind: string;
  readonly readOnly: boolean;
  readonly minimum?: number;
  readonly maximum?: number;
}

export interface ToolManifestEntry {
  readonly id: string;
  readonly name: string;
  readonly description: string;
  readonly instructions: string;
  readonly executor: string;
  readonly activation?: string;
  readonly execution: string;
  readonly requiresProject: boolean;
  readonly configuration: Readonly<Record<string, unknown>>;
  readonly fields: readonly ToolSettingField[];
}

export const nativeTools: readonly ToolManifestEntry[] = bundled.tools;
export const findNativeTool = (id: string): ToolManifestEntry | undefined => nativeTools.find(tool => tool.id === id);

/** MCP tools share the exact same workflow selector as native plugins. */
export function selectableTools(settings: { readonly tools: readonly BuiltInToolConfiguration[]; readonly mcpServers: readonly McpServerConfiguration[] }): { value: string; label: string }[] {
  return [
    ...settings.tools.filter(tool => tool.enabled).map(tool => ({ value: tool.id, label: tool.name })),
    ...settings.mcpServers.filter(server => server.enabled).flatMap(server => (server.tools ?? [])
      .filter(tool => tool.enabled).map(tool => ({ value: `mcp://${server.id}/${tool.name}`, label: `${server.name} · ${tool.name}` }))),
  ];
}

export function nativeToolDefaults(): BuiltInToolConfiguration[] {
  return nativeTools.map(tool => ({ id: tool.id, name: tool.name, enabled: false, requiresProject: tool.requiresProject,
    credentialBindings: [], configuration: structuredClone(tool.configuration) }));
}
