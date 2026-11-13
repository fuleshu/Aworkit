import { describe, expect, it } from "vitest";
import { parseSettingsV2Snapshot, settingsProblems } from "./configuration";
import { PreviewSettingsV2CorePort } from "./settingsV2Port";

function stdioServer(overrides: Record<string, unknown> = {}) {
  return {
    id: "plugin.rea",
    name: "REA",
    enabled: false,
    autoConnect: false,
    transport: {
      transport: "stdio",
      command: "C:\\tools\\node24\\node.exe",
      args: ["rea.mjs", "--mcp"],
      env: [],
    },
    ...overrides,
  };
}

async function baseSnapshot() {
  return structuredClone(await new PreviewSettingsV2CorePort().snapshot());
}

describe("tolerant settings snapshot parsing", () => {
  it("keeps valid entries and reports the bad one by path", async () => {
    const raw = await baseSnapshot();
    const settings = raw.settings as unknown as Record<string, unknown>;
    settings.mcpServers = [
      stdioServer(),
      stdioServer({
        id: "plugin.broken",
        transport: {
          transport: "stdio",
          command: "node",
          args: [],
          env: [],
          bogusKey: true,
        },
      }),
    ];

    const load = parseSettingsV2Snapshot(raw);

    expect(load.settings.mcpServers.map((server) => server.id)).toEqual(["plugin.rea"]);
    expect(load.problems).toHaveLength(1);
    expect(load.problems[0]?.path).toBe("settings.mcpServers[1]");
    expect(load.problems[0]?.message).toContain("bogusKey");
    // The rest of the document still loads.
    expect(load.version).toBe(raw.version);
    expect(load.settings.appearance.mode).toBe(raw.settings.appearance.mode);
  });

  it("isolates a broken discovered plugin instead of failing the snapshot", async () => {
    const raw = await baseSnapshot();
    const withPlugins = raw as unknown as Record<string, unknown>;
    withPlugins.toolPlugins = [
      { path: "C:\\plugins\\good", server: stdioServer(), error: null },
      {
        path: "C:\\plugins\\bad",
        server: { ...stdioServer(), transport: { transport: "http", url: "" } },
        error: null,
      },
    ];

    const load = parseSettingsV2Snapshot(withPlugins);

    expect(load.toolPlugins).toHaveLength(1);
    expect(load.toolPlugins?.[0]?.path).toBe("C:\\plugins\\good");
    expect(load.problems.map((problem) => problem.path)).toEqual(["toolPlugins[1]"]);
    expect(settingsProblems(load)).toHaveLength(1);
  });

  it("still throws, with the offending path, when the document itself is broken", async () => {
    const raw = await baseSnapshot();
    (raw.settings as unknown as Record<string, unknown>).appearance = { mode: "neon", fontScale: 1 };

    expect(() => parseSettingsV2Snapshot(raw)).toThrow(/settings\.appearance/);
  });

  it("reports a non-array collection instead of dropping the document", async () => {
    const raw = await baseSnapshot();
    (raw.settings as unknown as Record<string, unknown>).providers = { not: "an array" };

    const load = parseSettingsV2Snapshot(raw);

    expect(load.settings.providers).toEqual([]);
    expect(load.problems[0]).toEqual({ path: "settings.providers", message: "must be an array" });
  });
});
