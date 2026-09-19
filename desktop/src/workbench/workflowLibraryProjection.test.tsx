// @vitest-environment jsdom
import { cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { render } from "../test/renderWithNotifications";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  bundledDefaultWorkflowId,
  bundledWorkflowTemplates,
} from "./bundledWorkflows";
import type {
  WorkbenchReceipt,
  WorkflowCommit,
  WorkflowCorePort,
  WorkflowCreateCommand,
  WorkflowCreateReceipt,
  WorkflowLibraryPort,
  WorkflowLibrarySnapshot,
  WorkflowRenameCommand,
  WorkflowTargetCommand,
} from "./corePort";
import { WorkflowEditorScreen } from "./WorkflowEditorScreen";
import type { WorkflowDocument } from "./workflow";

afterEach(cleanup);

/**
 * Native-shaped double: one document store behind both the saved-workflow
 * library and the workflow document port, so an entry name is always derived
 * from the stored document exactly as the trusted core derives it.
 */
class WorkflowStore {
  private readonly stored = new Map<
    string,
    { readonly document: WorkflowDocument; readonly version: number; readonly editable: boolean }
  >();
  private defaultWorkflowId = bundledDefaultWorkflowId;
  private libraryVersion = 1;
  private customIds = 0;

  public constructor() {
    for (const template of bundledWorkflowTemplates) {
      if (!template.seedOnFreshProfile) continue;
      this.stored.set(template.workflowId, {
        document: structuredClone(template.document),
        version: 1,
        editable: true,
      });
    }
  }

  public readonly libraryPort: WorkflowLibraryPort = {
    snapshot: async (): Promise<WorkflowLibrarySnapshot> => ({
      version: this.libraryVersion,
      defaultWorkflowId: this.defaultWorkflowId,
      entries: [...this.stored].map(([id, entry]) => ({
        id,
        name: displayName(entry.document, id),
        version: entry.version,
        editable: entry.editable,
        default: id === this.defaultWorkflowId,
      })),
    }),
    create: async (command: WorkflowCreateCommand): Promise<WorkflowCreateReceipt> => {
      const template = bundledWorkflowTemplates.find(
        ({ templateId }) => templateId === (command.template ?? ""),
      );
      if (template === undefined)
        throw new Error(`unknown bundled workflow template '${command.template}'`);
      const id = `workflow.custom.${(this.customIds += 1)}`;
      this.stored.set(id, {
        document: { ...structuredClone(template.document), id, name: command.name },
        version: 1,
        editable: true,
      });
      this.libraryVersion += 1;
      return {
        commandId: command.commandId,
        accepted: true,
        currentVersion: 1,
        workflowId: id,
      };
    },
    duplicate: async (command: WorkflowRenameCommand): Promise<WorkflowCreateReceipt> => {
      const source = this.require(command.workflowId);
      const id = `workflow.custom.${(this.customIds += 1)}`;
      this.stored.set(id, {
        document: { ...structuredClone(source.document), id, name: command.name },
        version: 1,
        editable: true,
      });
      this.libraryVersion += 1;
      return {
        commandId: command.commandId,
        accepted: true,
        currentVersion: 1,
        workflowId: id,
      };
    },
    rename: async (command: WorkflowRenameCommand): Promise<WorkbenchReceipt> => {
      const source = this.require(command.workflowId);
      this.stored.set(command.workflowId, {
        ...source,
        document: { ...source.document, name: command.name },
        version: source.version + 1,
      });
      this.libraryVersion += 1;
      return {
        commandId: command.commandId,
        accepted: true,
        currentVersion: source.version + 1,
        reason: null,
      };
    },
    remove: async (command: WorkflowTargetCommand): Promise<WorkbenchReceipt> => {
      if (this.stored.size <= 1)
        throw new Error("at least one workflow must remain in the workflow library");
      this.require(command.workflowId);
      this.stored.delete(command.workflowId);
      if (this.defaultWorkflowId === command.workflowId)
        this.defaultWorkflowId = [...this.stored.keys()][0]!;
      this.libraryVersion += 1;
      return {
        commandId: command.commandId,
        accepted: true,
        currentVersion: this.libraryVersion,
        reason: null,
      };
    },
    setDefault: async (command: WorkflowTargetCommand): Promise<WorkbenchReceipt> => {
      this.require(command.workflowId);
      this.defaultWorkflowId = command.workflowId;
      this.libraryVersion += 1;
      return {
        commandId: command.commandId,
        accepted: true,
        currentVersion: this.libraryVersion,
        reason: null,
      };
    },
  };

  public readonly documentPort: WorkflowCorePort = {
    snapshot: async (workflowId?: string) => {
      const entry = this.require(workflowId ?? this.defaultWorkflowId);
      return {
        version: entry.version,
        document: structuredClone(entry.document),
        editable: entry.editable,
      };
    },
    commit: async (command: WorkflowCommit): Promise<WorkbenchReceipt> => {
      const target = command.workflowId ?? this.defaultWorkflowId;
      const entry = this.require(target);
      if (command.expectedVersion !== entry.version)
        throw new Error(
          `workflow version conflict: expected ${command.expectedVersion}, actual ${entry.version}`,
        );
      this.stored.set(target, {
        ...entry,
        document: structuredClone(command.document),
        version: entry.version + 1,
      });
      return {
        commandId: command.commandId,
        accepted: true,
        currentVersion: entry.version + 1,
        reason: null,
      };
    },
  };

  private require(workflowId: string) {
    const entry = this.stored.get(workflowId);
    if (entry === undefined)
      throw new Error(`workflow '${workflowId}' does not exist in the workflow library`);
    return entry;
  }
}

function displayName(document: WorkflowDocument, fallbackId: string): string {
  return typeof document.name === "string" && document.name.trim() !== ""
    ? document.name
    : fallbackId;
}

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
  it("names the library dropdown from stored documents after create, rename, and save", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    render(
      <WorkflowEditorScreen
        document={bundledWorkflowTemplates[0]!.document}
        libraryPort={store.libraryPort}
        onRun={vi.fn()}
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

    // Renaming republishes the label and adopts the renamed stored document.
    await user.clear(name);
    await user.type(name, "Renamed Agent");
    await user.click(within(bar).getByRole("button", { name: "Rename" }));
    await waitFor(() => expect(optionLabels(bar)).toContain("Renamed Agent"));
    expect(optionLabels(bar)).not.toContain("Research Agent");
    await screen.findByRole("heading", { name: "Renamed Agent" });

    // Editing the document name and saving must rename the library entry too.
    fireEvent.change(screen.getByLabelText("Workflow name"), {
      target: { value: "Saved Name" },
    });
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(optionLabels(bar)).toContain("Saved Name"));
    expect(optionLabels(bar)).not.toContain("Renamed Agent");
  });

  it("keeps unsaved edits usable across a library rename", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    render(
      <WorkflowEditorScreen
        document={bundledWorkflowTemplates[0]!.document}
        libraryPort={store.libraryPort}
        onRun={vi.fn()}
        workflowPort={store.documentPort}
      />,
    );
    const bar = await screen.findByRole("region", { name: "Workflow library" });
    await screen.findByRole("heading", { name: starterName });

    // An unsaved document edit and an unsaved structural edit.
    fireEvent.change(screen.getByLabelText("Comments"), {
      target: { value: "keep me" },
    });
    await user.click(screen.getByRole("button", { name: "Add Tool node" }));

    await user.type(
      within(bar).getByPlaceholderText("Workflow name"),
      "Renamed Under Edit",
    );
    await user.click(within(bar).getByRole("button", { name: "Rename" }));

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
        onRun={vi.fn()}
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
