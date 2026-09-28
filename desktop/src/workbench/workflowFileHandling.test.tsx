// @vitest-environment jsdom
/**
 * The Workflow designer's file commands.
 *
 * Save writes the open workflow back into its own workflow file; Save As and New
 * only ask for a name and store the result in the Aworkit workflow folder; only
 * Import and Export open an operating-system file dialog. These tests assert
 * exactly that, plus that a refused or invalid operation changes nothing at all.
 */
import { cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { render } from "../test/renderWithNotifications";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { bundledDefaultWorkflowId, bundledWorkflowTemplates } from "./bundledWorkflows";
import { WorkflowEditorScreen } from "./WorkflowEditorScreen";
import { WorkflowFileDouble } from "../test/workflowFileDouble";
import { WorkflowStore } from "../test/workflowLibraryStore";
import {
  suggestedWorkflowFileName,
  workflowFileNameFromPath,
  workflowNameFromPath,
} from "./workflowFilePort";
import { parseWorkflow, serializeWorkflow, type WorkflowDocument } from "./workflow";

afterEach(cleanup);

// jsdom does not run a real modal, so the dialog only has to be visible.
beforeAll(() => {
  HTMLDialogElement.prototype.showModal = function showModal() {
    this.setAttribute("open", "");
  };
});

const starterName =
  bundledWorkflowTemplates.find(
    ({ workflowId }) => workflowId === bundledDefaultWorkflowId,
  )?.name ?? "";

/** A library entry's version, which only a core-accepted save advances. */
async function versions(store: WorkflowStore): Promise<Record<string, number>> {
  const snapshot = await store.libraryPort.snapshot();
  return Object.fromEntries(snapshot.entries.map((entry) => [entry.id, entry.version]));
}

function libraryBar(): HTMLElement {
  return screen.getByRole("region", { name: "Workflow library" });
}

function optionLabels(): readonly string[] {
  const select = within(libraryBar()).getByRole("combobox", {
    name: "Workflow",
  }) as HTMLSelectElement;
  return [...select.options].map((option) =>
    (option.textContent ?? "").replace(" (default)", ""),
  );
}

function editorSurface(
  store: WorkflowStore,
  files: WorkflowFileDouble,
): React.JSX.Element {
  return (
    <WorkflowEditorScreen
      document={bundledWorkflowTemplates[0]!.document}
      filePort={files}
      libraryPort={store.libraryPort}
      workflowPort={store.documentPort}
    />
  );
}

describe("workflow file commands", () => {
  it("saves the open workflow back into its own workflow file without any dialog", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    const files = new WorkflowFileDouble();
    render(editorSurface(store, files));
    await screen.findByRole("heading", { name: starterName });

    await user.click(screen.getByRole("button", { name: "Add Tool node" }));
    expect(screen.getByText("Unsaved changes")).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Save" }));

    // Save never asks for a path and never writes a file of its own.
    expect(files.importCalls).toBe(0);
    expect(files.exportCalls).toBe(0);
    expect(files.writes).toHaveLength(0);
    await waitFor(() => expect(screen.getByText("✓ Draft saved")).toBeVisible());
    await waitFor(() =>
      expect(
        store.document(bundledDefaultWorkflowId)?.nodes.some(
          (node) => node.type === "tool",
        ),
      ).toBe(true),
    );
    // The active workflow and the dropdown are exactly as they were.
    expect(screen.getByRole("heading", { name: starterName })).toBeVisible();
    expect(
      within(libraryBar()).getByRole("combobox", { name: "Workflow" }),
    ).toHaveValue(bundledDefaultWorkflowId);

    // A workflow that was saved survives switching away and back.
    await user.selectOptions(
      within(libraryBar()).getByRole("combobox", { name: "Workflow" }),
      "workflow.simple-chat",
    );
    await screen.findByRole("heading", { name: "Simple Chat" });
    await user.selectOptions(
      within(libraryBar()).getByRole("combobox", { name: "Workflow" }),
      bundledDefaultWorkflowId,
    );
    await screen.findByRole("heading", { name: starterName });
    expect(screen.getByRole("button", { name: "Add Tool node" })).toBeEnabled();
    expect(screen.getByText("✓ Draft saved")).toBeVisible();
  });

  it("saves a copy under a new name that becomes active and listed everywhere", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    const files = new WorkflowFileDouble();
    render(editorSurface(store, files));
    await screen.findByRole("heading", { name: starterName });

    await user.click(screen.getByRole("button", { name: "Add Tool node" }));
    await user.click(screen.getByRole("button", { name: "Save As" }));

    const name = screen.getByRole("textbox", { name: "Save as workflow name" });
    expect(name).toHaveValue(starterName);
    await user.clear(name);
    await user.type(name, "Research Agent");
    await user.click(screen.getByRole("button", { name: "Save copy" }));

    // The copy is the active workflow, appears in the dropdown, and holds the
    // document the editor was showing, unsaved edits included.
    await screen.findByRole("heading", { name: "Research Agent" });
    expect(screen.getByText("✓ Draft saved")).toBeVisible();
    expect(optionLabels()).toContain("Research Agent");
    expect(optionLabels()).toContain(starterName);
    const selector = within(libraryBar()).getByRole("combobox", {
      name: "Workflow",
    }) as HTMLSelectElement;
    expect(selector.selectedOptions[0]?.textContent).toBe("Research Agent");
    const copy = await store.libraryPort.snapshot();
    const entry = copy.entries.find((candidate) => candidate.name === "Research Agent");
    expect(entry).toBeDefined();
    expect(
      store.document(entry!.id)?.nodes.some((node) => node.type === "tool"),
    ).toBe(true);
    // Save As asks for a name, never for a path.
    expect(files.exportCalls).toBe(0);
    expect(files.writes).toHaveLength(0);
    // The original workflow keeps exactly what it had.
    expect(
      store.document(bundledDefaultWorkflowId)?.nodes.some(
        (node) => node.type === "tool",
      ),
    ).toBe(false);
  });

  it("refuses a name another workflow already shows and keeps the dialog open", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    const files = new WorkflowFileDouble();
    render(editorSurface(store, files));
    await screen.findByRole("heading", { name: starterName });
    const before = await store.libraryPort.snapshot();

    await user.click(screen.getByRole("button", { name: "Save As" }));
    const name = screen.getByRole("textbox", { name: "Save as workflow name" });
    await user.clear(name);
    await user.type(name, "simple chat");
    await user.click(screen.getByRole("button", { name: "Save copy" }));

    expect(
      await screen.findByRole("alert"),
    ).toHaveTextContent("a workflow named 'simple chat' already exists");
    // The dialog stays open and nothing was stored or written.
    expect(screen.getByRole("textbox", { name: "Save as workflow name" })).toHaveValue(
      "simple chat",
    );
    expect((await store.libraryPort.snapshot()).entries).toHaveLength(
      before.entries.length,
    );
    expect(screen.getByRole("heading", { name: starterName })).toBeVisible();
    expect(files.writes).toHaveLength(0);
  });

  it("creates a named blank workflow with New that becomes active and listed", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    const files = new WorkflowFileDouble();
    render(editorSurface(store, files));
    await screen.findByRole("heading", { name: starterName });

    await user.click(screen.getByRole("button", { name: "New" }));
    const name = screen.getByRole("textbox", { name: "New workflow name" });
    expect(name).toHaveValue("");
    await user.type(name, "Research Agent");
    await user.click(screen.getByRole("button", { name: "Create" }));

    await screen.findByRole("heading", { name: "Research Agent" });
    expect(optionLabels()).toContain("Research Agent");
    expect(screen.getByText("✓ Draft saved")).toBeVisible();
    // A blank document: no nodes to delete yet.
    expect(screen.queryByRole("button", { name: "Delete node" })).toBeNull();
    expect(files.importCalls).toBe(0);
    expect(files.exportCalls).toBe(0);
    expect(files.writes).toHaveLength(0);
  });

  it("refuses a duplicate New name without creating a workflow", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    render(editorSurface(store, new WorkflowFileDouble()));
    await screen.findByRole("heading", { name: starterName });
    const before = await store.libraryPort.snapshot();

    await user.click(screen.getByRole("button", { name: "New" }));
    await user.type(screen.getByRole("textbox", { name: "New workflow name" }), starterName);
    await user.click(screen.getByRole("button", { name: "Create" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("already exists");
    expect((await store.libraryPort.snapshot()).entries).toHaveLength(
      before.entries.length,
    );
    expect(screen.getByRole("heading", { name: starterName })).toBeVisible();
  });

  it("cancelling the name dialog changes nothing", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    render(editorSurface(store, new WorkflowFileDouble()));
    await screen.findByRole("heading", { name: starterName });
    const before = await store.libraryPort.snapshot();

    await user.click(screen.getByRole("button", { name: "Save As" }));
    await user.click(screen.getByRole("button", { name: "Cancel" }));

    expect(screen.queryByRole("textbox", { name: "Save as workflow name" })).toBeNull();
    expect((await store.libraryPort.snapshot()).entries).toHaveLength(
      before.entries.length,
    );
    expect(screen.getByText("✓ Draft saved")).toBeVisible();
  });

  it("imports a valid file into the workflow folder and activates it", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    const files = new WorkflowFileDouble();
    const path = "/home/user/downloads/repository-engineer.aworkit.json";
    const imported: WorkflowDocument = {
      schemaVersion: 1,
      id: "workflow.repository-engineer",
      name: "Repository Engineer",
      nodes: [
        { id: "input.1", label: "Input", type: "input" },
        { id: "output.1", label: "Output", type: "output" },
      ],
      edges: [{ id: "input-output", source: "input.1", target: "output.1" }],
      futureRoot: { retained: true },
    };
    files.openPath = path;
    files.seed(path, JSON.stringify(imported));
    render(editorSurface(store, files));
    await screen.findByRole("heading", { name: starterName });

    await user.click(screen.getByRole("button", { name: "Import" }));

    expect(files.importCalls).toBe(1);
    await screen.findByRole("heading", { name: "Repository Engineer" });
    expect(optionLabels()).toContain("Repository Engineer");
    expect(optionLabels()).toContain(starterName);
    const snapshot = await store.libraryPort.snapshot();
    const entry = snapshot.entries.find(
      (candidate) => candidate.name === "Repository Engineer",
    );
    expect(entry).toBeDefined();
    // The import copied the document itself, unknown fields and all; only its
    // library identity changed, exactly as the workflow folder requires.
    const stored = store.document(entry!.id);
    expect(stored?.futureRoot).toEqual({ retained: true });
    expect(serializeWorkflow(stored!)).toBe(
      serializeWorkflow({
        ...parseWorkflow(JSON.stringify(imported)),
        id: entry!.id,
      }),
    );
    // The editor shows the imported document as saved, and nothing was written.
    expect(screen.getByText("✓ Draft saved")).toBeVisible();
    expect(files.writes).toHaveLength(0);
  });

  it("names an unnamed imported file after its basename", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    const files = new WorkflowFileDouble();
    const path = "/home/user/downloads/team_draft.aworkit.json";
    const unnamed = {
      ...bundledWorkflowTemplates[2]!.document,
      name: "",
    };
    files.openPath = path;
    files.seed(path, JSON.stringify(unnamed));
    render(editorSurface(store, files));
    await screen.findByRole("heading", { name: starterName });

    await user.click(screen.getByRole("button", { name: "Import" }));

    await screen.findByRole("heading", { name: "team draft" });
    expect(optionLabels()).toContain("team draft");
  });

  it("rejects an import that is not workflow JSON and changes nothing", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    const files = new WorkflowFileDouble();
    const path = "/home/user/downloads/corrupt.aworkit.json";
    files.openPath = path;
    files.seed(path, "{ this is not a workflow document");
    render(editorSurface(store, files));
    await screen.findByRole("heading", { name: starterName });
    const before = await store.libraryPort.snapshot();

    await user.click(screen.getByRole("button", { name: "Import" }));

    expect(
      await screen.findByText(
        /Import failed: corrupt\.aworkit\.json is not a valid workflow JSON document/,
      ),
    ).toBeVisible();
    expect(screen.getByRole("heading", { name: starterName })).toBeVisible();
    expect((await store.libraryPort.snapshot()).entries).toHaveLength(
      before.entries.length,
    );
    expect(files.writes).toHaveLength(0);
  });

  it("rejects an import the workflow folder would not accept and changes nothing", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    const files = new WorkflowFileDouble();
    const path = "/home/user/downloads/broken-transition.aworkit.json";
    const broken = {
      ...bundledWorkflowTemplates[1]!.document,
      name: "Broken transition target",
      edges: [{ id: "input-model", source: "input.1", target: "missing.9" }],
    };
    files.openPath = path;
    files.seed(path, JSON.stringify(broken));
    render(editorSurface(store, files));
    await screen.findByRole("heading", { name: starterName });
    const before = await store.libraryPort.snapshot();

    await user.click(screen.getByRole("button", { name: "Import" }));

    expect(await screen.findByText(/Import failed: .*missing\.9/)).toBeVisible();
    expect(screen.getByRole("heading", { name: starterName })).toBeVisible();
    expect((await store.libraryPort.snapshot()).entries).toHaveLength(
      before.entries.length,
    );
    expect(files.writes).toHaveLength(0);
  });

  it("exports the open workflow to a chosen path and leaves the workflow folder alone", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    const files = new WorkflowFileDouble();
    const path = "/home/user/exports/standard-copy.aworkit.json";
    files.savePath = path;
    render(editorSurface(store, files));
    await screen.findByRole("heading", { name: starterName });
    const before = await store.libraryPort.snapshot();
    const version = await versions(store);

    await user.click(screen.getByRole("button", { name: "Add Tool node" }));
    await user.click(screen.getByRole("button", { name: "Export" }));

    // The export suggested the workflow's own name and wrote the open document,
    // unsaved edits included, while the workflow folder keeps what it had.
    expect(files.exportCalls).toBe(1);
    expect(files.suggestedNames).toEqual([suggestedWorkflowFileName(starterName)]);
    await waitFor(() => expect(files.writes).toHaveLength(1));
    expect(files.writes[0]?.path).toBe(path);
    expect(
      parseWorkflow(files.files.get(path)!).nodes.some((node) => node.type === "tool"),
    ).toBe(true);
    expect(
      store
        .document(bundledDefaultWorkflowId)
        ?.nodes.some((node) => node.type === "tool"),
    ).toBe(false);
    // Nothing about the workflow folder, the active workflow or the draft changed.
    expect((await store.libraryPort.snapshot()).entries).toHaveLength(
      before.entries.length,
    );
    expect(await versions(store)).toEqual(version);
    expect(screen.getByRole("heading", { name: starterName })).toBeVisible();
    expect(screen.getByText("Unsaved changes")).toBeVisible();

    // An existing export target is replaced only after the user confirms.
    files.answer(false);
    await user.click(screen.getByRole("button", { name: "Export" }));
    await waitFor(() => expect(files.confirmations).toHaveLength(1));
    expect(files.confirmations[0]?.body).toContain(path);
    // The refused attempt changed no file: only the first export wrote.
    expect(files.writes.filter((write) => write.overwrite)).toHaveLength(0);
    files.answer(true);
    await user.click(screen.getByRole("button", { name: "Export" }));
    await waitFor(() =>
      expect(files.writes.at(-1)).toMatchObject({ path, overwrite: true }),
    );

    // Cancelling the export browser entirely changes nothing either.
    const writes = files.writes.length;
    files.savePath = null;
    await user.click(screen.getByRole("button", { name: "Export" }));
    await waitFor(() => expect(files.exportCalls).toBe(4));
    expect(files.writes).toHaveLength(writes);
    expect(screen.getByText("Unsaved changes")).toBeVisible();
  });

  it("deletes the active workflow, its JSON document, and selects another workflow", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    const files = new WorkflowFileDouble();
    render(editorSurface(store, files));
    await screen.findByRole("heading", { name: starterName });

    // The default workflow is the active one, so Delete is refused outright.
    const deleteButton = (): HTMLElement =>
      within(libraryBar()).getByRole("button", { name: "Delete" });
    expect(deleteButton()).toBeDisabled();
    expect(deleteButton()).toHaveAttribute(
      "title",
      "The default workflow cannot be deleted",
    );

    // Creating a workflow makes it active, and that one can be deleted.
    await user.click(screen.getByRole("button", { name: "New" }));
    await user.type(
      screen.getByRole("textbox", { name: "New workflow name" }),
      "Disposable",
    );
    await user.click(screen.getByRole("button", { name: "Create" }));
    await screen.findByRole("heading", { name: "Disposable" });
    const created = (await store.libraryPort.snapshot()).entries.find(
      (entry) => entry.name === "Disposable",
    );
    expect(created).toBeDefined();
    expect(deleteButton()).toBeEnabled();

    await user.click(deleteButton());

    // The workflow JSON document is gone, the dropdown no longer lists it, and
    // the editor moved to another workflow instead of pointing at a deleted one.
    await waitFor(() =>
      expect(optionLabels()).not.toContain("Disposable"),
    );
    expect(store.document(created!.id)).toBeNull();
    await screen.findByRole("heading", { name: starterName });
    expect(deleteButton()).toBeDisabled();
  });

  it("asks about unsaved changes before New and Import discard them", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    const files = new WorkflowFileDouble();
    const path = "/home/user/downloads/replacement.aworkit.json";
    files.openPath = path;
    files.seed(
      path,
      JSON.stringify({ ...bundledWorkflowTemplates[2]!.document, name: "Replacement" }),
    );
    render(editorSurface(store, files));
    await screen.findByRole("heading", { name: starterName });

    await user.click(screen.getByRole("button", { name: "Add Tool node" }));
    expect(screen.getByText("Unsaved changes")).toBeVisible();

    // Declining keeps the draft and never opens the file chooser or the dialog.
    files.answer(false);
    await user.click(screen.getByRole("button", { name: "Import" }));
    await waitFor(() => expect(files.confirmations).toHaveLength(1));
    expect(files.confirmations[0]?.body).toContain(starterName);
    expect(files.importCalls).toBe(0);
    expect(screen.getByText("Unsaved changes")).toBeVisible();

    // Declining New keeps the draft as well and opens no name dialog.
    files.answer(false);
    await user.click(screen.getByRole("button", { name: "New" }));
    await waitFor(() => expect(files.confirmations).toHaveLength(2));
    expect(screen.queryByRole("textbox", { name: "New workflow name" })).toBeNull();
    expect(screen.getByText("Unsaved changes")).toBeVisible();

    // Confirming the import copies the chosen file in and activates it.
    files.answer(true);
    await user.click(screen.getByRole("button", { name: "Import" }));
    await screen.findByRole("heading", { name: "Replacement" });
    expect(files.importCalls).toBe(1);
  });
});

describe("workflow file naming", () => {
  it("suggests a file name without ever requiring it to match the workflow name", () => {
    expect(suggestedWorkflowFileName("Repository Engineer")).toBe(
      "repository-engineer.aworkit.json",
    );
    expect(suggestedWorkflowFileName("")).toBe("workflow.aworkit.json");
    expect(workflowFileNameFromPath("/home/user/workflows/team.Workflow.json")).toBe(
      "team.Workflow.json",
    );
    // A basename is only a suggestion for a document that carries no name.
    expect(workflowNameFromPath("/home/user/workflows/team_draft.aworkit.json")).toBe(
      "team draft",
    );
    expect(workflowNameFromPath("/home/user/workflows/.aworkit.json")).toBe(
      "Untitled workflow",
    );
  });
});
