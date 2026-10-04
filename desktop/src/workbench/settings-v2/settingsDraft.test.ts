import { describe, expect, it } from "vitest";
import type { McpServerConfiguration, SettingsConfigurationV2 } from "../configuration";
import {
  dirtySettingsSections,
  rebaseSettingsDraft,
  settingsDraftIssues,
} from "./settingsDraft";

function server(id: string, plugin: boolean): McpServerConfiguration {
  return {
    id,
    name: id,
    enabled: false,
    autoConnect: false,
    transport: {
      transport: "stdio",
      command: "python",
      args: ["bridge.py"],
      cwd: null,
      env: [],
    },
    tools: [],
    ...(plugin
      ? {
          plugin: {
            manifestPath: `/plugins/${id}/tool-plugin.json`,
            contentHash: "sha256:x",
            version: "1.0.0",
          },
        }
      : {}),
  };
}

function configuration(
  mcpServers: readonly McpServerConfiguration[],
): SettingsConfigurationV2 {
  return {
    schemaVersion: 2,
    providers: [],
    modelTiers: [],
    credentials: [],
    tools: [],
    extensions: [],
    mcpServers: [...mcpServers],
    externalAgents: [],
    data: {
      portableHistoryEnabled: false,
      detailedCaptureEnabled: false,
      portableDirectory: "",
    },
    projects: [],
    appearance: { mode: "system", fontScale: 1 },
    chatDefaults: {},
    layout: {},
    desktop: {},
  } as unknown as SettingsConfigurationV2;
}

describe("tool plugin settings sections", () => {
  it("marks only Tool Plugins dirty when a plugin-backed server changes", () => {
    const canonical = configuration([
      server("plugin.comfyui", true),
      server("mcp.general", false),
    ]);
    const draft = structuredClone(canonical);
    draft.mcpServers[0] = { ...draft.mcpServers[0], enabled: true };
    const dirty = dirtySettingsSections(draft, canonical);
    expect(dirty.has("tool_plugins")).toBe(true);
    expect(dirty.has("mcp")).toBe(false);
  });

  it("marks only MCP servers dirty when a general server changes", () => {
    const canonical = configuration([
      server("plugin.comfyui", true),
      server("mcp.general", false),
    ]);
    const draft = structuredClone(canonical);
    draft.mcpServers[1] = { ...draft.mcpServers[1], enabled: true };
    const dirty = dirtySettingsSections(draft, canonical);
    expect(dirty.has("mcp")).toBe(true);
    expect(dirty.has("tool_plugins")).toBe(false);
  });

  it("rebases the two groups independently", () => {
    const canonical = configuration([
      server("plugin.comfyui", true),
      server("mcp.general", false),
    ]);
    const draft = structuredClone(canonical);
    draft.mcpServers[0] = { ...draft.mcpServers[0], name: "Edited plugin" };
    const latest = structuredClone(canonical);
    latest.mcpServers[1] = { ...latest.mcpServers[1], name: "Edited general" };
    const rebased = rebaseSettingsDraft(
      latest,
      draft,
      dirtySettingsSections(draft, canonical),
    );
    expect(
      rebased.mcpServers.find((entry) => entry.id === "plugin.comfyui")?.name,
    ).toBe("Edited plugin");
    expect(
      rebased.mcpServers.find((entry) => entry.id === "mcp.general")?.name,
    ).toBe("Edited general");
  });

  it("accepts the shipped ComfyUI plugin arguments as secret-free", () => {
    const comfyui: McpServerConfiguration = {
      id: "plugin.comfyui-bridge",
      name: "ComfyUI bridge",
      enabled: false,
      autoConnect: false,
      transport: {
        transport: "stdio",
        command: "python",
        args: [
          "bridge.py",
          "--endpoint",
          "http://127.0.0.1:8188",
          "--workflows",
          "workflows.json",
        ],
        cwd: "/plugins/comfyui-bridge",
        env: [],
      },
      plugin: {
        manifestPath: "/plugins/comfyui-bridge/tool-plugin.json",
        contentHash: "sha256:x",
        version: "1.0.0",
      },
      tools: [
        {
          name: "comfyui_status",
          description: "Status",
          inputSchema: { type: "object" },
          enabled: true,
        },
      ],
    };
    const issues = settingsDraftIssues(configuration([comfyui]), {}).filter(
      (issue) => issue.section === "tool_plugins",
    );
    expect(issues).toEqual([]);
  });

  it("accepts the shipped FFmpeg plugin arguments as secret-free", () => {
    const ffmpeg: McpServerConfiguration = {
      id: "plugin.ffmpeg",
      name: "FFmpeg media tools",
      enabled: false,
      autoConnect: false,
      transport: {
        transport: "stdio",
        command: "python",
        args: [
          "ffmpeg_bridge.py",
          "--ffmpeg",
          "ffmpeg",
          "--ffprobe",
          "ffprobe",
        ],
        cwd: "/plugins/ffmpeg",
        env: [],
      },
      plugin: {
        manifestPath: "/plugins/ffmpeg/tool-plugin.json",
        contentHash: "sha256:x",
        version: "1.0.0",
      },
      tools: [
        {
          name: "ffmpeg_probe",
          description: "Probe",
          inputSchema: { type: "object" },
          enabled: true,
        },
      ],
    };
    const issues = settingsDraftIssues(configuration([ffmpeg]), {}).filter(
      (issue) => issue.section === "tool_plugins",
    );
    expect(issues).toEqual([]);
  });
});
