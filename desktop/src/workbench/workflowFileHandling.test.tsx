// @vitest-environment jsdom
/**
 * Standard file handling for one workflow document: New, Open, Save, and Save
 * As. Every operation crosses the injected native file port, so these tests
 * assert what the real desktop build does with operating-system dialogs: which
 * path is chosen, when the user is asked before a file is replaced, and that a
 * refused or invalid file changes nothing at all.
 */
import { cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { render } from "../test/renderWithNotifications";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
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

const starterName =
  bundledWorkflowTemplates.find(
    ({ workflowId }) => workflowId === bundledDefaultWorkflowId,
  )?.name ?? "";

/** A library entry's version, which only a core-accepted save advances. */
async function versions(store: WorkflowStore): Promise<Record<string, number>> {
  const snapshot = await store.libraryPort.snapshot();
  return Object.fromEntries(snapshot.entries.map((entry) => [entry.id, entry.version]));
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

describe("workflow file handling", () => {
  it("loads a file, switches to another workflow, and keeps it when switching back", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    const files = new WorkflowFileDouble();
    const path = "/home/user/workflows/repository-engineer.aworkit.json";
    const loaded: WorkflowDocument = {
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
    files.seed(path, JSON.stringify(loaded));
    render(editorSurface(store, files));
    await screen.findByRole("heading", { name: starterName });

    await user.click(screen.getByRole("button", { name: "Open" }));
    expect(
      await screen.findByRole("heading", { name: "Repository Engineer" }),
    ).toBeVisible();
    // The loaded document is stored in the library entry this editor edits, so
    // it keeps that entry's identity even though the file carried its own.
    await waitFor(async () =>
      expect((await store.documentPort.snapshot("workflow.standard-agent")).document.id).toBe(
        "workflow.standard-agent",
      ),
    );

    // An unsaved edit is committed by Save, which writes the bound file too.
    await user.click(screen.getByRole("button", { name: "Add Tool node" }));
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(files.writes).toHaveLength(1));
    expect(files.writes[0]?.path).toBe(path);

    // Switching to another workflow shows that workflow, not the loaded one.
    const bar = await screen.findByRole("region", { name: "Workflow library" });
    const selector = within(bar).getByRole("combobox", { name: "Workflow" });
    await user.selectOptions(selector, "workflow.simple-chat");
    await screen.findByRole("heading", { name: "Simple Chat" });

    // Switching back shows the same document, complete with the saved edit.
    await user.selectOptions(selector, "workflow.standard-agent");
    expect(
      await screen.findByRole("heading", { name: "Repository Engineer" }),
    ).toBeVisible();
    const stored = store.document("workflow.standard-agent");
    expect(stored?.futureRoot).toEqual({ retained: true });
    expect(stored?.nodes.length).toBe(3);
    expect(files.files.get(path)).toBe(JSON.stringify(stored, null, 2));
    expect(screen.getByText("✓ Draft saved")).toBeVisible();
  });

  it("saves to a path and opens that path into an identical document", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    const files = new WorkflowFileDouble();
    const path = "/home/user/workflows/round-trip.aworkit.json";
    files.savePath = path;
    render(editorSurface(store, files));
    await screen.findByRole("heading", { name: starterName });

    fireEvent.change(screen.getByLabelText("Workflow name"), {
      target: { value: "Round Trip" },
    });
    await user.click(screen.getByRole("button", { name: "Add Tool node" }));
    await user.click(screen.getByRole("button", { name: "Save" }));

    // Save As offered the workflow's own name as the file-name suggestion.
    await waitFor(() => expect(files.writes).toHaveLength(1));
    expect(files.suggestedNames).toEqual(["round-trip.aworkit.json"]);
    const written = files.files.get(path);
    expect(written).toBeDefined();
    const storedBefore = store.document("workflow.standard-agent");
    expect(serializeWorkflow(parseWorkflow(written!))).toBe(
      serializeWorkflow(storedBefore!),
    );
    expect(screen.getByText(/File · round-trip\.aworkit\.json/)).toBeVisible();

    // Opening the same path again activates exactly that document.
    files.openPath = path;
    await user.click(screen.getByRole("button", { name: "Open" }));
    expect(await screen.findByRole("heading", { name: "Round Trip" })).toBeVisible();
    expect(screen.getByRole("button", { name: "Wait for input" })).toBeVisible();
    await waitFor(async () => {
      const reopened = store.document("workflow.standard-agent");
      expect(serializeWorkflow(reopened!)).toBe(serializeWorkflow(storedBefore!));
    });
    // Re-opening the identical document leaves nothing unsaved.
    expect(screen.getByText("✓ Draft saved")).toBeVisible();
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
  });

  it("rebinds later saves to the path Save As chose and confirms before replacing", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    const files = new WorkflowFileDouble();
    const first = "/home/user/workflows/first.aworkit.json";
    const second = "/home/user/workflows/second.aworkit.json";
    render(editorSurface(store, files));
    await screen.findByRole("heading", { name: starterName });

    // Save As writes the complete document to the chosen path and binds it.
    files.savePath = first;
    await user.click(screen.getByRole("button", { name: "Save As" }));
    await waitFor(() => expect(files.writes.map((write) => write.path)).toEqual([first]));

    // A later Save writes the bound file without asking for a path again.
    await user.click(screen.getByRole("button", { name: "Add Tool node" }));
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(files.writes.map((write) => write.path)).toEqual([first, first]),
    );
    expect(files.suggestedNames).toHaveLength(1);

    // Save As to another path rebinds every later save to that path.
    files.savePath = second;
    await user.click(screen.getByRole("button", { name: "Save As" }));
    await waitFor(() =>
      expect(files.writes.map((write) => write.path)).toEqual([first, first, second]),
    );
    await user.click(screen.getByRole("button", { name: "Add Tool node" }));
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(files.writes.map((write) => write.path)).toEqual([
        first,
        first,
        second,
        second,
      ]),
    );
    expect(files.suggestedNames).toHaveLength(2);
    expect(screen.getByText("✓ Draft saved")).toBeVisible();

    // Replacing an existing file is confirmed first, and a refusal changes
    // nothing: not the file, not the workflow library, not the draft.
    const seeded = files.files.get(first);
    const before = await versions(store);
    await user.click(screen.getByRole("button", { name: "Add Tool node" }));
    files.savePath = first;
    files.answer(false);
    await user.click(screen.getByRole("button", { name: "Save As" }));
    await waitFor(() => expect(files.confirmations).toHaveLength(1));
    expect(files.confirmations[0]?.body).toContain(first);
    expect(files.files.get(first)).toBe(seeded);
    expect(await versions(store)).toEqual(before);
    expect(screen.getByText("Unsaved changes")).toBeVisible();

    // Confirming the replacement writes the file and commits the document.
    files.answer(true);
    await user.click(screen.getByRole("button", { name: "Save As" }));
    await waitFor(() =>
      expect(files.writes.at(-1)).toMatchObject({ path: first, overwrite: true }),
    );
    await waitFor(() =>
      expect(files.files.get(first)).toBe(
        JSON.stringify(store.document("workflow.standard-agent"), null, 2),
      ),
    );
    expect(await versions(store)).not.toEqual(before);
  });

  it("leaves the editor and the workflow library untouched when the file is not valid JSON", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    const files = new WorkflowFileDouble();
    const path = "/home/user/workflows/corrupt.aworkit.json";
    files.openPath = path;
    files.seed(path, "{ this is not a workflow document");
    render(editorSurface(store, files));
    await screen.findByRole("heading", { name: starterName });
    const before = await versions(store);

    await user.click(screen.getByRole("button", { name: "Open" }));

    expect(
      await screen.findByText(/Open failed: corrupt\.aworkit\.json is not a valid workflow JSON document/),
    ).toBeVisible();
    // Nothing was activated, stored, written, or bound.
    expect(screen.getByRole("heading", { name: starterName })).toBeVisible();
    expect(screen.getByRole("button", { name: "Wait for input" })).toBeVisible();
    expect(await versions(store)).toEqual(before);
    expect(files.writes).toHaveLength(0);
    expect(screen.getByText("No file")).toBeVisible();
    expect(screen.getByText("✓ Draft saved")).toBeVisible();
  });

  it("asks about unsaved changes before New and Open discard them", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    const files = new WorkflowFileDouble();
    const path = "/home/user/workflows/replacement.aworkit.json";
    files.openPath = path;
    files.seed(
      path,
      JSON.stringify({ ...bundledWorkflowTemplates[2]!.document, name: "Replacement" }),
    );
    render(editorSurface(store, files));
    await screen.findByRole("heading", { name: starterName });

    await user.click(screen.getByRole("button", { name: "Add Tool node" }));
    expect(screen.getByText("Unsaved changes")).toBeVisible();

    // Declining keeps the draft and never even opens the file chooser.
    files.answer(false);
    await user.click(screen.getByRole("button", { name: "Open" }));
    await waitFor(() => expect(files.confirmations).toHaveLength(1));
    expect(files.confirmations[0]?.body).toContain(starterName);
    expect(files.openCalls).toBe(0);
    expect(screen.getByText("Unsaved changes")).toBeVisible();
    expect(screen.getByRole("button", { name: "Delete node" })).toBeEnabled();

    // Confirming asks for a path and activates the chosen file.
    files.answer(true);
    await user.click(screen.getByRole("button", { name: "Open" }));
    expect(
      await screen.findByRole("heading", { name: "Replacement" }),
    ).toBeVisible();
    expect(files.openCalls).toBe(1);

    // New asks the same question, and a refusal changes nothing either.
    await user.click(screen.getByRole("button", { name: "Add Tool node" }));
    files.answer(false);
    await user.click(screen.getByRole("button", { name: "New" }));
    await waitFor(() => expect(files.confirmations).toHaveLength(3));
    expect(files.confirmations[2]?.body).toContain("Replacement");
    expect(screen.getByRole("heading", { name: "Replacement" })).toBeVisible();

    // Confirming starts an unsaved draft with no bound file: Save asks for one.
    files.answer(true);
    await user.click(screen.getByRole("button", { name: "New" }));
    expect(await screen.findByRole("heading", { name: "Blank" })).toBeVisible();
    expect(screen.getByText("No file")).toBeVisible();
    expect(screen.getByText("Unsaved changes")).toBeVisible();
    files.savePath = "/home/user/workflows/new-draft.aworkit.json";
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(files.saveCalls).toBe(1));
    expect(files.files.has("/home/user/workflows/new-draft.aworkit.json")).toBe(true);
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
