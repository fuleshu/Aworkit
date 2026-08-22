import { describe, expect, it } from "vitest";
import { PreviewSettingsCorePort, PreviewWorkflowCorePort } from "./corePort";
import { parseWorkflow } from "./workflow";

describe("versioned workbench core ports", () => {
  it("commits settings once per stable command and rejects stale versions", async () => {
    const port = new PreviewSettingsCorePort();
    const command = {
      commandId: "settings.1",
      expectedVersion: 3,
      appearance: "dark" as const,
      configuredCapabilities: ["model.local"],
      portableHistoryEnabled: true,
    };
    const first = await port.commit(command);
    const duplicate = await port.commit(command);
    expect(first.currentVersion).toBe(4);
    expect(duplicate).toEqual(first);
    expect(await port.snapshot()).toMatchObject({
      appearance: "dark",
      portableHistoryEnabled: true,
    });
    await expect(
      port.commit({ ...command, appearance: "light" }),
    ).rejects.toThrow("reused with different content");
    await expect(
      port.commit({ ...command, commandId: "settings.2" }),
    ).rejects.toThrow("version conflict");
  });

  it("round-trips unknown workflow fields and fences stale overwrites", async () => {
    const document = parseWorkflow(
      '{"schemaVersion":1,"nodes":[{"id":"a","type":"model","future":{"keep":true}}],"edges":[],"futureRoot":{"keep":true}}',
    );
    const port = new PreviewWorkflowCorePort(document);
    await port.commit({
      commandId: "workflow.1",
      expectedVersion: 1,
      document,
    });
    await expect(
      port.commit({
        commandId: "workflow.1",
        expectedVersion: 1,
        document: { ...document, futureRoot: { keep: false } },
      }),
    ).rejects.toThrow("reused with different content");
    expect((await port.snapshot()).document).toEqual(document);
    await expect(
      port.commit({
        commandId: "workflow.2",
        expectedVersion: 1,
        document,
      }),
    ).rejects.toThrow("version conflict");
  });
});
