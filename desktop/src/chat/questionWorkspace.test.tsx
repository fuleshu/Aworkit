// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { ChatWorkspaceScreen } from "./ChatWorkspaceScreen";

// jsdom has no layout, so the real virtualizer would render no timeline rows.
const { virtualizerMeasure, virtualizerResizeItem } = vi.hoisted(() => ({
  virtualizerMeasure: vi.fn(),
  virtualizerResizeItem: vi.fn(),
}));

vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: ({ count }: { readonly count: number }) => ({
    getTotalSize: () => count * 100,
    getVirtualItems: () =>
      Array.from({ length: count }, (_, index) => ({
        index,
        key: index,
        start: index * 100,
      })),
    measureElement: () => undefined,
    measure: virtualizerMeasure,
    resizeItem: virtualizerResizeItem,
    scrollToIndex: () => undefined,
  }),
}));
import type {
  ChatCorePort,
  RuntimeEvent,
  RuntimeSnapshot,
} from "./corePort";
import type { ChatIntent } from "./types";

beforeAll(() => {
  HTMLDialogElement.prototype.showModal = function showModal() {
    this.setAttribute("open", "");
  };
});

afterEach(cleanup);

const chat = {
  approvalMode: "ask_for_approval" as const,
  chatId: "chat.question",
  runId: "run.question",
  title: "Asking chat",
  scope: "No project",
  workflowId: "workflow.standard",
  workflowName: "Standard Agent",
  branch: null,
  projectId: null,
  phase: "awaiting_answer" as const,
  lockedWorkflow: true,
  recoveryPending: false,
  queuedInputs: [],
  expectedVersion: 4,
};

const event = (
  sequence: number,
  kind: string,
  payload: Record<string, unknown>,
): RuntimeEvent => ({
  schemaVersion: 1,
  streamId: chat.chatId,
  branchId: "main",
  sequence,
  eventId: `event.${sequence}`,
  kind,
  spanId: typeof payload.spanId === "string" ? payload.spanId : undefined,
  payload,
});

const asked = [
  event(1, "message.user", { body: "Ship the release", createdAt: "1" }),
  event(2, "question.asked", {
    createdAt: "2",
    commandId: "command.start",
    questionId: "question.release-channel",
    nodeId: "agent.1",
    title: "Release channel",
    prompt: "Which release channel should this build target?",
    kind: "choice",
    options: [
      { id: "stable", label: "Stable" },
      { id: "beta", label: "Beta", description: "Early access" },
    ],
    allowFreeText: true,
    defaultOptionId: null,
    frozenContextHash: "sha256:context",
    invocationId: "invoke.question",
  }),
];

function snapshot(events: readonly RuntimeEvent[]): RuntimeSnapshot {
  const through = events.at(-1)?.sequence ?? 1;
  return {
    version: through,
    throughSequence: through,
    reducerVersion: "chat.semantic.reducer.v1",
    stateHash: `sha256:${"0".repeat(64)}`,
    chat: { ...chat, expectedVersion: through },
    history: [],
    projects: [],
    evidence: [],
    events: [...events],
    subagents: [],
  };
}

function port(events: readonly RuntimeEvent[], dispatched: ChatIntent[]): ChatCorePort {
  return {
    async snapshot(): Promise<RuntimeSnapshot> {
      return snapshot(events);
    },
    async command(intent) {
      dispatched.push(intent);
      return {
        commandId: intent.commandId,
        accepted: true,
        currentVersion: 2,
        reason: null,
      };
    },
  };
}

describe("a model question in the Chat workspace", () => {
  it("raises its dialog, keeps a durable card, and sends the typed answer once", async () => {
    const user = userEvent.setup();
    const dispatched: ChatIntent[] = [];
    render(
      <ChatWorkspaceScreen
        corePort={port(asked, dispatched)}
        pollIntervalMs={50}
      />,
    );
    // The dialog opens by itself for a newly asked question.
    const dialog = await screen.findByRole("dialog", { name: "Release channel" });
    expect(dialog).toHaveAttribute("open");
    expect(
      screen.getByText("Which release channel should this build target?"),
    ).toBeVisible();
    // The Run is waiting for the answer, not for ordinary input.
    expect(
      screen.getByRole("button", { name: "Stop response" }),
    ).toBeVisible();

    await user.click(screen.getByRole("radio", { name: /Beta/ }));
    await user.click(screen.getByRole("button", { name: "Submit answer" }));
    await waitFor(() => expect(dispatched).toHaveLength(1));
    expect(dispatched[0]).toMatchObject({
      type: "question",
      targetId: chat.chatId,
      questionId: "question.release-channel",
      optionId: "beta",
    });
    // The durable card stays in the timeline after the dialog closes.
    expect(
      screen.getByRole("article", { name: /Question: Release channel/ }),
    ).toBeVisible();
  });

  it("keeps a dismissed question answerable from its card and never reopens it", async () => {
    const user = userEvent.setup();
    const dispatched: ChatIntent[] = [];
    render(
      <ChatWorkspaceScreen
        corePort={port(asked, dispatched)}
        pollIntervalMs={20}
      />,
    );
    const dialog = await screen.findByRole("dialog", { name: "Release channel" });
    expect(dialog).toHaveAttribute("open");
    await user.click(screen.getByRole("button", { name: "Decide later" }));
    await waitFor(() =>
      expect(
        screen.queryByRole("dialog", { name: "Release channel" }),
      ).toBeNull(),
    );
    // Nothing was answered, and the card is still the way back in.
    expect(dispatched).toHaveLength(0);
    const answer = screen.getByRole("button", { name: "Answer" });
    await user.click(answer);
    expect(
      await screen.findByRole("dialog", { name: "Release channel" }),
    ).toHaveAttribute("open");
  });

  it("shows a path question with the operating system chooser", async () => {
    const user = userEvent.setup();
    const dispatched: ChatIntent[] = [];
    const pickPath = vi.fn().mockResolvedValue("/home/user/reports");
    render(
      <ChatWorkspaceScreen
        corePort={port(
          [
            asked[0],
            event(2, "question.asked", {
              createdAt: "2",
              questionId: "question.folder",
              nodeId: "agent.1",
              title: "Report folder",
              prompt: "Which folder holds the exported reports?",
              kind: "folder",
              options: [],
              allowFreeText: false,
              frozenContextHash: "sha256:context",
              invocationId: "invoke.folder",
            }),
          ],
          dispatched,
        )}
        pollIntervalMs={50}
        pickPath={pickPath}
      />,
    );
    await screen.findByRole("dialog", { name: "Report folder" });
    await user.click(screen.getByRole("button", { name: "Choose folder…" }));
    expect(pickPath).toHaveBeenCalledWith("folder", []);
    await waitFor(() =>
      expect(screen.getByText("/home/user/reports")).toBeVisible(),
    );
    await user.click(screen.getByRole("button", { name: "Submit answer" }));
    await waitFor(() => expect(dispatched).toHaveLength(1));
    expect(dispatched[0]).toMatchObject({
      type: "question",
      questionId: "question.folder",
      path: "/home/user/reports",
    });
  });

  it("narrows a file question to the requested extensions", async () => {
    const user = userEvent.setup();
    const dispatched: ChatIntent[] = [];
    const pickPath = vi.fn().mockResolvedValue("/home/user/export.csv");
    render(
      <ChatWorkspaceScreen
        corePort={port(
          [
            asked[0],
            event(2, "question.asked", {
              createdAt: "2",
              questionId: "question.spreadsheet",
              nodeId: "agent.1",
              title: "Export file",
              prompt: "Which export should I read?",
              kind: "file",
              options: [],
              allowFreeText: false,
              extensions: ["csv", "tsv"],
              invocationId: "invoke.spreadsheet",
            }),
          ],
          dispatched,
        )}
        pollIntervalMs={50}
        pickPath={pickPath}
      />,
    );
    await screen.findByRole("dialog", { name: "Export file" });
    await user.click(screen.getByRole("button", { name: "Choose file…" }));
    expect(pickPath).toHaveBeenCalledWith("file", ["csv", "tsv"]);
    await waitFor(() =>
      expect(screen.getByText("/home/user/export.csv")).toBeVisible(),
    );
    await user.click(screen.getByRole("button", { name: "Submit answer" }));
    await waitFor(() => expect(dispatched).toHaveLength(1));
    expect(dispatched[0]).toMatchObject({
      type: "question",
      questionId: "question.spreadsheet",
      path: "/home/user/export.csv",
    });
  });

  it("closes the dialog once the answer is committed", async () => {
    const user = userEvent.setup();
    const dispatched: ChatIntent[] = [];
    const { rerender } = render(
      <ChatWorkspaceScreen
        corePort={port(asked, dispatched)}
        pollIntervalMs={50}
      />,
    );
    await screen.findByRole("dialog", { name: "Release channel" });
    await user.click(screen.getByRole("button", { name: "Skip" }));
    await waitFor(() => expect(dispatched).toHaveLength(1));
    expect(dispatched[0]).toMatchObject({
      type: "question",
      cancelled: true,
    });
    // The committed answer removes the waiting state and its dialog.
    rerender(
      <ChatWorkspaceScreen
        corePort={port(
          [
            ...asked,
            event(3, "question.cancelled", {
              createdAt: "3",
              questionId: "question.release-channel",
              cancelled: true,
            }),
          ],
          dispatched,
        )}
        pollIntervalMs={50}
      />,
    );
    await waitFor(() =>
      expect(
        screen.queryByRole("dialog", { name: "Release channel" }),
      ).toBeNull(),
    );
    expect(
      screen.getByRole("article", { name: /Question: Release channel/ }),
    ).toBeVisible();
    expect(
      screen.queryByRole("button", { name: "Answer" }),
    ).toBeNull();
  });
});
