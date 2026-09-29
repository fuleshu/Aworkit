// @vitest-environment jsdom
import { cleanup, screen, waitFor } from "@testing-library/react";
import { render } from "../test/renderWithNotifications";
import { afterEach, describe, expect, it } from "vitest";
import { ChatComposer } from "./ChatComposer";
import { ChatWorkspaceScreen } from "./ChatWorkspaceScreen";
import type { ChatCorePort, RuntimeSnapshot } from "./corePort";
import type { ChatIntent, ChatProjection } from "./types";
import type {
  WorkflowLibraryEntry,
  WorkflowLibraryPort,
  WorkflowLibrarySnapshot,
  WorkflowSnapshot,
} from "../workbench/corePort";
import { chatSnapshot } from "../test/fixtures/chat";

afterEach(cleanup);

const frozenChat: ChatProjection = {
  chatId: "chat.frozen",
  runId: "run.frozen",
  title: "Used a workflow that changed",
  scope: "No project",
  workflowId: "workflow.custom.1",
  workflowName: "Research Agent",
  branch: null,
  projectId: null,
  phase: "waiting_input",
  lockedWorkflow: true,
  recoveryPending: false,
  queuedInputs: [],
  expectedVersion: 4,
};

/** A library whose only entry is the default and never the frozen workflow. */
function library(
  entries: readonly WorkflowLibraryEntry[] = [
    {
      id: "workflow.standard-agent",
      name: "Standard Agent",
      version: 1,
      editable: true,
      default: true,
    },
  ],
): WorkflowLibraryPort {
  return {
    async snapshot(): Promise<WorkflowLibrarySnapshot> {
      return { version: 1, defaultWorkflowId: entries[0]!.id, entries };
    },
    async create() {
      throw new Error("not used");
    },
    async saveAs() {
      throw new Error("not used");
    },
    async duplicate() {
      throw new Error("not used");
    },
    async rename() {
      throw new Error("not used");
    },
    async remove() {
      throw new Error("not used");
    },
    async setDefault() {
      throw new Error("not used");
    },
  };
}

function workflowSelect(): HTMLSelectElement {
  return screen.getByRole("combobox", {
    name: "Workflow for the first Chat input",
  }) as HTMLSelectElement;
}

function lockedWorkspacePort(): ChatCorePort {
  return {
    async snapshot(): Promise<RuntimeSnapshot> {
      return chatSnapshot({ chat: frozenChat, throughSequence: 0 });
    },
    async command(intent: ChatIntent) {
      return {
        commandId: intent.commandId,
        accepted: true,
        currentVersion: 5,
        reason: null,
      };
    },
  };
}

/** A native workflow port for an entry the library no longer holds. */
const missingWorkflowPort = {
  async snapshot(): Promise<WorkflowSnapshot> {
    return { version: 0, document: null as never, editable: false };
  },
};

describe("frozen workflow presentation", () => {
  it("shows the frozen workflow when its library entry was deleted", () => {
    render(
      <ChatComposer
        chat={frozenChat}
        projects={[]}
        workflows={[{ id: "workflow.standard-agent", name: "Standard Agent" }]}
        defaultWorkflowId="workflow.standard-agent"
        nextCommandId={() => "command.frozen.deleted"}
        pending={false}
        stale={false}
        onSubmit={async () => true}
      />,
    );
    expect(workflowSelect()).toHaveValue("workflow.custom.1");
    expect(
      screen.getByRole("option", { name: "Research Agent" }),
    ).toBeTruthy();
  });

  it("shows the workflow name of that time when its entry was renamed", () => {
    render(
      <ChatComposer
        chat={frozenChat}
        projects={[]}
        workflows={[
          { id: "workflow.standard-agent", name: "Standard Agent" },
          { id: "workflow.custom.1", name: "Renamed workflow" },
        ]}
        defaultWorkflowId="workflow.standard-agent"
        nextCommandId={() => "command.frozen.renamed"}
        pending={false}
        stale={false}
        onSubmit={async () => true}
      />,
    );
    expect(workflowSelect()).toHaveValue("workflow.custom.1");
    // The frozen name wins over the library's current name for that id.
    expect(screen.getByRole("option", { name: "Research Agent" })).toBeTruthy();
    expect(
      screen.queryByRole("option", { name: "Renamed workflow" }),
    ).toBeNull();
  });

  it("keeps a deleted entry from becoming a visible first-input readiness failure", async () => {
    render(
      <ChatWorkspaceScreen
        corePort={lockedWorkspacePort()}
        pollIntervalMs={60_000}
        libraryPort={library()}
        workflowPort={missingWorkflowPort}
        active
      />,
    );
    await waitFor(() => expect(workflowSelect()).toHaveValue("workflow.custom.1"));
    expect(
      screen.getByRole("option", { name: "Research Agent" }),
    ).toBeTruthy();
    expect(workflowSelect()).toBeDisabled();
    expect(
      screen.queryByText(/read-only schema cannot run/i),
    ).toBeNull();
  });
});
