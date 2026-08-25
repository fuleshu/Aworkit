import { describe, expect, it } from "vitest";
import { PreviewWorkflowLibraryPort } from "./corePort";
import {
  bundledDefaultWorkflowId,
  bundledWorkflowTemplates,
} from "./bundledWorkflows";

describe("preview workflow library", () => {
  it("seeds exactly the JSON-declared workflows and default", async () => {
    const port = new PreviewWorkflowLibraryPort();
    const snapshot = await port.snapshot();
    expect(snapshot.defaultWorkflowId).toBe(bundledDefaultWorkflowId);
    expect(snapshot.entries.map(({ id }) => id)).toEqual(
      bundledWorkflowTemplates
        .filter(({ seedOnFreshProfile }) => seedOnFreshProfile)
        .map(({ workflowId }) => workflowId),
    );
    expect(
      snapshot.entries.find(({ id }) => id === bundledDefaultWorkflowId)?.default,
    ).toBe(true);
  });

  it("creates, renames, duplicates, defaults, and deletes workflows", async () => {
    const port = new PreviewWorkflowLibraryPort();
    const created = await port.create({
      commandId: "workflow.create.1",
      name: "My Agent",
      template: "standard-agent",
    });
    expect(created.workflowId).toBe("workflow.my-agent");
    let snapshot = await port.snapshot();
    expect(snapshot.entries.some(({ id }) => id === "workflow.my-agent")).toBe(
      true,
    );

    await port.rename({
      commandId: "workflow.rename.1",
      workflowId: "workflow.my-agent",
      name: "Renamed Agent",
    });
    snapshot = await port.snapshot();
    expect(
      snapshot.entries.find(({ id }) => id === "workflow.my-agent")?.name,
    ).toBe("Renamed Agent");

    const duplicate = await port.duplicate({
      commandId: "workflow.duplicate.1",
      workflowId: "workflow.my-agent",
      name: "Copy",
    });
    expect(duplicate.workflowId).toBe("workflow.copy");

    await port.setDefault({
      commandId: "workflow.default.1",
      workflowId: "workflow.my-agent",
    });
    snapshot = await port.snapshot();
    expect(snapshot.defaultWorkflowId).toBe("workflow.my-agent");
    expect(
      snapshot.entries.find(({ id }) => id === "workflow.my-agent")?.default,
    ).toBe(true);

    await port.remove({
      commandId: "workflow.delete.1",
      workflowId: "workflow.copy",
    });
    snapshot = await port.snapshot();
    expect(snapshot.entries.some(({ id }) => id === "workflow.copy")).toBe(
      false,
    );
  });

  it("refuses to delete the final workflow", async () => {
    const port = new PreviewWorkflowLibraryPort();
    await port.remove({
      commandId: "workflow.delete.1",
      workflowId: "workflow.standard-agent",
    });
    await expect(
      port.remove({
        commandId: "workflow.delete.2",
        workflowId: "workflow.simple-chat",
      }),
    ).rejects.toThrow("at least one workflow");
  });
});
