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

  it("stores a copy under a new name and refuses a name that is taken", async () => {
    const port = new PreviewWorkflowLibraryPort();
    const document = bundledWorkflowTemplates[0]!.document;

    const saved = await port.saveAs({
      commandId: "workflow.save-as.1",
      name: "Imported Harness",
      document,
    });
    expect(saved.workflowId).toBe("workflow.imported-harness");
    const snapshot = await port.snapshot();
    expect(
      snapshot.entries.some(
        ({ id, name }) => id === saved.workflowId && name === "Imported Harness",
      ),
    ).toBe(true);

    // A name another workflow already shows is refused, in any case.
    await expect(
      port.saveAs({
        commandId: "workflow.save-as.2",
        name: "imported harness",
        document,
      }),
    ).rejects.toThrow("already exists");
    await expect(
      port.create({
        commandId: "workflow.create.1",
        name: "Simple Chat",
        template: "blank",
      }),
    ).rejects.toThrow("already exists");
    // A document this build cannot store is refused as well.
    await expect(
      port.saveAs({
        commandId: "workflow.save-as.3",
        name: "Future Harness",
        document: { ...document, schemaVersion: 2 },
      }),
    ).rejects.toThrow("schema version 1");
  });

  it("refuses to delete the default workflow and the final workflow", async () => {
    const port = new PreviewWorkflowLibraryPort();
    await expect(
      port.remove({
        commandId: "workflow.delete.1",
        workflowId: bundledDefaultWorkflowId,
      }),
    ).rejects.toThrow("default workflow cannot be deleted");

    // The former default can be deleted once another workflow is the default.
    const survivor = "workflow.simple-chat";
    await port.setDefault({
      commandId: "workflow.default.1",
      workflowId: survivor,
    });
    await port.remove({
      commandId: "workflow.delete.2",
      workflowId: bundledDefaultWorkflowId,
    });

    // Reduce the library to exactly the default, whatever the bundle seeds.
    const others = (await port.snapshot()).entries
      .map(({ id }) => id)
      .filter((id) => id !== survivor);
    for (const [index, workflowId] of others.entries()) {
      await port.remove({
        commandId: `workflow.delete.seeded.${index}`,
        workflowId,
      });
    }
    expect((await port.snapshot()).entries.map(({ id }) => id)).toEqual([
      survivor,
    ]);

    // What remains is the default, so it cannot be deleted either.
    await expect(
      port.remove({
        commandId: "workflow.delete.3",
        workflowId: survivor,
      }),
    ).rejects.toThrow("default workflow cannot be deleted");
  });
});
