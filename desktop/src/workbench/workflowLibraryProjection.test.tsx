// @vitest-environment jsdom
import { cleanup, fireEvent, screen, waitFor } from "@testing-library/react";
import { render } from "../test/renderWithNotifications";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import {
  bundledDefaultWorkflowId,
  bundledWorkflowTemplates,
} from "./bundledWorkflows";
import type { WorkflowLibraryPort } from "./corePort";
import { WorkflowEditorScreen } from "./WorkflowEditorScreen";
import { WorkflowFileDouble } from "../test/workflowFileDouble";
import { WorkflowStore } from "../test/workflowLibraryStore";

afterEach(cleanup);

// jsdom does not run a real modal, so the dialog only has to be visible.
beforeAll(() => {
  HTMLDialogElement.prototype.showModal = function showModal() {
    this.setAttribute("open", "");
  };
});

/** Library dropdown labels without the default marker the bar appends. */
function optionLabels(bar: HTMLElement): readonly string[] {
  return [...bar.querySelectorAll("select")[0]!.options].map((option) =>
    (option.textContent ?? "").replace(/ \(default\)$/u, ""),
  );
}

const starterName =
  bundledWorkflowTemplates.find(
    ({ workflowId }) => workflowId === bundledDefaultWorkflowId,
  )?.name ?? "";

/** Creates a workflow through the same New dialog the user sees. */
async function createWorkflow(name: string): Promise<void> {
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "New" }));
  await user.type(screen.getByRole("textbox", { name: "New workflow name" }), name);
  await user.click(screen.getByRole("button", { name: "Create" }));
}

describe("saved-workflow library projection", () => {
  it("names the library dropdown from stored documents after New and a saved name change", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    render(
      <WorkflowEditorScreen
        document={bundledWorkflowTemplates[0]!.document}
        filePort={new WorkflowFileDouble()}
        libraryPort={store.libraryPort}
        workflowPort={store.documentPort}
      />,
    );
    const bar = await screen.findByRole("region", { name: "Workflow library" });
    await screen.findByRole("heading", { name: starterName });

    // New stores a workflow in the folder and lists it immediately.
    await createWorkflow("Research Agent");
    await waitFor(() => expect(optionLabels(bar)).toContain("Research Agent"));
    await screen.findByRole("heading", { name: "Research Agent" });

    // This strip has no Rename control: the workflow's own Name property
    // renames the stored entry, and saving republishes the library label.
    fireEvent.change(screen.getByLabelText("Workflow name"), {
      target: { value: "Saved Name" },
    });
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(optionLabels(bar)).toContain("Saved Name"));
    expect(optionLabels(bar)).not.toContain("Research Agent");
    await screen.findByRole("heading", { name: "Saved Name" });
  });

  it("keeps unsaved edits usable while the Name property renames the workflow", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    render(
      <WorkflowEditorScreen
        document={bundledWorkflowTemplates[0]!.document}
        filePort={new WorkflowFileDouble()}
        libraryPort={store.libraryPort}
        workflowPort={store.documentPort}
      />,
    );
    const bar = await screen.findByRole("region", { name: "Workflow library" });
    await screen.findByRole("heading", { name: starterName });

    // An unsaved document edit, an unsaved name change, and an unsaved
    // structural edit.
    fireEvent.change(screen.getByLabelText("Comments"), {
      target: { value: "keep me" },
    });
    fireEvent.change(screen.getByLabelText("Workflow name"), {
      target: { value: "Renamed Under Edit" },
    });
    await user.click(screen.getByRole("button", { name: "Add Tool node" }));

    // The draft survives the rename: the toolbar already names the renamed
    // workflow, its document edits are still unsaved, and Undo still applies.
    expect(screen.getByRole("button", { name: /Undo/ })).toBeEnabled();
    expect(screen.getByText(/Editable · Not runnable/)).toBeVisible();
    const save = screen.getByRole("button", { name: "Save" });
    await waitFor(() => expect(save).toBeEnabled());
    await user.click(save);
    await waitFor(() => expect(optionLabels(bar)).toContain("Renamed Under Edit"));
    const saved = await store.documentPort.snapshot(bundledDefaultWorkflowId);
    expect(
      saved.document.nodes.some((node) => node.type === "tool"),
    ).toBe(true);
    expect(saved.document.comments).toBe("keep me");
    expect(saved.document.name).toBe("Renamed Under Edit");
    expect(optionLabels(bar)).not.toContain(starterName);
  });

  it("keeps the last library projection when the store accepts but reread fails", async () => {
    const store = new WorkflowStore();
    // The bundle seeds more than one workflow; measure whatever it ships
    // instead of assuming a fixed library size.
    const baseline = (await store.libraryPort.snapshot()).entries.length;
    const flaky: WorkflowLibraryPort = {
      ...store.libraryPort,
      snapshot: async () => {
        const snapshot = await store.libraryPort.snapshot();
        if (snapshot.entries.length > baseline)
          throw new Error("the workflow folder is temporarily unavailable");
        return snapshot;
      },
    };
    render(
      <WorkflowEditorScreen
        document={bundledWorkflowTemplates[0]!.document}
        libraryPort={flaky}
        workflowPort={store.documentPort}
      />,
    );
    const bar = await screen.findByRole("region", { name: "Workflow library" });
    expect(optionLabels(bar)).toHaveLength(baseline);

    await createWorkflow("Research Agent");

    // The accepted create stands even though the follow-up read failed: the
    // stale projection is kept and the failure is reported instead of hidden.
    expect(optionLabels(bar)).toHaveLength(baseline);
    await screen.findByRole("heading", { name: "Research Agent" });
  });
});
