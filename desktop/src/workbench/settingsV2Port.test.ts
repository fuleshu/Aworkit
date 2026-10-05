import { afterEach, describe, expect, it, vi } from "vitest";
import { nativeToolDefaults } from "./toolRegistry";
import { TauriSettingsV2CorePort } from "./settingsV2Port";

const native = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn(), unlisten: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: native.listen }));

afterEach(() => {
  native.invoke.mockReset();
  native.listen.mockReset();
  native.unlisten.mockReset();
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

describe("native plugin folder commands", () => {
  it("installs, removes and opens the plugin folder through dedicated commands", async () => {
    native.invoke.mockResolvedValue(preUpgradeProjection());
    const port = new TauriSettingsV2CorePort();
    await port.installToolPlugin("/tmp/plugins/example");
    expect(native.invoke).toHaveBeenLastCalledWith(
      "settings_v2_install_tool_plugin",
      { path: "/tmp/plugins/example" },
    );
    await port.removeToolPlugin("plugin.example");
    expect(native.invoke).toHaveBeenLastCalledWith(
      "settings_v2_remove_tool_plugin",
      { pluginId: "plugin.example" },
    );
    native.invoke.mockResolvedValue(undefined);
    await port.openToolPluginFolder();
    expect(native.invoke).toHaveBeenLastCalledWith(
      "settings_v2_open_tool_plugin_folder",
    );
  });
});

/** The exact measured projection the trusted core returns for a large store. */
function historyStoreProjection(): Record<string, unknown> {
  return {
    storeBytes: 33_000_000_000,
    payloadBytes: 16_000_000_000,
    snapshotBytes: 15_000_000_000,
    deletedChats: 29,
    deletedChatBytes: 900_000_000,
    outboxRows: 573_690,
    outboxDeliveredRows: 573_178,
    outboxBytes: 16_277_250_048,
    retainedTurns: 20,
    kinds: [
      { kind: "context.checkpoint", events: 7_694, bytes: 7_500_000_000 },
      { kind: "span.started", events: 7_923, bytes: 7_500_000_000 },
    ],
  };
}

function historyReclaimProjection(): Record<string, unknown> {
  return {
    report: {
      streamsScanned: 124,
      payloadsPruned: 15_383,
      payloadBytesReleased: 8_200_000_000,
      chatsPurged: 29,
      eventsRemoved: 52_118,
      outboxRowsRemoved: 573_178,
      outboxBytesReleased: 16_277_250_048,
    },
    storeBytesBefore: 33_000_000_000,
    storeBytesAfter: 21_000_000_000,
  };
}

describe("native history store commands", () => {
  it("measures the store and runs the reclaim through their own commands", async () => {
    const port = new TauriSettingsV2CorePort();
    native.invoke.mockResolvedValueOnce(historyStoreProjection());
    const status = await port.historyStoreStatus();
    expect(native.invoke).toHaveBeenLastCalledWith(
      "desktop_history_store_status",
    );
    expect(status.retainedTurns).toBe(20);
    expect(status.outboxRows).toBe(573_690);
    expect(status.outboxDeliveredRows).toBe(573_178);
    expect(status.outboxBytes).toBe(16_277_250_048);
    expect(status.kinds).toEqual([
      { kind: "context.checkpoint", events: 7_694, bytes: 7_500_000_000 },
      { kind: "span.started", events: 7_923, bytes: 7_500_000_000 },
    ]);

    native.invoke.mockResolvedValueOnce(historyReclaimProjection());
    const outcome = await port.historyReclaim();
    expect(native.invoke).toHaveBeenLastCalledWith("desktop_history_reclaim");
    expect(outcome.report.payloadBytesReleased).toBe(8_200_000_000);
    expect(outcome.report.outboxRowsRemoved).toBe(573_178);
    expect(outcome.report.outboxBytesReleased).toBe(16_277_250_048);
    expect(outcome.storeBytesBefore).toBe(33_000_000_000);
    expect(outcome.storeBytesAfter).toBe(21_000_000_000);
  });

  it("rejects a malformed store projection instead of rendering it", async () => {
    const port = new TauriSettingsV2CorePort();
    native.invoke.mockResolvedValueOnce({
      ...historyStoreProjection(),
      kinds: [{ kind: "context.checkpoint", events: "many", bytes: 1 }],
    });
    await expect(port.historyStoreStatus()).rejects.toThrow();

    native.invoke.mockResolvedValueOnce({
      ...historyStoreProjection(),
      snapshotBytes: -1,
    });
    await expect(port.historyStoreStatus()).rejects.toThrow();

    // The delivery-queue measurement is part of the contract, not optional.
    const withoutOutbox = historyStoreProjection();
    delete withoutOutbox.outboxDeliveredRows;
    native.invoke.mockResolvedValueOnce(withoutOutbox);
    await expect(port.historyStoreStatus()).rejects.toThrow();

    native.invoke.mockResolvedValueOnce({
      ...historyReclaimProjection(),
      report: { streamsScanned: 1 },
    });
    await expect(port.historyReclaim()).rejects.toThrow();

    const impossibleRelease = historyReclaimProjection();
    (impossibleRelease.report as Record<string, unknown>).outboxBytesReleased = -1;
    native.invoke.mockResolvedValueOnce(impossibleRelease);
    await expect(port.historyReclaim()).rejects.toThrow();
  });

  it("reports only well-formed reclaim progress from the native event", async () => {
    native.listen.mockResolvedValue(native.unlisten);
    const received: unknown[] = [];
    const port = new TauriSettingsV2CorePort();
    const dispose = await port.onHistoryReclaimProgress((progress) =>
      received.push(progress),
    );
    expect(native.listen).toHaveBeenCalledWith(
      "aworkit:history-reclaim",
      expect.any(Function),
    );
    expect(dispose).toBe(native.unlisten);
    const emit = native.listen.mock.calls[0]![1] as (event: {
      payload: unknown;
    }) => void;
    emit({ payload: { phase: "releasing", done: 42, total: 153 } });
    emit({ payload: { phase: "rewriting", done: 1, total: 1 } });
    emit({ payload: { phase: "deleting", done: 1, total: 1 } });
    expect(received).toEqual([
      { phase: "releasing", done: 42, total: 153 },
      { phase: "rewriting", done: 1, total: 1 },
    ]);
  });
});
