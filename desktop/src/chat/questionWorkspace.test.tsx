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
import {
  chatProjection,
  chatSnapshot,
  runtimeEvent,
  testPort,
} from "../test/fixtures/chat";

beforeAll(() => {
  HTMLDialogElement.prototype.showModal = function showModal() {
    this.setAttribute("open", "");
  };
});

afterEach(cleanup);

const chat = chatProjection({
  chatId: "chat.question",
  runId: "run.question",
  title: "Asking chat",
  workflowId: "workflow.standard",
  workflowName: "Standard Agent",
  phase: "awaiting_answer",
  lockedWorkflow: true,
});

const event = (
  sequence: number,
  kind: string,
  payload: Record<string, unknown>,
): RuntimeEvent => runtimeEvent(sequence, kind, payload, { streamId: chat.chatId });

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
  return chatSnapshot({ chat, events });
}

function port(events: readonly RuntimeEvent[], dispatched: ChatIntent[]): ChatCorePort {
  return testPort({
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
  });
}

/** True when any alert (dialog error or workspace notice) carries the text. */
function showsAlert(pattern: RegExp): boolean {
  return screen
    .queryAllByRole("alert")
    .some((node) => pattern.test(node.textContent ?? ""));
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

  it("asks for a path through the operating system chooser", async () => {
    const user = userEvent.setup();
    for (const [kind, extensions, chosen] of [
      ["folder", [], "/home/user/reports"],
      ["file", ["csv", "tsv"], "/home/user/export.csv"],
    ] as const) {
      const dispatched: ChatIntent[] = [];
      const pickPath = vi.fn().mockResolvedValue(chosen);
      const { unmount } = render(
        <ChatWorkspaceScreen
          corePort={port(
            [
              asked[0],
              event(2, "question.asked", {
                createdAt: "2",
                questionId: `question.${kind}`,
                nodeId: "agent.1",
                title: `${kind} question`,
                prompt: `Which ${kind} should I use?`,
                kind,
                options: [],
                allowFreeText: false,
                ...(extensions.length === 0 ? {} : { extensions }),
                invocationId: `invoke.${kind}`,
              }),
            ],
            dispatched,
          )}
          pollIntervalMs={50}
          pickPath={pickPath}
        />,
      );
      await screen.findByRole("dialog", { name: `${kind} question` });
      await user.click(
        screen.getByRole("button", {
          name: kind === "folder" ? "Choose folder…" : "Choose file…",
        }),
      );
      expect(pickPath).toHaveBeenCalledWith(kind, extensions);
      await waitFor(() => expect(screen.getByText(chosen)).toBeVisible());
      await user.click(screen.getByRole("button", { name: "Submit answer" }));
      await waitFor(() => expect(dispatched).toHaveLength(1));
      expect(dispatched[0]).toMatchObject({
        type: "question",
        questionId: `question.${kind}`,
        path: chosen,
      });
      unmount();
    }
  });

  it("still sends the answer when the projection is stale, and closes at once", async () => {
    // The reported failure: the answer looked submitted but nothing reached the
    // core. A user decision is addressed to one durable question, so a stale or
    // unhappy projection must not swallow it - the answer is sent anyway.
    const user = userEvent.setup();
    const dispatched: ChatIntent[] = [];
    let snapshots = 0;
    const flaky = testPort({
      async snapshot(): Promise<RuntimeSnapshot> {
        snapshots += 1;
        if (snapshots > 1) throw new Error("projection unavailable");
        return snapshot(asked);
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
    });
    render(<ChatWorkspaceScreen corePort={flaky} pollIntervalMs={20} />);
    await screen.findByRole("dialog", { name: "Release channel" });
    await waitFor(() => expect(showsAlert(/stale/i)).toBe(true));
    await user.click(screen.getByRole("radio", { name: /Beta/ }));
    await user.click(screen.getByRole("button", { name: "Submit answer" }));
    await waitFor(() => expect(dispatched).toHaveLength(1));
    expect(dispatched[0]).toMatchObject({
      type: "question",
      questionId: "question.release-channel",
      optionId: "beta",
    });
    await waitFor(() =>
      expect(
        screen.queryByRole("dialog", { name: "Release channel" }),
      ).toBeNull(),
    );
  });

  it("sends a refused answer again from the card and shows the core's reason", async () => {
    const user = userEvent.setup();
    const dispatched: ChatIntent[] = [];
    const refusing = testPort({
      async snapshot(): Promise<RuntimeSnapshot> {
        return snapshot(asked);
      },
      async command(intent) {
        dispatched.push(intent);
        return {
          commandId: intent.commandId,
          accepted: false,
          currentVersion: 2,
          reason: "desktop version conflict: expected 2, actual 3",
        };
      },
    });
    render(<ChatWorkspaceScreen corePort={refusing} pollIntervalMs={60_000} />);
    await screen.findByRole("dialog", { name: "Release channel" });
    await user.click(screen.getByRole("radio", { name: /Beta/ }));
    await user.click(screen.getByRole("button", { name: "Submit answer" }));
    await waitFor(() => expect(dispatched).toHaveLength(1));
    // The dialog closed immediately, the card is still the way back in.
    await waitFor(() =>
      expect(
        screen.queryByRole("dialog", { name: "Release channel" }),
      ).toBeNull(),
    );
    await user.click(screen.getByRole("button", { name: "Answer" }));
    await screen.findByRole("dialog", { name: "Release channel" });
    // A freshly opened dialog starts from the question's declared default, so
    // the option is chosen again before submitting.
    await user.click(screen.getByRole("radio", { name: /Beta/ }));
    await user.click(screen.getByRole("button", { name: "Submit answer" }));
    await waitFor(() => expect(dispatched).toHaveLength(2));
    expect(dispatched[1]).toMatchObject({
      type: "question",
      questionId: "question.release-channel",
      optionId: "beta",
    });
  });

  it("raises the newest of several unanswered questions and keeps the older one on its card", async () => {
    const user = userEvent.setup();
    const dispatched: ChatIntent[] = [];
    const twoAsked = [
      ...asked,
      event(3, "question.asked", {
        createdAt: "3",
        commandId: "command.start.2",
        questionId: "question.newest",
        nodeId: "agent.1",
        title: "Newest question",
        prompt: "Which of these should I use?",
        kind: "choice",
        options: [{ id: "one", label: "One" }],
        allowFreeText: false,
        invocationId: "invoke.newest",
      }),
    ];
    render(
      <ChatWorkspaceScreen corePort={port(twoAsked, dispatched)} pollIntervalMs={60_000} />,
    );
    // The Run waits on the newest question, so that is the dialog the user sees.
    await screen.findByRole("dialog", { name: "Newest question" });
    expect(
      screen.queryByRole("dialog", { name: "Release channel" }),
    ).toBeNull();
    // Skipping it leaves the older question answerable from its own card.
    await user.click(screen.getByRole("button", { name: "Skip" }));
    await waitFor(() => expect(dispatched).toHaveLength(1));
    expect(dispatched[0]).toMatchObject({
      type: "question",
      questionId: "question.newest",
      cancelled: true,
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
