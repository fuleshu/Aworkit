// @vitest-environment jsdom
import { cleanup, screen, waitFor } from "@testing-library/react";
import { render } from "../test/renderWithNotifications";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import type {
  WorkflowCreateCommand,
  WorkflowCreateReceipt,
  WorkflowLibraryEntry,
  WorkflowLibraryPort,
  WorkflowLibrarySnapshot,
  WorkflowRenameCommand,
  WorkflowTargetCommand,
  WorkbenchReceipt,
} from "../workbench/corePort";
import { ChatWorkspaceScreen } from "./ChatWorkspaceScreen";
import type { ChatCorePort, RuntimeSnapshot } from "./corePort";
import type { ChatIntent, ChatProjection } from "./types";

afterEach(cleanup);

const draftChat: ChatProjection = {
  chatId: "chat.draft",
  runId: "run.draft",
  title: "New Chat",
  scope: "No project",
  workflowId: null,
  workflowName: null,
  branch: null,
  projectId: null,
  phase: "draft",
  lockedWorkflow: false,
  recoveryPending: false,
  queuedInputs: [],
  expectedVersion: 1,
};

const corePort: ChatCorePort = {
  async snapshot(): Promise<RuntimeSnapshot> {
    return {
      version: 1,
      throughSequence: 0,
      reducerVersion: "chat.semantic.reducer.v1",
      stateHash: `sha256:${"0".repeat(64)}`,
      chat: draftChat,
      history: [],
      projects: [],
      evidence: [],
      events: [],
    };
  },
  async command(intent: ChatIntent) {
    return {
      commandId: intent.commandId,
      accepted: true,
      currentVersion: 2,
      reason: null,
    };
  },
};

/** A library whose stored entries change outside this Chat surface. */
class ChangingLibrary implements WorkflowLibraryPort {
  public entries: WorkflowLibraryEntry[] = [
    {
      id: "workflow.simple-chat",
      name: "Simple Chat",
      version: 1,
      editable: true,
      default: true,
    },
  ];
  public reads = 0;
  public failing = false;

  public async snapshot(): Promise<WorkflowLibrarySnapshot> {
    if (this.failing) throw new Error("document store is busy");
    this.reads += 1;
    return {
      version: this.reads,
      defaultWorkflowId: this.entries[0]!.id,
      entries: this.entries.map((entry) => ({ ...entry })),
    };
  }
  public async create(_command: WorkflowCreateCommand): Promise<WorkflowCreateReceipt> {
    throw new Error("not used");
  }
  public async duplicate(_command: WorkflowRenameCommand): Promise<WorkflowCreateReceipt> {
    throw new Error("not used");
  }
  public async rename(_command: WorkflowRenameCommand): Promise<WorkbenchReceipt> {
    throw new Error("not used");
  }
  public async remove(_command: WorkflowTargetCommand): Promise<WorkbenchReceipt> {
    throw new Error("not used");
  }
  public async setDefault(_command: WorkflowTargetCommand): Promise<WorkbenchReceipt> {
    throw new Error("not used");
  }

  public labels(): readonly string[] {
    return [...workflowSelect().options].map((option) => option.textContent);
  }
}

function workflowSelect(): HTMLSelectElement {
  return screen.getByRole("combobox", {
    name: "Workflow for the first Chat input",
  }) as HTMLSelectElement;
}

function createdEntry(name: string, version = 1): WorkflowLibraryEntry {
  return {
    id: "workflow.custom.1",
    name,
    version,
    editable: true,
    default: false,
  };
}

describe("Chat workflow library refresh", () => {
  it("lists a workflow created elsewhere once the Chat surface is re-entered", async () => {
    const user = userEvent.setup();
    const library = new ChangingLibrary();
    const props = { corePort, pollIntervalMs: 60_000, libraryPort: library } as const;
    const rendered = render(<ChatWorkspaceScreen {...props} active />);
    await waitFor(() => expect(workflowSelect()).toHaveValue("workflow.simple-chat"));
    expect(library.labels()).toEqual(["Simple Chat"]);

    // The workflow editor creates this workflow while the Chat stays mounted.
    library.entries = [...library.entries, createdEntry("Research Agent")];
    rendered.rerender(<ChatWorkspaceScreen {...props} active={false} />);
    rendered.rerender(<ChatWorkspaceScreen {...props} active />);

    await waitFor(() => expect(library.labels()).toContain("Research Agent"));
    // The user's own selection survives the refresh.
    expect(workflowSelect()).toHaveValue("workflow.simple-chat");

    // The new workflow is selectable and can start a Chat.
    await user.selectOptions(workflowSelect(), "workflow.custom.1");
    await user.type(screen.getByRole("textbox", { name: "Chat input" }), "research this");
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Send" })).toBeEnabled(),
    );
  });

  it("reloads the library when another surface reports a change", async () => {
    const library = new ChangingLibrary();
    const props = { corePort, pollIntervalMs: 60_000, libraryPort: library } as const;
    const rendered = render(
      <ChatWorkspaceScreen {...props} active libraryRevision={0} />,
    );
    await waitFor(() => expect(workflowSelect()).toHaveValue("workflow.simple-chat"));
    const reads = library.reads;

    library.entries = [...library.entries, createdEntry("Renamed Agent", 2)];
    rendered.rerender(
      <ChatWorkspaceScreen {...props} active libraryRevision={1} />,
    );
    await waitFor(() => expect(library.labels()).toContain("Renamed Agent"));
    expect(library.reads).toBeGreaterThan(reads);
  });

  it("keeps the last list and reports the failure when a library reload fails", async () => {
    const user = userEvent.setup();
    const library = new ChangingLibrary();
    const props = { corePort, pollIntervalMs: 60_000, libraryPort: library } as const;
    const rendered = render(<ChatWorkspaceScreen {...props} active />);
    await waitFor(() => expect(workflowSelect()).toHaveValue("workflow.simple-chat"));

    library.failing = true;
    rendered.rerender(<ChatWorkspaceScreen {...props} active={false} />);
    rendered.rerender(<ChatWorkspaceScreen {...props} active />);

    await user.type(screen.getByRole("textbox", { name: "Chat input" }), "hello");
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Send" })).toHaveAttribute(
        "title",
        expect.stringContaining("Could not load the workflow library"),
      ),
    );
    // The last known options remain usable instead of blanking the dropdown.
    expect(library.labels()).toEqual(["Simple Chat"]);
  });
});
