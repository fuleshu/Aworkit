// @vitest-environment jsdom
import { cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { render } from "../test/renderWithNotifications";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import {
  bundledDefaultWorkflowId,
  bundledWorkflowTemplates,
} from "./bundledWorkflows";
import type { WorkflowLibraryPort } from "./corePort";
import { WorkflowEditorScreen } from "./WorkflowEditorScreen";
import { WorkflowFileDouble } from "../test/workflowFileDouble";
import { WorkflowStore } from "../test/workflowLibraryStore";

afterEach(cleanup);

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

describe("saved-workflow library projection", () => {
  it("names the library dropdown from stored documents after create and a saved name change", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    const files = new WorkflowFileDouble();
    files.savePath = "/tmp/saved-name.aworkit.json";
    render(
      <WorkflowEditorScreen
        document={bundledWorkflowTemplates[0]!.document}
        filePort={files}
        libraryPort={store.libraryPort}
        workflowPort={store.documentPort}
      />,
    );
    const bar = await screen.findByRole("region", { name: "Workflow library" });
    const name = within(bar).getByPlaceholderText("Workflow name");
    await screen.findByRole("heading", { name: starterName });

    // Creating from a template adds a selectable entry immediately.
    await user.type(name, "Research Agent");
    await user.click(within(bar).getByRole("button", { name: "Create" }));
    await waitFor(() =>
      expect(optionLabels(bar)).toContain("Research Agent"),
    );
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
    const files = new WorkflowFileDouble();
    files.savePath = "/tmp/renamed-under-edit.aworkit.json";
    render(
      <WorkflowEditorScreen
        document={bundledWorkflowTemplates[0]!.document}
        filePort={files}
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
    const user = userEvent.setup();
    const store = new WorkflowStore();
    const flaky: WorkflowLibraryPort = {
      ...store.libraryPort,
      snapshot: async () => {
        const snapshot = await store.libraryPort.snapshot();
        if (snapshot.entries.length > 2)
          throw new Error("the workflow library is temporarily unavailable");
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
    expect(optionLabels(bar)).toHaveLength(2);
    await user.type(
      within(bar).getByPlaceholderText("Workflow name"),
      "Research Agent",
    );
    await user.click(within(bar).getByRole("button", { name: "Create" }));
    // The accepted create stands even though the follow-up read failed.
    expect(optionLabels(bar)).toHaveLength(2);
    await screen.findByRole("heading", { name: "Research Agent" });
  });
});
