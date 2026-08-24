import { describe, expect, it } from "vitest";
import { PreviewSettingsCorePort, PreviewWorkflowCorePort } from "./corePort";
import { parseWorkflow } from "./workflow";

describe("versioned workbench core ports", () => {
  it("commits settings once per stable command and rejects stale versions", async () => {
    const port = new PreviewSettingsCorePort();
    const command = {
      commandId: "settings.1",
      expectedVersion: 0,
      appearance: "dark" as const,
      portableHistoryEnabled: true,
      provider: {
        baseUrl: "http://localhost:11434/v1",
        model: "qwen3",
        credentialAction: "replace" as const,
        apiKey: "test-only-key",
      },
    };
    const first = await port.commit(command);
    const duplicate = await port.commit(command);
    expect(first.currentVersion).toBe(1);
    expect(duplicate).toEqual(first);
    expect(await port.snapshot()).toMatchObject({
      appearance: "dark",
      portableHistoryEnabled: true,
      provider: {
        baseUrl: "http://localhost:11434/v1",
        model: "qwen3",
        credentialConfigured: true,
        state: "configured",
      },
    });
    await expect(
      port.commit({ ...command, appearance: "light" }),
    ).rejects.toThrow("reused with different content");
    await expect(
      port.commit({ ...command, commandId: "settings.2" }),
    ).rejects.toThrow("version conflict");
  });

  it("tests provider input honestly in browser Preview without claiming connectivity", async () => {
    const port = new PreviewSettingsCorePort();
    await expect(
      port.testProvider({
        baseUrl: "not a URL",
        model: "model",
        apiKey: null,
        useStoredCredential: false,
      }),
    ).resolves.toMatchObject({ ok: false, model: null });
    await expect(
      port.testProvider({
        baseUrl: "http://localhost:11434/v1",
        model: "qwen3",
        apiKey: null,
        useStoredCredential: false,
      }),
    ).resolves.toEqual({
      ok: false,
      message:
        "Connection testing requires the native desktop runtime; browser Preview did not contact the provider.",
      model: null,
    });
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
    expect(await port.snapshot()).toMatchObject({
      editable: true,
      document,
    });
    await expect(
      port.commit({
        commandId: "workflow.2",
        expectedVersion: 1,
        document,
      }),
    ).rejects.toThrow("version conflict");
  });

  it("keeps future workflow schemas inspectable and read-only in Preview", async () => {
    const future = parseWorkflow(
      '{"schemaVersion":2,"nodes":[{"id":"future.1","type":"future@2","future":{"kept":true}}],"edges":[],"futureRoot":{"kept":true}}',
    );
    const port = new PreviewWorkflowCorePort(future);
    expect(await port.snapshot()).toMatchObject({
      editable: false,
      document: { futureRoot: { kept: true } },
    });
    await expect(
      port.commit({
        commandId: "workflow.future",
        expectedVersion: 1,
        document: future,
      }),
    ).rejects.toThrow("inspectable read-only");
  });
});
