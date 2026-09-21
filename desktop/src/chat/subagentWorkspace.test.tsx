// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ChatWorkspaceScreen } from "./ChatWorkspaceScreen";

// jsdom has no layout, so the real virtualizer would render no rows at all.
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
  ChatEventPage,
  RuntimeEvent,
  RuntimeSnapshot,
  SubagentChildSummary,
} from "./corePort";
import type { ChatProjection } from "./types";

afterEach(cleanup);

const chat: ChatProjection = {
  chatId: "chat.subagents",
  runId: "run.subagents",
  title: "Delegating chat",
  scope: "No project",
  workflowId: "workflow.standard",
  workflowName: "Standard Agent",
  branch: null,
  projectId: null,
  phase: "running",
  lockedWorkflow: true,
  recoveryPending: false,
  queuedInputs: [],
  expectedVersion: 7,
  approvalMode: "ask_for_approval",
};

type Draft = readonly [kind: string, payload: Record<string, unknown>];

/** The canonical feed is contiguous, so sequences come from stream order. */
function stream(drafts: readonly Draft[]): RuntimeEvent[] {
  return drafts.map(([kind, payload], index) => {
    const sequence = index + 1;
    return {
      schemaVersion: 1,
      streamId: chat.chatId,
      branchId: "main",
      sequence,
      eventId: `event.${sequence}`,
      kind,
      spanId: typeof payload.spanId === "string" ? payload.spanId : undefined,
      payload,
    };
  });
}

const delegation: readonly Draft[] = [
  ["message.user", { body: "research VR headsets", createdAt: "1" }],
  [
    "span.started",
    {
      spanId: "span.tool.call.delegate",
      spanKind: "tool_call",
      semanticRole: "tool",
      title: "tool.subagent",
      capabilityId: "tool.subagent",
      callId: "call.delegate",
      status: "running",
      createdAt: "5",
    },
  ],
  [
    "span.completed",
    {
      spanId: "span.tool.call.delegate",
      callId: "call.delegate",
      capabilityId: "tool.subagent",
      status: "completed",
      body: "Subagent started",
      createdAt: "6",
    },
  ],
];

function childDrafts(childId: string): readonly Draft[] {
  return [
    [
      "span.started",
      {
        spanId: `span.run.${childId}`,
        spanKind: "run",
        semanticRole: "run",
        title: "Run",
        status: "running",
        subagentChildId: childId,
        createdAt: "10",
      },
    ],
    [
      "span.started",
      {
        spanId: `span.model.${childId}.1`,
        parentSpanId: `span.run.${childId}`,
        spanKind: "model_call",
        semanticRole: "model_call",
        title: `Model call ${childId}`,
        status: "running",
        subagentChildId: childId,
        createdAt: "11",
        input: { messages: [] },
      },
    ],
    [
      "span.content_delta",
      {
        spanId: `span.model.${childId}.1`,
        channel: "assistant_output",
        append: `answer from ${childId}`,
        body: `answer from ${childId}`,
        status: "running",
        subagentChildId: childId,
        createdAt: "12",
      },
    ],
    [
      "span.completed",
      {
        spanId: `span.model.${childId}.1`,
        status: "completed",
        body: `answer from ${childId}`,
        output: { assistant: `answer from ${childId}` },
        subagentChildId: childId,
        createdAt: "13",
      },
    ],
  ];
}

function summary(
  overrides: Partial<SubagentChildSummary> = {},
): SubagentChildSummary {
  return {
    childId: "child.research",
    kind: "fresh",
    status: "completed",
    running: false,
    depth: 1,
    nodeId: "node.agent",
    parentInvocationId: "invoke.1",
    parentCallId: "call.delegate",
    task: "Research VR headsets",
    contextText: "Prefer vendor documentation",
    finalText: "answer from child.research",
    modelTurns: 1,
    toolCalls: 0,
    inputTokens: 12,
    outputTokens: 4,
    headRevision: 1,
    createdAt: "2026-09-21T10:00:00Z",
    updatedAt: "2026-09-21T10:00:05Z",
    ...overrides,
  };
}

function snapshot(
  subagents: readonly SubagentChildSummary[],
  events: readonly RuntimeEvent[],
): RuntimeSnapshot {
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
    subagents: [...subagents],
  };
}

function port(snapshotFor: () => RuntimeSnapshot): ChatCorePort {
  return {
    async snapshot(): Promise<RuntimeSnapshot> {
      return snapshotFor();
    },
    async command(intent) {
      return {
        commandId: intent.commandId,
        accepted: false,
        currentVersion: 1,
        reason: "unused",
      };
    },
  };
}

/** One child-scoped page as the core returns it (cursor already stepped back). */
function childPage(events: readonly RuntimeEvent[]): ChatEventPage {
  const first = events[0]?.sequence ?? 1;
  const last = events.at(-1)?.sequence ?? first;
  return {
    window: {
      firstSequence: first,
      lastSequence: last,
      headSequence: 13,
      hasMore: first > 1,
      supportingEvents: [],
    },
    events: [...events],
  };
}

const researchOnly = stream([...delegation, ...childDrafts("child.research")]);

describe("subagent tabs in the Chat workspace", () => {
  it("hydrates a child tab whose facts are outside the Chat's recent window", async () => {
    const user = userEvent.setup();
    // The Chat feed only holds the recent parent window; the child's own
    // evidence — including the span records its cards need — is older.
    const recentWindow = stream(delegation);
    const childEvidence = stream(childDrafts("child.research"));
    const subagentEvents = vi.fn(
      async (): Promise<ChatEventPage> => childPage(childEvidence),
    );
    const corePort: ChatCorePort = {
      ...port(() => snapshot([summary()], recentWindow)),
      subagentEvents,
    };
    render(<ChatWorkspaceScreen corePort={corePort} pollIntervalMs={50} />);
    await user.click(
      await screen.findByRole("button", { name: "Open subagent" }),
    );
    // The child scope pulls its own page instead of rendering the final answer
    // as a bare wall of text.
    await waitFor(() => expect(subagentEvents).toHaveBeenCalled());
    expect(
      await screen.findByText("answer from child.research"),
    ).toBeVisible();
    expect(document.querySelector(".subagent-empty")).toBeNull();
    expect(
      document.querySelector(".model-call-block, .subagent-conversation .actor-turn"),
    ).not.toBeNull();
  });

  it("re-renders a remembered child tab after leaving and returning to the Chat", async () => {
    const user = userEvent.setup();
    const corePort = port(() => snapshot([summary()], researchOnly));
    const { rerender } = render(
      <ChatWorkspaceScreen corePort={corePort} pollIntervalMs={50} active />,
    );
    await user.click(
      await screen.findByRole("button", { name: "Open subagent" }),
    );
    expect(await screen.findByText("answer from child.research")).toBeVisible();
    expect(document.querySelector(".subagent-empty")).toBeNull();

    // Leaving the route keeps the workspace mounted but inactive; returning
    // resynchronizes the projection and rebuilds the remembered tab set.
    rerender(
      <ChatWorkspaceScreen corePort={corePort} pollIntervalMs={50} active={false} />,
    );
    rerender(
      <ChatWorkspaceScreen corePort={corePort} pollIntervalMs={50} active />,
    );
    await waitFor(() =>
      expect(
        screen.getByRole("tab", { name: /Research VR headsets/ }),
      ).toHaveAttribute("aria-selected", "true"),
    );
    expect(await screen.findByText("answer from child.research")).toBeVisible();
    expect(document.querySelector(".subagent-empty")).toBeNull();
  });

  it("opens a child tab from the delegating tool block and renders it read-only", async () => {
    const user = userEvent.setup();
    render(
      <ChatWorkspaceScreen
        corePort={port(() => snapshot([summary()], researchOnly))}
        pollIntervalMs={50}
      />,
    );
    expect(
      await screen.findByRole("heading", { name: "Delegating chat" }),
    ).toBeVisible();
    // No child tab until the delegating tool block asks for one.
    expect(
      screen.queryByRole("tab", { name: /Research VR headsets/ }),
    ).toBeNull();
    await user.click(
      await screen.findByRole("button", { name: "Open subagent" }),
    );
    const childTab = await screen.findByRole("tab", {
      name: /Research VR headsets/,
    });
    expect(childTab).toHaveAttribute("aria-selected", "true");
    expect(
      screen.getByRole("heading", { name: "Research VR headsets" }),
    ).toBeVisible();
    expect(screen.getByText(/delegated by/)).toBeVisible();
    expect(screen.getByText(/Read-only/)).toBeVisible();
    expect(screen.getByText("answer from child.research")).toBeVisible();
    // A child tab never offers a composer or steering.
    expect(screen.queryByLabelText("Chat composer")).toBeNull();
    expect(screen.queryByLabelText("Chat input")).toBeNull();
    // Returning to the parent restores the composer.
    await user.click(screen.getByRole("tab", { name: "Chat" }));
    expect(await screen.findByLabelText("Chat composer")).toBeVisible();
  });

  it("filters the child tab to its own evidence", async () => {
    const user = userEvent.setup();
    const events = stream([
      ...delegation,
      ...childDrafts("child.research"),
      ...childDrafts("child.other"),
    ]);
    render(
      <ChatWorkspaceScreen
        corePort={port(() => snapshot([summary()], events))}
        pollIntervalMs={50}
      />,
    );
    await user.click(
      await screen.findByRole("button", { name: "Open subagent" }),
    );
    expect(screen.getByText("answer from child.research")).toBeVisible();
    expect(screen.queryByText("answer from child.other")).toBeNull();
    // Parent-only activity never leaks into a child scope.
    expect(screen.queryByText("research VR headsets")).toBeNull();
  });

  it("lists children running-first in the composer dialog", async () => {
    const user = userEvent.setup();
    render(
      <ChatWorkspaceScreen
        corePort={port(() =>
          snapshot(
            [
              summary(),
              summary({
                childId: "child.live",
                task: "Watch the logs",
                status: "running",
                running: true,
                parentCallId: "call.other",
                updatedAt: "2026-09-21T09:00:00Z",
              }),
            ],
            researchOnly,
          ),
        )}
        pollIntervalMs={50}
      />,
    );
    const trigger = await screen.findByLabelText(
      "2 subagent(s) in this Chat, 1 running",
    );
    await user.click(trigger);
    const options = screen.getAllByTitle(/Open the tab for subagent/);
    expect(options[0]).toHaveAttribute(
      "title",
      "Open the tab for subagent child.live",
    );
  });

  it("auto-opens a newly created child without switching the active tab", async () => {
    let created = false;
    render(
      <ChatWorkspaceScreen
        corePort={port(() =>
          created
            ? snapshot([summary()], researchOnly)
            : snapshot([], stream(delegation)),
        )}
        pollIntervalMs={20}
      />,
    );
    expect(
      await screen.findByRole("heading", { name: "Delegating chat" }),
    ).toBeVisible();
    created = true;
    const childTab = await screen.findByRole("tab", {
      name: /Research VR headsets/,
    });
    // The background tab exists but the parent stays active.
    expect(childTab).toHaveAttribute("aria-selected", "false");
    expect(screen.getByRole("tab", { name: "Chat" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(screen.getByLabelText("Chat composer")).toBeVisible();
  });

  it("auto-closes a settled child's inactive tab only when the preference is on", async () => {
    let stage = 0;
    const corePort = port(() => {
      if (stage === 0) return snapshot([], stream(delegation));
      if (stage === 1)
        return snapshot(
          [summary({ status: "running", running: true })],
          researchOnly,
        );
      return snapshot([summary()], researchOnly);
    });
    render(
      <ChatWorkspaceScreen
        corePort={corePort}
        pollIntervalMs={20}
        subagentView={{ autoOpen: true, autoClose: true }}
      />,
    );
    await screen.findByRole("heading", { name: "Delegating chat" });
    stage = 1;
    const childTab = await screen.findByRole("tab", {
      name: /Research VR headsets/,
    });
    expect(childTab).toHaveAttribute("aria-selected", "false");
    stage = 2;
    await waitFor(() =>
      expect(
        screen.queryByRole("tab", { name: /Research VR headsets/ }),
      ).toBeNull(),
    );
    // The parent tab is permanent and the composer is available again.
    expect(screen.getByRole("tab", { name: "Chat" })).toBeVisible();
    expect(screen.getByLabelText("Chat composer")).toBeVisible();
  });

  it("keeps a settled child's tab while the auto-close preference is off", async () => {
    let stage = 0;
    render(
      <ChatWorkspaceScreen
        corePort={port(() => {
          if (stage === 0) return snapshot([], stream(delegation));
          if (stage === 1)
            return snapshot(
              [summary({ status: "running", running: true })],
              researchOnly,
            );
          return snapshot([summary()], researchOnly);
        })}
        pollIntervalMs={20}
        subagentView={{ autoOpen: true, autoClose: false }}
      />,
    );
    await screen.findByRole("heading", { name: "Delegating chat" });
    stage = 1;
    await screen.findByRole("tab", { name: /Research VR headsets/ });
    stage = 2;
    await waitFor(() =>
      expect(
        screen.getByLabelText("1 subagent(s) in this Chat, 0 running"),
      ).toBeVisible(),
    );
    expect(
      screen.getByRole("tab", { name: /Research VR headsets/ }),
    ).toBeVisible();
  });
});

