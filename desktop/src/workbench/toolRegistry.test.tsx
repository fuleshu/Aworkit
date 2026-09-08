// @vitest-environment jsdom
import { useState } from "react";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { McpServersSection } from "./settings-v2/IntegrationSections";
import { ToolConfigurationEditor } from "./settings-v2/ToolConfigurationEditor";
import { ToolPluginLibrary } from "./settings-v2/ToolPluginLibrary";
import { nativeToolDefaults, findNativeTool, selectableTools } from "./toolRegistry";
import { type BuiltInToolConfiguration, type McpServerConfiguration, type SettingsV2Snapshot } from "./configuration";

afterEach(cleanup);

it("edits instructions, executable and limits through typed controls", () => {
  let latest: BuiltInToolConfiguration;
  function Editor() {
    const [tool, setTool] = useState(nativeToolDefaults().find(tool => tool.id === "tool.shell.host")!);
    latest = tool;
    return <ToolConfigurationEditor tool={tool} onChange={setTool} />;
  }
  render(<Editor />);
  fireEvent.change(screen.getByLabelText("Tool instructions"), { target: { value: "Run commands only when they help the task." } });
  fireEvent.change(screen.getByLabelText("Shell executable"), { target: { value: "C:\\Tools\\pwsh.exe" } });
  fireEvent.change(screen.getByLabelText("Approval mode"), { target: { value: "ask_for_approval" } });
  const field = findNativeTool("tool.shell.host")!.fields.find(field => field.key === "timeoutSeconds")!;
  fireEvent.change(screen.getByLabelText(field.label), { target: { value: "45" } });
  expect(latest!.options).toEqual({ instructions: "Run commands only when they help the task.", executable: "C:\\Tools\\pwsh.exe", approvalMode: "ask_for_approval" });
  expect(latest!.configuration.timeoutSeconds).toBe(45);
  fireEvent.click(screen.getByText("Restore plugin instructions"));
  expect(latest!.options?.instructions).toBeUndefined();
  expect(screen.getByLabelText("Tool instructions")).toHaveValue(findNativeTool("tool.shell.host")!.instructions);
  expect(screen.queryByLabelText(/JSON/)).toBeNull();
});

it("merges discovered MCP tools without overwriting overrides or another edited server", async () => {
  const base: McpServerConfiguration = { id: "mcp.test", name: "Test", enabled: true, autoConnect: false,
    transport: { transport: "http", url: "http://localhost:3100/mcp", headers: [] },
    tools: [{ name: "echo", description: "Old description", inputSchema: { type: "object" }, enabled: false,
      options: { instructions: "My guidance", approvalMode: "ask_for_approval" } }] };
  let finish!: (value: any) => void;
  const probe = vi.fn((server: McpServerConfiguration) => new Promise<any>(resolve => {
    finish = value => resolve({ ...value, draftFingerprint: JSON.stringify(server) });
  }));
  let latest: readonly McpServerConfiguration[] = [];
  function Editor() {
    const [servers, setServers] = useState<readonly McpServerConfiguration[]>([base, { ...base, id: "mcp.other", name: "Other", tools: [] }]);
    latest = servers;
    return <McpServersSection servers={servers} credentials={[]} onPickCommand={async () => null} onChange={setServers} onProbe={probe} />;
  }
  render(<Editor />);
  fireEvent.click(screen.getByRole("button", { name: "Refresh functions" }));
  fireEvent.change(screen.getAllByLabelText("Server name")[1], { target: { value: "Edited while discovering" } });
  finish({ ok: true, message: "Discovered", tools: [
    { name: "echo", description: "New description", inputSchema: { type: "object", properties: { text: { type: "string" } } }, enabled: true },
    { name: "read", description: "Read", inputSchema: { type: "object" }, enabled: true },
  ] });
  await waitFor(() => expect(latest[0].tools).toHaveLength(2));
  expect(latest[1].name).toBe("Edited while discovering");
  expect(latest[0].tools![0]).toMatchObject({ description: "New description", enabled: false, options: base.tools![0].options });
  expect(screen.getByText(/Connection successful/)).toBeVisible();
  expect(selectableTools({ tools: [], mcpServers: latest })).toEqual([{ value: "mcp://mcp.test/read", label: "Test · read" }]);
});

it("the shared registry supplies every native tool and keeps MCP identifiers in the same selection list", () => {
  const tools = nativeToolDefaults().map(tool => ({ ...tool, enabled: true }));
  const ids = new Set(selectableTools({ tools, mcpServers: [] }).map(tool => tool.value));
  expect([...ids].sort()).toEqual(tools.map(tool => tool.id).sort());
  expect(ids.has("tool.skill")).toBe(true);
  // All defaults are the same values consumed by native Settings validation.
  for (const tool of tools) expect(tool.configuration).toEqual(findNativeTool(tool.id)!.configuration);
});

it("keeps discovered tool overrides when a new package manifest omits its optional tool catalog", () => {
  const original: McpServerConfiguration = { id: "plugin.echo", name: "Echo", enabled: true, autoConnect: false,
    transport: { transport: "http", url: "https://example.com/mcp", headers: [] },
    plugin: { manifestPath: "C:\\Plugins\\echo\\tool-plugin.json", version: "1.0", contentHash: "sha256:old" },
    tools: [{ name: "echo", description: "Echo", inputSchema: { type: "object" }, enabled: false,
      options: { instructions: "Keep my prompt", approvalMode: "ask_for_approval" } }] };
  const updated: McpServerConfiguration = { ...original, tools: [], enabled: false,
    plugin: { ...original.plugin!, version: "2.0", contentHash: "sha256:new" } };
  const snapshot = { toolPluginDirectory: "C:\\Plugins", toolPlugins: [{ path: updated.plugin!.manifestPath, server: updated, error: null }] } as SettingsV2Snapshot;
  const add = vi.fn();
  render(<ToolPluginLibrary snapshot={snapshot} servers={[original]} onAdd={add} onRefresh={async () => {}} />);
  fireEvent.click(screen.getByRole("button", { name: "Load updated plugin" }));
  expect(add).toHaveBeenCalledWith({ ...updated, tools: original.tools });
});
