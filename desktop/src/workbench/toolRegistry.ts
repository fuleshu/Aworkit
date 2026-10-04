/** Shared plugin manifests drive tool discovery, defaults, and labelled forms. */
import bundled from "../../tool-plugins/aworkit-native/tool-plugin.json";
import type {
  BuiltInToolConfiguration,
  ComfyUiConfiguration,
  McpServerConfiguration,
} from "./configuration";

/** Stand-in for a caller that supplies no ComfyUI section; adds no workflow tools. */
export const NO_COMFYUI_SECTION: ComfyUiConfiguration = {
  endpoint: "",
  installPath: null,
  launchArguments: [],
  autoStart: false,
  workflowFolder: null,
  workflowTools: [],
};

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

/**
 * ComfyUI tools a section offers to every Agent node: the two read-only
 * authoring helpers, plus one native tool per enabled workflow tool. They are
 * native capabilities, not MCP functions of a server, so they are listed by
 * name exactly like a bundled tool.
 */
export function comfyuiToolEntries(comfyui: ComfyUiConfiguration): { value: string; label: string }[] {
  // A workspace that never configured a ComfyUI workflow tool is not offered
  // ComfyUI entries at all, so the Tools list is unchanged for it.
  if (comfyui.workflowTools.length === 0) return [];
  return [
    { value: "comfyui.list_node_types", label: "ComfyUI · list node types" },
    { value: "comfyui.get_workflow", label: "ComfyUI · get workflow" },
    ...comfyui.workflowTools
      .filter(tool => tool.enabled && tool.id.trim() !== "")
      .map(tool => ({
        value: `comfyui.${tool.id}`,
        label: `ComfyUI · ${tool.name.trim() === "" ? tool.id : tool.name}`,
      })),
  ];
}

/** Bundled, ComfyUI and MCP tools share the exact same workflow selector. */
export function selectableTools(settings: { readonly tools: readonly BuiltInToolConfiguration[]; readonly mcpServers: readonly McpServerConfiguration[]; readonly comfyui?: ComfyUiConfiguration }): { value: string; label: string }[] {
  return [
    ...settings.tools.filter(tool => tool.enabled).map(tool => ({ value: tool.id, label: tool.name })),
    ...comfyuiToolEntries(settings.comfyui ?? NO_COMFYUI_SECTION),
    ...settings.mcpServers.filter(server => server.enabled).flatMap(server => (server.tools ?? [])
      .filter(tool => tool.enabled).map(tool => ({ value: `mcp://${encodeURIComponent(server.name)}/${tool.name}`, label: `${server.name} · ${tool.name}` }))),
  ];
}

export function nativeToolDefaults(): BuiltInToolConfiguration[] {
  // Mirrors the native default: every bundled tool is available to workflows
  // until the user disables it in Settings.
  return nativeTools.map(tool => ({ id: tool.id, name: tool.name, enabled: true, requiresProject: tool.requiresProject,
    credentialBindings: [], configuration: structuredClone(tool.configuration) }));
}
