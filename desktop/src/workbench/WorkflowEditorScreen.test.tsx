// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import axe from "axe-core";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  WorkbenchReceipt,
  WorkflowCommit,
  WorkflowCorePort,
  WorkflowSnapshot,
} from "./corePort";
import { WorkflowEditorScreen } from "./WorkflowEditorScreen";
import type { WorkflowDocument } from "./workflow";

afterEach(cleanup);

describe("lossless workflow editor", () => {
  it("creates and deletes nodes and transitions without claiming they can run", async () => {
    const user = userEvent.setup();
    const { container } = render(
      <WorkflowEditorScreen document={simpleChat()} onRun={vi.fn()} />,
    );
    await screen.findByText("Version 1");
    expect(screen.getByRole("button", { name: "Run" })).toBeEnabled();
    const accessibility = await axe.run(container, {
      rules: { "color-contrast": { enabled: false } },
    });
    expect(accessibility.violations).toEqual([]);

    await user.click(screen.getByRole("button", { name: "Add Tool node" }));
    expect(screen.getByText("Editable · Not runnable")).toBeVisible();
    expect(screen.getByRole("button", { name: "Run" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Save" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Delete node" })).toBeEnabled();

    await user.click(screen.getByRole("button", { name: "Delete node" }));
    expect(screen.getByText("Executable workflow")).toBeVisible();
    expect(screen.getByRole("button", { name: "Run" })).toBeEnabled();

    await user.click(screen.getByRole("button", { name: "Add transition" }));
    expect(screen.getByRole("button", { name: "Delete transition" })).toBeEnabled();
    // Extra edges stay executable under the v1 catalog contract as long as
    // the graph remains acyclic and fully reachable, but Run opens a Chat
    // with the saved document, so the dirty graph must be saved first.
    expect(screen.getByRole("button", { name: "Run" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Run" })).toHaveAttribute(
      "title",
      "Save this workflow before starting a Run",
    );
    await user.click(screen.getByRole("button", { name: "Delete transition" }));
    expect(screen.getByRole("button", { name: "Run" })).toBeEnabled();
  });

  it("edits type and configuration as undoable transactions", async () => {
    const user = userEvent.setup();
    render(<WorkflowEditorScreen document={simpleChat()} onRun={vi.fn()} />);
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

  it("does not let Run bypass interrupted-command recovery", async () => {
    const onRun = vi.fn();
    const reason =
      "Resume or abandon the interrupted command before starting another Run";
    render(
      <WorkflowEditorScreen
        document={simpleChat()}
        onRun={onRun}
        runBlockedReason={reason}
      />,
    );
    await screen.findByText("Version 1");
    const run = screen.getByRole("button", { name: "Run" });
    expect(run).toBeDisabled();
    expect(run).toHaveAttribute("title", reason);
    expect(onRun).not.toHaveBeenCalled();
  });

  it("imports exact JSON losslessly, saves it, and gates a richer graph", async () => {
    const user = userEvent.setup();
    const port = new RecordingWorkflowPort(simpleChat());
    render(
      <WorkflowEditorScreen
        document={simpleChat()}
        onRun={vi.fn()}
        workflowPort={port}
      />,
    );
    await screen.findByText("Version 7");
    const exactImport = {
      ...simpleChat(),
      name: "Imported Simple Chat",
      futureRoot: { retained: true },
      nodes: simpleChat().nodes.map((node) =>
        node.id === "agent.1"
          ? { ...node, futureNode: { retained: true } }
          : node,
      ),
    };
    fireEvent.change(screen.getByLabelText("Workflow JSON file"), {
      target: {
        files: [
          new File([JSON.stringify(exactImport)], "exact.json", {
            type: "application/json",
          }),
        ],
      },
    });
    expect(
      await screen.findByRole("heading", { name: "Imported Simple Chat" }),
    ).toBeVisible();
    expect(screen.getByRole("button", { name: "Save" })).toBeEnabled();
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(port.commits).toHaveLength(1));
    expect(port.commits[0]?.document.futureRoot).toEqual({ retained: true });
    expect(port.commits[0]?.document.nodes[1]?.futureNode).toEqual({
      retained: true,
    });

    const richerImport = {
      ...exactImport,
      name: "Advanced Harness",
      nodes: [
        ...exactImport.nodes,
        {
          id: "approval.5",
          type: "approval",
          configuration: { futurePolicy: { retained: true } },
        },
      ],
    };
    fireEvent.change(screen.getByLabelText("Workflow JSON file"), {
      target: {
        files: [
          new File([JSON.stringify(richerImport)], "advanced.json", {
            type: "application/json",
          }),
        ],
      },
    });
    expect(
      await screen.findByRole("heading", { name: "Advanced Harness" }),
    ).toBeVisible();
    expect(screen.getByText("Editable · Not runnable")).toBeVisible();
    expect(screen.getByRole("button", { name: "Save" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Run" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Export" })).toBeEnabled();
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(port.commits).toHaveLength(2));
    expect(port.commits[1]?.document.nodes[4]?.configuration).toEqual({
      futurePolicy: { retained: true },
    });
  });

  it("keeps a malformed imported transition ID editable while gating native Run", async () => {
    const onRun = vi.fn();
    render(<WorkflowEditorScreen document={simpleChat()} onRun={onRun} />);
    await screen.findByText("Version 1");
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

    fireEvent.change(screen.getByLabelText("Workflow JSON file"), {
      target: {
        files: [
          new File([JSON.stringify(malformed)], "malformed-edge.json", {
            type: "application/json",
          }),
        ],
      },
    });

    expect(
      await screen.findByRole("heading", {
        name: "Malformed transition identity",
      }),
    ).toBeVisible();
    expect(screen.getByText("Editable · Not runnable")).toBeVisible();
    expect(
      screen.getAllByText(
        /Every transition ID must be a StableId/,
      ),
    ).not.toHaveLength(0);
    expect(screen.getByRole("button", { name: "Save" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Run" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Export" })).toBeEnabled();
    expect(onRun).not.toHaveBeenCalled();
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
        onRun={vi.fn()}
        workflowPort={port}
      />,
    );
    expect(
      await screen.findByRole("heading", { name: "Future Harness" }),
    ).toBeVisible();
    expect(screen.getByText("Read-only schema")).toBeVisible();
    expect(screen.getByLabelText("Workflow name")).toBeDisabled();
    expect(screen.getByRole("button", { name: "Add Tool node" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Run" })).toBeDisabled();
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
          maxTurns: 1,
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
