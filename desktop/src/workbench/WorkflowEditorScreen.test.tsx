// @vitest-environment jsdom
import { cleanup, fireEvent, screen, waitFor } from "@testing-library/react";
import { render } from "../test/renderWithNotifications";
import userEvent from "@testing-library/user-event";
import axe from "axe-core";
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import type {
  WorkbenchReceipt,
  WorkflowCommit,
  WorkflowCorePort,
  WorkflowSnapshot,
} from "./corePort";
import { WorkflowEditorScreen } from "./WorkflowEditorScreen";
import { WorkflowFileDouble } from "../test/workflowFileDouble";
import { WorkflowStore } from "../test/workflowLibraryStore";
import { bundledDefaultWorkflowId, bundledWorkflowTemplates } from "./bundledWorkflows";
import type { WorkflowDocument } from "./workflow";

afterEach(cleanup);

// jsdom does not run a real modal, so the dialog only has to be visible.
beforeAll(() => {
  HTMLDialogElement.prototype.showModal = function showModal() {
    this.setAttribute("open", "");
  };
});

const libraryDefaultName =
  bundledWorkflowTemplates.find(
    ({ workflowId }) => workflowId === bundledDefaultWorkflowId,
  )?.name ?? "";

/** The stored document of the only workflow whose name matches `name`. */
async function storedByName(
  store: WorkflowStore,
  name: string,
): Promise<WorkflowDocument | null> {
  const snapshot = await store.libraryPort.snapshot();
  const entry = snapshot.entries.find((candidate) => candidate.name === name);
  return entry === undefined ? null : store.document(entry.id);
}

describe("lossless workflow editor", () => {
  it("validates and saves an Agent after binding Skills", async () => {
    const user = userEvent.setup();
    const port = new RecordingWorkflowPort(simpleChat());
    const files = new WorkflowFileDouble();
    render(
      <WorkflowEditorScreen
        document={simpleChat()}
        filePort={files}
        workflowPort={port}
      />,
    );
    await screen.findByText("Version 7");
    await user.click(screen.getByRole("button", { name: "Agent" }));
    fireEvent.change(screen.getByLabelText("Configuration JSON"), {
      target: { value: JSON.stringify({ modelTierId: "tier:balanced", toolIds: ["tool.skill"] }) },
    });
    await user.click(screen.getByRole("button", { name: "Apply configuration" }));
    await user.click(screen.getByRole("button", { name: /^Validate/ }));
    expect(screen.getByText("Validation passed: this workflow document is executable.")).toBeVisible();
    expect(screen.queryByText(/no installed executor/)).toBeNull();
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(port.commits).toHaveLength(1));
    expect(port.commits[0]?.document.nodes[1]?.configuration).toMatchObject({ toolIds: ["tool.skill"] });
    // Save writes the open workflow into its own workflow folder entry and
    // never opens a file dialog.
    expect(files.writes).toHaveLength(0);
    expect(files.exportCalls).toBe(0);
    expect(files.importCalls).toBe(0);
    expect(screen.getByText("✓ Draft saved")).toBeVisible();
  });

  it("creates and deletes nodes and transitions without claiming they can run", async () => {
    const user = userEvent.setup();
    const { container } = render(<WorkflowEditorScreen document={simpleChat()} />);
    await screen.findByText("Version 1");
    expect(screen.getByText("✓ Draft saved")).toBeVisible();
    const accessibility = await axe.run(container, {
      rules: { "color-contrast": { enabled: false } },
    });
    expect(accessibility.violations).toEqual([]);

    await user.click(screen.getByRole("button", { name: "Add Tool node" }));
    expect(screen.getByText("Editable · Not runnable")).toBeVisible();
    expect(screen.getByText("Unsaved changes")).toBeVisible();
    expect(screen.getByRole("button", { name: "Save" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Delete node" })).toBeEnabled();

    await user.click(screen.getByRole("button", { name: "Delete node" }));
    expect(screen.getByText("Executable workflow")).toBeVisible();
    expect(screen.getByText("✓ Draft saved")).toBeVisible();

    await user.click(screen.getByRole("button", { name: "Add transition" }));
    expect(screen.getByRole("button", { name: "Delete transition" })).toBeEnabled();
    // Extra edges stay executable under the v1 catalog contract as long as
    // the graph remains acyclic and fully reachable, but the graph is an
    // unsaved draft until Save commits it to the workflow folder.
    expect(screen.getByText("Unsaved changes")).toBeVisible();
    expect(screen.getByText("Executable workflow")).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Delete transition" }));
    expect(screen.getByText("✓ Draft saved")).toBeVisible();
  });

  it("edits type and configuration as undoable transactions", async () => {
    const user = userEvent.setup();
    render(<WorkflowEditorScreen document={simpleChat()} />);
    await screen.findByText("Version 1");
    await user.click(screen.getByRole("button", { name: "Agent" }));

    const type = screen.getByLabelText("Node type");
    expect(type).toBeEnabled();
    fireEvent.change(type, { target: { value: "future_agent" } });
    expect(screen.getByText("Editable · Not runnable")).toBeVisible();
    await user.click(screen.getByRole("button", { name: /Undo/ }));
    expect(type).toHaveValue("agent");

    const configuration = screen.getByLabelText("Configuration JSON");
    fireEvent.change(configuration, {
      target: {
        value:
          '{"modelTierId":"tier:other","future":{"retained":true}}',
      },
    });
    await user.click(
      screen.getByRole("button", { name: "Apply configuration" }),
    );
    expect(screen.getByText("Editable · Not runnable")).toBeVisible();
    await user.click(screen.getByRole("button", { name: /Undo/ }));
    expect(screen.getByText("Executable workflow")).toBeVisible();
  });

  it("offers no Import JSON, Rename, or Run control on this surface", async () => {
    const store = new WorkflowStore();
    render(
      <WorkflowEditorScreen
        document={simpleChat()}
        filePort={new WorkflowFileDouble()}
        libraryPort={store.libraryPort}
        workflowPort={store.documentPort}
      />,
    );
    await screen.findByRole("heading", { name: libraryDefaultName });
    expect(screen.queryByRole("button", { name: "Run" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Rename" })).toBeNull();
    expect(screen.queryByRole("button", { name: /Import JSON/ })).toBeNull();
    expect(screen.queryByLabelText("Workflow JSON file")).toBeNull();
    // The file commands the surface does offer.
    expect(screen.getByRole("button", { name: "New" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Import" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Save As" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Export" })).toBeEnabled();
  });

  it("imports exact JSON losslessly and gates a richer graph", async () => {
    const user = userEvent.setup();
    const store = new WorkflowStore();
    const files = new WorkflowFileDouble();
    const exactPath = "/tmp/exact.aworkit.json";
    const exactOpen = {
      ...simpleChat(),
      name: "Opened Simple Chat",
      futureRoot: { retained: true },
      nodes: simpleChat().nodes.map((node) =>
        node.id === "agent.1"
          ? { ...node, futureNode: { retained: true } }
          : node,
      ),
    };
    files.openPath = exactPath;
    files.seed(exactPath, JSON.stringify(exactOpen));
    render(
      <WorkflowEditorScreen
        document={simpleChat()}
        filePort={files}
        libraryPort={store.libraryPort}
        workflowPort={store.documentPort}
      />,
    );
    await screen.findByRole("heading", { name: libraryDefaultName });
    await user.click(screen.getByRole("button", { name: "Import" }));
    expect(
      await screen.findByRole("heading", { name: "Opened Simple Chat" }),
    ).toBeVisible();
    // Importing copies the file into the workflow folder verbatim: every
    // unknown field survives, and the copy is what the editor now shows.
    const copied = await storedByName(store, "Opened Simple Chat");
    expect(copied?.futureRoot).toEqual({ retained: true });
    expect(copied?.nodes[1]?.futureNode).toEqual({ retained: true });
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Save As" })).toBeEnabled();

    const advancedPath = "/tmp/advanced.aworkit.json";
    const advancedOpen = {
      ...exactOpen,
      name: "Advanced Harness",
      nodes: [
        ...exactOpen.nodes,
        {
          id: "approval.5",
          type: "approval",
          configuration: { futurePolicy: { retained: true } },
        },
      ],
    };
    files.openPath = advancedPath;
    files.seed(advancedPath, JSON.stringify(advancedOpen));
    await user.click(screen.getByRole("button", { name: "Import" }));
    expect(
      await screen.findByRole("heading", { name: "Advanced Harness" }),
    ).toBeVisible();
    expect(screen.getByText("Editable · Not runnable")).toBeVisible();
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Save As" })).toBeEnabled();
    const advanced = await storedByName(store, "Advanced Harness");
    expect(advanced?.nodes[4]?.configuration).toEqual({
      futurePolicy: { retained: true },
    });
  });

  it("keeps a malformed imported transition identity editable while gating native execution", async () => {
    const store = new WorkflowStore();
    const files = new WorkflowFileDouble();
    const malformedPath = "/tmp/malformed-edge.aworkit.json";
    const malformed = {
      ...simpleChat(),
      name: "Malformed transition identity",
      edges: simpleChat().edges.map((edge, index) =>
        index === 0
          ? {
              ...edge,
              id: "not a stable id!",
              futureEdge: { retained: true },
            }
          : edge,
      ),
    };
    files.openPath = malformedPath;
    files.seed(malformedPath, JSON.stringify(malformed));
    render(
      <WorkflowEditorScreen
        document={simpleChat()}
        filePort={files}
        libraryPort={store.libraryPort}
        workflowPort={store.documentPort}
      />,
    );
    await screen.findByRole("heading", { name: libraryDefaultName });

    await userEvent.setup().click(screen.getByRole("button", { name: "Import" }));

    expect(
      await screen.findByRole("heading", {
        name: "Malformed transition identity",
      }),
    ).toBeVisible();
    expect(screen.getByText("Editable · Not runnable")).toBeVisible();
    expect(
      screen.getAllByText(/Every transition ID must be a StableId/),
    ).not.toHaveLength(0);
    // The workflow folder accepted the document, so it stays editable and its
    // unknown fields survive; only native execution is gated.
    const copied = await storedByName(store, "Malformed transition identity");
    expect(copied?.edges[0]?.futureEdge).toEqual({ retained: true });
    expect(screen.getByRole("button", { name: "Save As" })).toBeEnabled();
  });

  it("refuses an import the workflow folder would reject and leaves the editor untouched", async () => {
    const store = new WorkflowStore();
    const files = new WorkflowFileDouble();
    const brokenPath = "/tmp/broken-transition.aworkit.json";
    const broken = {
      ...simpleChat(),
      name: "Broken transition target",
      edges: simpleChat().edges.map((edge, index) =>
        index === 0 ? { ...edge, target: "missing.9" } : edge,
      ),
    };
    files.openPath = brokenPath;
    files.seed(brokenPath, JSON.stringify(broken));
    render(
      <WorkflowEditorScreen
        document={simpleChat()}
        filePort={files}
        libraryPort={store.libraryPort}
        workflowPort={store.documentPort}
      />,
    );
    await screen.findByRole("heading", { name: libraryDefaultName });
    const before = await store.libraryPort.snapshot();

    await userEvent.setup().click(screen.getByRole("button", { name: "Import" }));

    expect(
      await screen.findByText(/Import failed: .*missing\.9/),
    ).toBeVisible();
    // Neither the editor nor the workflow folder changed.
    expect(screen.getByRole("heading", { name: libraryDefaultName })).toBeVisible();
    expect((await store.libraryPort.snapshot()).entries).toHaveLength(
      before.entries.length,
    );
    expect(await storedByName(store, "Broken transition target")).toBeNull();
  });

  it("keeps future workflow schemas inspectable and losslessly read-only", async () => {
    const future: WorkflowDocument = {
      schemaVersion: 2,
      name: "Future Harness",
      nodes: [
        {
          id: "future.1",
          type: "future@2",
          configuration: { newField: { retained: true } },
        },
      ],
      edges: [],
      futureRoot: { retained: true },
    };
    const port = new RecordingWorkflowPort(future, false);
    render(
      <WorkflowEditorScreen
        document={simpleChat()}
        workflowPort={port}
      />,
    );
    expect(
      await screen.findByRole("heading", { name: "Future Harness" }),
    ).toBeVisible();
    expect(screen.getByText("Read-only schema")).toBeVisible();
    expect(screen.getByLabelText("Workflow name")).toBeDisabled();
    expect(screen.getByRole("button", { name: "Add Tool node" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "New" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Import" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Save As" })).toBeDisabled();
    // Exporting an inspectable document is a lossless copy that changes nothing.
    expect(screen.getByRole("button", { name: "Export" })).toBeEnabled();
    expect(screen.getByText(/Complete preserved workflow JSON/)).toBeVisible();
  });
});

class RecordingWorkflowPort implements WorkflowCorePort {
  public readonly commits: WorkflowCommit[] = [];
  public constructor(
    private document: WorkflowDocument,
    private readonly editable = true,
  ) {}

  public async snapshot(): Promise<WorkflowSnapshot> {
    return { version: 7, document: this.document, editable: this.editable };
  }

  public async commit(command: WorkflowCommit): Promise<WorkbenchReceipt> {
    this.commits.push(command);
    this.document = command.document;
    return {
      commandId: command.commandId,
      accepted: true,
      currentVersion: 8,
      reason: null,
    };
  }
}

function simpleChat(): WorkflowDocument {
  return {
    schemaVersion: 1,
    id: "workflow.simple-chat",
    name: "Simple Chat",
    nodes: [
      { id: "input.1", label: "Input", type: "input" },
      {
        id: "agent.1",
        label: "Agent",
        type: "agent",
        configuration: {
          modelTierId: "tier:balanced",
          toolIds: [],
        },
      },
      { id: "output.1", label: "Output", type: "output" },
      { id: "wait.1", label: "Wait for input", type: "wait" },
    ],
    edges: [
      { id: "input-agent", source: "input.1", target: "agent.1" },
      { id: "agent-output", source: "agent.1", target: "output.1" },
      { id: "output-wait", source: "output.1", target: "wait.1" },
    ],
  };
}
