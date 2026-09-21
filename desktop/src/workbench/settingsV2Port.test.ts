import { afterEach, describe, expect, it, vi } from "vitest";
import { nativeToolDefaults } from "./toolRegistry";
import { TauriSettingsV2CorePort } from "./settingsV2Port";

const native = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));

afterEach(() => {
  native.invoke.mockReset();
});

/**
 * The exact projection the trusted core returns for a profile whose stored
 * subagent contract predates the inherited-tool field. Reading it must succeed;
 * Settings cannot render anything without this snapshot.
 */
function preUpgradeProjection(): unknown {
  const tools = nativeToolDefaults();
  const subagent = tools.find((tool) => tool.id === "tool.subagent")!;
  // Every configuration field a newer build added to the frozen subagent
  // contract: the pre-upgrade document carries none of them.
  for (const key of ["inheritParentTools", "maximumDepth", "maximumChildren", "runInBackground"]) {
    delete subagent.configuration[key];
  }
  return {
    toolPluginDirectory: "C:/profile/tool-plugins",
    toolPlugins: [],
    version: 12,
    schemaVersion: 2,
    settings: {
      approvals: { defaultMode: "ask_for_approval" },
      schemaVersion: 2,
      providers: [],
      modelTiers: ["fast", "simple", "balanced", "quality"].map((name) => ({
        id: `tier:${name}`,
        name,
        kind: "standard",
        resolution: { strategy: "unconfigured" },
      })),
      credentials: [],
      tools,
      extensions: [],
      mcpServers: [],
      externalAgents: [],
      data: {
        portableHistoryEnabled: false,
        detailedCaptureEnabled: false,
        portableDirectory: ".aworkit/sessions",
      },
      projects: [],
      appearance: { mode: "system", fontScale: 1 },
      chatDefaults: {},
      layout: {},
    },
    providerHealth: [],
  };
}

describe("native Settings v2 read", () => {
  it("reads a pre-upgrade tool contract instead of failing the whole snapshot", async () => {
    native.invoke.mockResolvedValue(preUpgradeProjection());
    const snapshot = await new TauriSettingsV2CorePort().snapshot();
    expect(native.invoke).toHaveBeenCalledWith("settings_v2_snapshot");
    expect(
      snapshot.settings.tools.find((tool) => tool.id === "tool.subagent")
        ?.configuration,
    ).toEqual({ authorityMode: "run_subagent", requiresApproval: true });
  });

  it("still rejects a projection the trusted core would refuse to persist", async () => {
    const projection = preUpgradeProjection() as {
      settings: { tools: { id: string; configuration: Record<string, unknown> }[] };
    };
    projection.settings.tools.find(
      (tool) => tool.id === "tool.subagent",
    )!.configuration.inheritParentTools = false;
    native.invoke.mockResolvedValue(projection);
    await expect(new TauriSettingsV2CorePort().snapshot()).rejects.toThrow();
  });
});
