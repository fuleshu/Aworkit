// @vitest-environment jsdom
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ChatComposer } from "./ChatComposer";
import {
  ChatWorkspaceScreen,
  timelineActionIntent,
} from "./ChatWorkspaceScreen";
import { toConversationCard } from "./conversation";
import { ConversationTimeline, TimelineCard } from "./ConversationTimeline";
import { NavigationPane } from "../shell/NavigationPane";
import type { ChatCorePort, RuntimeEvent, RuntimeSnapshot } from "./corePort";
import type { ChatIntent, ChatProjection, CoreEventEnvelope } from "./types";
import type { WorkflowCorePort } from "../workbench/corePort";
import type { WorkflowDocument } from "../workbench/workflow";
import { bundledDefaultWorkflowId } from "../workbench/bundledWorkflows";

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

afterEach(cleanup);

const runningChat: ChatProjection = {
  chatId: "chat.test",
  runId: "run.test",
  title: "Contiguous projection",
  scope: "Test project",
  workflowName: "Test workflow",
  branch: "main",
  projectId: "project.test",
  phase: "running",
  lockedWorkflow: true,
  recoveryPending: false,
  queuedInputs: [],
  expectedVersion: 1,
};

describe("Chat native-port recovery contracts", () => {
  it("remeasures every expanded evidence row together after layout", () => {
    virtualizerMeasure.mockClear();
    virtualizerResizeItem.mockClear();
    const frameCallbacks: FrameRequestCallback[] = [];
    const requestAnimationFrame = vi
      .spyOn(window, "requestAnimationFrame")
      .mockImplementation((callback) => {
        frameCallbacks.push(callback);
        return frameCallbacks.length;
      });
    render(
      <ConversationTimeline
        items={[
          {
            id: "event.chat.1",
            kind: "tool",
            title: "Project file tool",
            body: "Listed files",
            createdAt: "now",
            status: "completed",
            raw: { body: "Listed files" },
          },
          {
            id: "event.chat.2",
            kind: "model",
            title: "Agent",
            body: "Settled",
            createdAt: "now",
            status: "completed",
            raw: { body: "Settled" },
          },
        ]}
        selectedId={null}
        onSelect={() => undefined}
        onAction={() => undefined}
      />,
    );
    while (frameCallbacks.length > 0) frameCallbacks.shift()?.(0);
    virtualizerResizeItem.mockClear();
    const details = screen
      .getAllByText("Inspect source record")
      .map((summary) => summary.closest("details"));
    expect(details).not.toContain(null);
    const rows = details.map((detail) => detail!.closest(".virtual-row")!);
    const firstBounds = vi
      .spyOn(rows[0], "getBoundingClientRect")
      .mockReturnValue({ height: 240 } as DOMRect);
    const secondBounds = vi
      .spyOn(rows[1], "getBoundingClientRect")
      .mockReturnValue({ height: 320 } as DOMRect);
    fireEvent(details[0]!, new Event("toggle"));
    fireEvent(details[1]!, new Event("toggle"));
    expect(virtualizerMeasure).not.toHaveBeenCalled();
    expect(virtualizerResizeItem).not.toHaveBeenCalled();
    frameCallbacks.shift()?.(0);
    expect(virtualizerResizeItem).toHaveBeenCalledTimes(2);
    expect(virtualizerResizeItem).toHaveBeenCalledWith(0, 240);
    expect(virtualizerResizeItem).toHaveBeenCalledWith(1, 320);
    firstBounds.mockRestore();
    secondBounds.mockRestore();
    requestAnimationFrame.mockRestore();
  });

  it("includes the selected saved project only in the first-send intent", async () => {
    const user = userEvent.setup();
    const intents: ChatIntent[] = [];
    render(
      <ChatComposer
        chat={{ ...runningChat, phase: "draft", lockedWorkflow: false, projectId: null }}
        projects={[
          {
            projectId: "project.atlas",
            name: "Project Atlas",
            workspaceKind: "git_worktree",
          },
        ]}
        nextCommandId={() => "command.project.start"}
        pending={false}
        stale={false}
        onSubmit={async (intent) => {
          intents.push(intent);
          return true;
        }}
      />,
    );
    await user.selectOptions(
      screen.getByRole("combobox", { name: "Project for the first Chat input" }),
      "project.atlas",
    );
    await user.type(screen.getByRole("textbox", { name: "Chat input" }), "inspect it");
    await user.click(screen.getByRole("button", { name: "Send" }));
    expect(intents).toEqual([
      {
        type: "start",
        commandId: "command.project.start",
        workflowId: bundledDefaultWorkflowId,
        projectId: "project.atlas",
        input: "inspect it",
        attachments: [],
      },
    ]);
  });

  it("blocks first Send when the saved workflow binds project tools but No project is selected", async () => {
    const user = userEvent.setup();
    const intents: ChatIntent[] = [];
    render(
      <ChatComposer
        chat={{ ...runningChat, phase: "draft", lockedWorkflow: false, projectId: null }}
        projects={[
          {
            projectId: "project.atlas",
            name: "Project Atlas",
            workspaceKind: "local_directory",
          },
        ]}
        workflowRequiresProject
        nextCommandId={() => "command.project.required"}
        pending={false}
        stale={false}
        onSubmit={async (intent) => {
          intents.push(intent);
          return true;
        }}
      />,
    );

    await user.type(screen.getByRole("textbox", { name: "Chat input" }), "read it");
    const send = screen.getByRole("button", { name: "Send" });
    const reason =
      "Select a saved project before sending because the selected workflow binds project file tools.";
    expect(send).toBeDisabled();
    expect(send).toHaveAttribute("title", reason);
    expect(screen.getByText(reason)).toBeVisible();
    expect(intents).toEqual([]);

    await user.selectOptions(
      screen.getByRole("combobox", { name: "Project for the first Chat input" }),
      "project.atlas",
    );
    expect(send).toBeEnabled();
    await user.click(send);
    expect(intents).toEqual([
      expect.objectContaining({ type: "start", projectId: "project.atlas" }),
    ]);
  });

  it("reuses the stable command ID after an uncertain result and clears only after confirmation", async () => {
    const user = userEvent.setup();
    const intents: ChatIntent[] = [];
    render(
      <ChatComposer
        chat={runningChat}
        projects={[]}
        nextCommandId={() => `command.${intents.length + 1}`}
        pending={false}
        stale={false}
        onSubmit={async (intent) => {
          intents.push(intent);
          return intents.length === 2;
        }}
      />,
    );
    const input = screen.getByRole("textbox", { name: "Chat input" });
    await user.type(input, "retain until committed");
    await user.click(screen.getByRole("button", { name: "Queue" }));
    expect(input).toHaveValue("retain until committed");
    await user.click(screen.getByRole("button", { name: "Queue" }));
    await waitFor(() => expect(input).toHaveValue(""));
    expect(intents).toHaveLength(2);
    expect(intents[1]?.commandId).toBe(intents[0]?.commandId);
  });

  it("locks every editable composer control for a deferred command so no accepted typing is lost", async () => {
    const user = userEvent.setup();
    let settle!: (accepted: boolean) => void;
    const deferred = new Promise<boolean>((resolve) => {
      settle = resolve;
    });
    render(
      <ChatComposer
        chat={{
          ...runningChat,
          phase: "draft",
          lockedWorkflow: false,
          projectId: null,
        }}
        projects={[
          {
            projectId: "project.atlas",
            name: "Project Atlas",
            workspaceKind: "local_directory",
          },
        ]}
        nextCommandId={() => "command.deferred"}
        pending={false}
        stale={false}
        onSubmit={() => deferred}
      />,
    );
    const input = screen.getByRole("textbox", { name: "Chat input" });
    const workflow = screen.getByRole("combobox", {
      name: "Workflow for the first Chat input",
    });
    const project = screen.getByRole("combobox", {
      name: "Project for the first Chat input",
    });
    await user.type(input, "submitted text");
    await user.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(input).toBeDisabled());
    expect(workflow).toBeDisabled();
    expect(project).toBeDisabled();
    expect(input).toHaveValue("submitted text");
    await user.type(input, " must not disappear");
    expect(input).toHaveValue("submitted text");
    settle(true);
    await waitFor(() => expect(input).toHaveValue(""));
  });

  it("freezes the visible projection when the native delta skips a sequence", async () => {
    let snapshots = 0;
    const port: ChatCorePort = {
      async snapshot(): Promise<RuntimeSnapshot> {
        snapshots += 1;
        if (snapshots === 1)
          return snapshot(1, "Contiguous projection", [{ sequence: 1 }]);
        return snapshot(3, "Must stay hidden", [{ sequence: 3 }]);
      },
      async command() {
        return {
          commandId: "unused",
          accepted: true,
          currentVersion: 1,
          reason: null,
        };
      },
    };
    render(<ChatWorkspaceScreen corePort={port} pollIntervalMs={5} />);
    expect(
      await screen.findByRole("heading", { name: "Contiguous projection" }),
    ).toBeVisible();
    expect(await screen.findByText(/Projection disconnected/)).toBeVisible();
    expect(
      screen.queryByRole("heading", { name: "Must stay hidden" }),
    ).toBeNull();
  });

  it("projects auxiliary recovery changes even when throughSequence is unchanged", async () => {
    let snapshots = 0;
    const port: ChatCorePort = {
      async snapshot(): Promise<RuntimeSnapshot> {
        snapshots += 1;
        const base = snapshot(1, "Same-sequence Chat", [{ sequence: 1 }]);
        if (snapshots === 1) return base;
        return {
          ...base,
          chat: {
            ...base.chat,
            phase: "paused",
            recoveryPending: true,
          },
          events: [],
        };
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
    render(<ChatWorkspaceScreen corePort={port} pollIntervalMs={5} />);
    expect(
      await screen.findByRole("heading", { name: "Same-sequence Chat" }),
    ).toBeVisible();
    expect(
      await screen.findByText(/Interrupted command requires an explicit decision/),
    ).toBeVisible();
    expect(snapshots).toBeGreaterThan(1);
  });

  it("refreshes provider, project, and saved-workflow readiness when Chat re-enters", async () => {
    const user = userEvent.setup();
    let providerReady = true;
    let projectAvailable = false;
    let bindsProjectRead = false;
    let workflowSnapshots = 0;
    const corePort: ChatCorePort = {
      async snapshot() {
        const projected = snapshot(1, "Settings refresh Chat", [
          { sequence: 1 },
        ]);
        return {
          ...projected,
          chat: {
            ...projected.chat,
            phase: "draft",
            lockedWorkflow: false,
            projectId: null,
            disabledReason: providerReady
              ? undefined
              : "Configure an Exact model tier after the Settings change.",
          },
          projects: projectAvailable
            ? [
                {
                  projectId: "project.refreshed",
                  name: "Refreshed project",
                  workspaceKind: "local_directory" as const,
                },
              ]
            : [],
        };
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
    const workflowPort: Pick<WorkflowCorePort, "snapshot"> = {
      async snapshot() {
        workflowSnapshots += 1;
        return {
          version: workflowSnapshots,
          editable: true,
          document: simpleChatWorkflow(bindsProjectRead),
        };
      },
    };
    const screenProps = {
      corePort,
      pollIntervalMs: 60_000,
      workflowPort,
    } as const;
    const rendered = render(
      <ChatWorkspaceScreen {...screenProps} active />,
    );
    const input = await screen.findByRole("textbox", { name: "Chat input" });
    await user.type(input, "preserve this draft");
    const send = screen.getByRole("button", { name: "Send" });
    await waitFor(() => expect(send).toBeEnabled());

    rendered.rerender(<ChatWorkspaceScreen {...screenProps} active={false} />);
    providerReady = false;
    projectAvailable = true;
    bindsProjectRead = true;
    rendered.rerender(<ChatWorkspaceScreen {...screenProps} active />);
    await waitFor(() =>
      expect(
        screen.getByRole("option", { name: "Refreshed project" }),
      ).toBeInTheDocument(),
    );
    expect(input).toHaveValue("preserve this draft");
    await waitFor(() =>
      expect(send).toHaveAttribute(
        "title",
        "Configure an Exact model tier after the Settings change.",
      ),
    );

    rendered.rerender(<ChatWorkspaceScreen {...screenProps} active={false} />);
    providerReady = true;
    rendered.rerender(<ChatWorkspaceScreen {...screenProps} active />);
    await waitFor(() =>
      expect(send).toHaveAttribute(
        "title",
        "Select a saved project before sending because the selected workflow binds project file tools.",
      ),
    );
    expect(workflowSnapshots).toBeGreaterThanOrEqual(3);
    expect(input).toHaveValue("preserve this draft");
  });

  it("shows fail-after-stage recovery immediately and unlocks New Chat only after abandonment", async () => {
    const user = userEvent.setup();
    const onNewChat = vi.fn();
    const confirm = vi.fn(async () => true);
    const commands: ChatIntent[] = [];
    let state: "normal" | "recovery" | "abandoned" | "new" = "normal";
    const normal = {
      ...snapshot(1, "Staged failure Chat", [{ sequence: 1 }]),
      chat: {
        ...runningChat,
        title: "Staged failure Chat",
        phase: "waiting_input" as const,
        recoveryPending: false,
        expectedVersion: 1,
      },
    };
    const recovery = {
      ...normal,
      chat: {
        ...normal.chat,
        phase: "paused" as const,
        recoveryPending: true,
      },
    };
    const abandoned = {
      ...snapshot(2, "Staged failure Chat", [
        { sequence: 1 },
        { sequence: 2 },
      ]),
      chat: {
        ...normal.chat,
        phase: "failed" as const,
        recoveryPending: false,
        expectedVersion: 2,
      },
    };
    const newChatProjection = {
      ...snapshot(3, "New Chat", [
        { sequence: 1 },
        { sequence: 2 },
        { sequence: 3 },
      ]),
      chat: {
        ...normal.chat,
        chatId: "chat.new",
        runId: "run.draft",
        title: "New Chat",
        scope: "No project",
        phase: "draft" as const,
        lockedWorkflow: false,
        recoveryPending: false,
        expectedVersion: 3,
      },
    };
    const port: ChatCorePort = {
      async snapshot() {
        if (state === "normal") return normal;
        if (state === "recovery") return recovery;
        return state === "abandoned" ? abandoned : newChatProjection;
      },
      async command(intent) {
        commands.push(intent);
        if (intent.type === "enqueue") {
          state = "recovery";
          throw new Error("provider failed after durable command staging");
        }
        if (intent.type === "abandon_recovery") {
          state = "abandoned";
          return {
            commandId: intent.commandId,
            accepted: true,
            currentVersion: 2,
            reason: null,
          };
        }
        if (intent.type === "new_chat") {
          state = "new";
          return {
            commandId: intent.commandId,
            accepted: true,
            currentVersion: 3,
            reason: null,
          };
        }
        throw new Error(`unexpected command ${intent.type}`);
      },
    };

    function FailureRecoveryHarness(): React.JSX.Element {
      const [recoveryPending, setRecoveryPending] = useState(false);
      const [newChatRequest, setNewChatRequest] = useState(0);
      return (
        <>
          <NavigationPane
            route="chat"
            collapsed={false}
            newChatDisabledReason={
              recoveryPending ? "Resolve interrupted command first" : null
            }
            onNavigate={() => undefined}
            onNewChat={() => {
              onNewChat();
              setNewChatRequest((current) => current + 1);
            }}
            onToggleCollapsed={() => undefined}
          />
          <ChatWorkspaceScreen
            confirmRecoveryAbandon={confirm}
            corePort={port}
            newChatRequest={newChatRequest}
            pollIntervalMs={60_000}
            onRecoveryPendingChange={setRecoveryPending}
          />
        </>
      );
    }

    render(<FailureRecoveryHarness />);
    const input = await screen.findByRole("textbox", { name: "Chat input" });
    await user.type(input, "trigger staged failure");
    await user.click(screen.getByRole("button", { name: "Queue" }));
    expect(
      await screen.findByText(/Interrupted command requires an explicit decision/),
    ).toBeVisible();
    expect(input).toBeDisabled();
    const newChat = screen.getByRole("button", { name: /New Chat/ });
    expect(newChat).toBeDisabled();
    expect(commands.map(({ type }) => type)).toEqual(["enqueue"]);

    await user.click(
      screen.getByRole("button", { name: "Abandon as uncertain" }),
    );
    await waitFor(() =>
      expect(commands.map(({ type }) => type)).toEqual([
        "enqueue",
        "abandon_recovery",
      ]),
    );
    await waitFor(() => expect(newChat).toBeEnabled());
    await user.click(newChat);
    expect(onNewChat).toHaveBeenCalledOnce();
    expect(
      await screen.findByRole("heading", { name: "New Chat" }),
    ).toBeVisible();
    await waitFor(() =>
      expect(commands.map(({ type }) => type)).toEqual([
        "enqueue",
        "abandon_recovery",
        "new_chat",
      ]),
    );
    const freshInput = screen.getByRole("textbox", { name: "Chat input" });
    expect(freshInput).toBeEnabled();
    expect(freshInput).toHaveValue("");
  });

  it("locks normal Chat actions and dispatches one fresh explicit resume for interrupted recovery", async () => {
    const user = userEvent.setup();
    const commands: ChatIntent[] = [];
    const recovery = {
      ...snapshot(1, "Interrupted Chat", [{ sequence: 1 }]),
      chat: {
        ...runningChat,
        title: "Interrupted Chat",
        phase: "paused" as const,
        recoveryPending: true,
        expectedVersion: 1,
      },
    };
    const port: ChatCorePort = {
      async snapshot() {
        return recovery;
      },
      async command(intent) {
        commands.push(intent);
        return {
          commandId: intent.commandId,
          accepted: false,
          currentVersion: 1,
          reason: "fixture keeps recovery pending",
        };
      },
    };
    render(<ChatWorkspaceScreen corePort={port} pollIntervalMs={60_000} />);

    expect(
      await screen.findByText(/Interrupted command requires an explicit decision/),
    ).toBeVisible();
    expect(screen.getByRole("textbox", { name: "Chat input" })).toBeDisabled();
    expect(screen.getByRole("button", { name: /Cancel/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Queue" })).toBeDisabled();
    const resume = screen.getByRole("button", {
      name: "Resume interrupted command",
    });
    expect(resume).toBeEnabled();
    await user.click(resume);
    await waitFor(() => expect(commands).toHaveLength(1));
    expect(commands[0]).toMatchObject({ type: "resume" });
    expect(commands[0]!.commandId).toMatch(/^(?:desktop\.)?chat\./u);
    expect(
      await screen.findByText(/fixture keeps recovery pending/),
    ).toBeVisible();
    expect(screen.getByRole("textbox", { name: "Chat input" })).toBeDisabled();
  });

  it("keeps New Chat locked until confirmed uncertain abandonment is committed", async () => {
    const user = userEvent.setup();
    const onNewChat = vi.fn();
    const confirm = vi
      .fn<(title: string, body: string) => Promise<boolean>>()
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce(true);
    const commands: ChatIntent[] = [];
    let abandoned = false;
    const interrupted = {
      ...snapshot(1, "Interrupted Chat", [{ sequence: 1 }]),
      chat: {
        ...runningChat,
        title: "Interrupted Chat",
        phase: "paused" as const,
        recoveryPending: true,
        expectedVersion: 1,
      },
    };
    const resolved = {
      ...snapshot(2, "Interrupted Chat", [
        { sequence: 1 },
        { sequence: 2 },
      ]),
      chat: {
        ...runningChat,
        title: "Interrupted Chat",
        phase: "failed" as const,
        recoveryPending: false,
        expectedVersion: 2,
      },
    };
    const port: ChatCorePort = {
      async snapshot() {
        return abandoned ? resolved : interrupted;
      },
      async command(intent) {
        commands.push(intent);
        abandoned = true;
        return {
          commandId: intent.commandId,
          accepted: true,
          currentVersion: 2,
          reason: null,
        };
      },
    };

    function RecoveryHarness(): React.JSX.Element {
      const [recoveryPending, setRecoveryPending] = useState(false);
      return (
        <>
          <NavigationPane
            route="chat"
            collapsed={false}
            newChatDisabledReason={
              recoveryPending
                ? "Resume the interrupted command before starting a New Chat"
                : null
            }
            onNavigate={() => undefined}
            onNewChat={onNewChat}
            onToggleCollapsed={() => undefined}
          />
          <ChatWorkspaceScreen
            confirmRecoveryAbandon={confirm}
            corePort={port}
            pollIntervalMs={60_000}
            onRecoveryPendingChange={setRecoveryPending}
          />
        </>
      );
    }

    render(
      <RecoveryHarness />,
    );
    const newChat = screen.getByRole("button", { name: /New Chat/ });
    await waitFor(() => expect(newChat).toBeDisabled());
    expect(newChat).toHaveAttribute(
      "title",
      "Resume the interrupted command before starting a New Chat",
    );
    const abandon = screen.getByRole("button", {
      name: "Abandon as uncertain",
    });
    await user.click(abandon);
    expect(confirm).toHaveBeenLastCalledWith(
      "Abandon interrupted command as uncertain?",
      expect.stringContaining("without calling its provider or tools"),
    );
    expect(commands).toEqual([]);
    expect(newChat).toBeDisabled();

    await user.click(abandon);
    await waitFor(() => expect(commands).toHaveLength(1));
    expect(commands[0]).toMatchObject({ type: "abandon_recovery" });
    expect(commands[0]!.commandId).toMatch(/^(?:desktop\.)?chat\./u);
    await waitFor(() => expect(newChat).toBeEnabled());
    expect(
      screen.queryByRole("button", { name: "Abandon as uncertain" }),
    ).toBeNull();
    await user.click(newChat);
    expect(onNewChat).toHaveBeenCalledOnce();
  });

  it("maps selected tool and error cards to distinct contextual Run details", async () => {
    const user = userEvent.setup();
    const projected = {
      ...snapshot(2, "Run details Chat", [
        canonicalEvent(1, "tool.completed", {
          title: "Read notes",
          body: "notes.txt",
          status: "completed",
        }),
        canonicalEvent(2, "execution.failed", {
          title: "Provider failure",
          body: "network unavailable",
          status: "failed",
        }),
      ]),
      evidence: [
        {
          id: "evidence.event.chat.1",
          category: "artifact" as const,
          label: "Tool evidence",
          value: { path: "notes.txt" },
          state: "available" as const,
        },
        {
          id: "evidence.event.chat.2",
          category: "debug" as const,
          label: "Error evidence",
          value: { reason: "network unavailable" },
          state: "available" as const,
        },
      ],
    };
    const port: ChatCorePort = {
      async snapshot() {
        return projected;
      },
      async command(intent) {
        return {
          commandId: intent.commandId,
          accepted: false,
          currentVersion: 2,
          reason: "unused",
        };
      },
    };
    render(<ChatWorkspaceScreen corePort={port} pollIntervalMs={60_000} />);

    const toolCard = await screen.findByRole("article", {
      name: "Tool: Read notes",
    });
    await user.click(within(toolCard).getByTitle("Show Run details for Read notes"));
    expect(
      screen.getByRole("heading", { name: "Read notes" }),
    ).toBeVisible();
    const inspectorElement = screen.getByLabelText("Run details");
    const inspector = within(inspectorElement);
    expect(inspector.getByText(/notes\.txt/)).toBeVisible();
    expect(inspectorElement.querySelector(".run-details-content pre")).toBeNull();
    await user.click(inspector.getByRole("tab", { name: "Raw JSON" }));
    expect(inspectorElement.querySelector(".run-details-json")).toHaveTextContent(
      '"scope": "timeline_item"',
    );
    await user.click(inspector.getByRole("tab", { name: "Details" }));
    const errorCard = screen.getByRole("article", {
      name: "Error: Provider failure",
    });
    await user.click(
      within(errorCard).getByTitle("Show Run details for Provider failure"),
    );
    expect(
      screen.getByRole("heading", { name: "Provider failure" }),
    ).toBeVisible();
    expect(inspector.getByText(/network unavailable/)).toBeVisible();
    await user.click(
      inspector.getByTitle("Show Run details for Entire run"),
    );
    expect(
      screen.getByRole("heading", { name: "Entire run" }),
    ).toBeVisible();
    expect(inspector.getByRole("heading", { name: "Execution log" })).toBeVisible();
  });

  it("maps approval-card actions to the stable target and typed intent", async () => {
    const user = userEvent.setup();
    const actions: Array<{
      readonly action: "approve" | "reject";
      readonly id: string;
    }> = [];
    const item = {
      id: "approval.lease.1",
      kind: "approval" as const,
      title: "Approve workspace lease",
      body: "Write generated files",
      createdAt: "now",
      status: "pending",
    };
    render(
      <TimelineCard
        card={toConversationCard(item)}
        item={item}
        selected={false}
        onSelect={() => undefined}
        onAction={(action, id) =>
          actions.push({ action: action as "approve" | "reject", id })
        }
      />,
    );
    await user.click(screen.getByRole("button", { name: "Approve" }));
    expect(actions).toEqual([{ action: "approve", id: "approval.lease.1" }]);
    expect(
      timelineActionIntent(actions[0]!.action, actions[0]!.id, "command.5"),
    ).toEqual({
      type: "approval",
      commandId: "command.5",
      targetId: "approval.lease.1",
      approved: true,
    });
  });

  it("preserves waiting-for-input and queues follow-up input without run controls", async () => {
    const user = userEvent.setup();
    const commands: ChatIntent[] = [];
    const waiting = {
      ...snapshot(1, "Waiting Chat", [{ sequence: 1 }]),
      chat: {
        ...runningChat,
        title: "Waiting Chat",
        phase: "waiting_input" as const,
        expectedVersion: 1,
      },
    };
    const port: ChatCorePort = {
      async snapshot() {
        return waiting;
      },
      async command(intent) {
        commands.push(intent);
        return {
          commandId: intent.commandId,
          accepted: false,
          currentVersion: 1,
          reason: "test receipt",
        };
      },
    };
    render(<ChatWorkspaceScreen corePort={port} pollIntervalMs={60_000} />);
    expect(await screen.findByText("Waiting for input")).toBeVisible();
    expect(screen.queryByRole("button", { name: /Pause/ })).toBeNull();
    const input = screen.getByRole("textbox", { name: "Chat input" });
    await user.type(input, "follow up");
    await user.click(screen.getByRole("button", { name: "Queue" }));
    await waitFor(() => expect(commands[0]?.type).toBe("enqueue"));
  });

  it("streams exact committed model and tool spans before command settlement", async () => {
    const user = userEvent.setup();
    let eventListener: ((event: CoreEventEnvelope) => void) | undefined;
    let settle!: (receipt: Awaited<ReturnType<ChatCorePort["command"]>>) => void;
    let commandId = "";
    let projectedSnapshot = snapshot(1, "Live Chat", [{ sequence: 1 }]);
    const port: ChatCorePort = {
      async snapshot() {
        return projectedSnapshot;
      },
      command(intent) {
        commandId = intent.commandId;
        return new Promise((resolve) => {
          settle = resolve;
        });
      },
      async subscribeEvents(listener) {
        eventListener = listener;
        return () => undefined;
      },
    };
    render(<ChatWorkspaceScreen corePort={port} pollIntervalMs={60_000} />);
    await screen.findByRole("heading", { name: "Live Chat" });
    const input = screen.getByRole("textbox", { name: "Chat input" });
    await user.type(input, "inspect the project");
    await user.click(screen.getByRole("button", { name: "Queue" }));
    await waitFor(() => expect(commandId).not.toBe(""));
    const streamed = [
      canonicalEvent(2, "span.started", {
        spanId: "span.agent.test",
        spanKind: "agent_loop",
        semanticRole: "agent_loop",
        title: "Agent",
        status: "running",
      }),
      canonicalEvent(3, "span.started", {
        spanId: "span.model.test",
        parentSpanId: "span.agent.test",
        spanKind: "model_call",
        semanticRole: "model_call",
        title: "Model call 1",
        status: "running",
      }),
      canonicalEvent(4, "span.content_delta", {
        spanId: "span.model.test",
        channel: "reasoning",
        sourceClassification: "source_provided",
        append: "Inspecting the project",
        status: "running",
      }),
      canonicalEvent(5, "span.started", {
        spanId: "span.tool.test",
        parentSpanId: "span.agent.test",
        spanKind: "tool_call",
        semanticRole: "tool",
        title: "tool.files.list",
        capabilityId: "tool.files.list",
        status: "running",
        hasInput: true,
        input: { path: "." },
      }),
      canonicalEvent(6, "span.completed", {
        spanId: "span.tool.test",
        status: "completed",
        hasOutput: true,
        output: { files: ["notes.txt"] },
      }),
    ];
    for (const event of streamed) eventListener?.(event);
    const toolCard = await screen.findByRole("article", {
      name: "Tool: tool.files.list",
    });
    expect(within(toolCard).getByText("tool.files.list")).toBeVisible();
    expect(within(toolCard).getByText("Input")).toBeVisible();
    expect(within(toolCard).getByText("Output")).toBeVisible();
    expect(within(toolCard).getAllByText(/"path": "\."/)[0]).toBeVisible();
    expect(within(toolCard).getAllByText(/"notes.txt"/)[0]).toBeVisible();
    expect(screen.getByText("Inspecting the project")).toBeVisible();
    const terminal = [
      canonicalEvent(7, "span.completed", {
        spanId: "span.model.test",
        status: "completed",
        hasOutput: true,
        output: "I found the project",
      }),
      canonicalEvent(8, "span.completed", {
        spanId: "span.agent.test",
        status: "completed",
      }),
      canonicalEvent(9, "message.assistant", {
        body: "Agent finished",
        createdAt: "now",
      }),
    ];
    projectedSnapshot = snapshot(9, "Live Chat", [
      { sequence: 1 },
      ...streamed,
      ...terminal,
    ]);
    settle({
      commandId,
      accepted: true,
      currentVersion: 9,
      reason: null,
    });
    expect(await screen.findByText("Agent finished")).toBeVisible();
    const reasoningBubble = screen.getByLabelText("Thinking: Model call 1");
    expect(
      within(reasoningBubble).getByText("Inspecting the project"),
    ).toBeVisible();
    expect(screen.getByText("Provider-supplied reasoning")).toBeVisible();
  });

  it("subscribes before dispatch so immediate provider states cannot be lost", async () => {
    const user = userEvent.setup();
    let eventListener: ((event: CoreEventEnvelope) => void) | undefined;
    let releaseSubscription!: () => void;
    let settleCommand!: (
      receipt: Awaited<ReturnType<ChatCorePort["command"]>>,
    ) => void;
    let commandStarted = false;
    const port: ChatCorePort = {
      async snapshot() {
        return snapshot(1, "Fast provider", [{ sequence: 1 }]);
      },
      command(intent) {
        commandStarted = true;
        eventListener?.(
          canonicalEvent(2, "span.started", {
            requestId: intent.commandId,
            runId: "run.test",
            spanId: `span.model.${intent.commandId}`,
            spanKind: "model_call",
            semanticRole: "model_call",
            title: "Model call 1",
            body: "First synchronous chunk",
            status: "running",
          }),
        );
        return new Promise((resolve) => {
          settleCommand = resolve;
        });
      },
      subscribeEvents(listener) {
        eventListener = listener;
        return new Promise((resolve) => {
          releaseSubscription = () => resolve(() => undefined);
        });
      },
    };
    render(<ChatWorkspaceScreen corePort={port} pollIntervalMs={60_000} />);
    await screen.findByRole("heading", { name: "Fast provider" });
    await user.type(screen.getByRole("textbox", { name: "Chat input" }), "go");
    await user.click(screen.getByRole("button", { name: "Queue" }));
    expect(commandStarted).toBe(false);

    releaseSubscription();
    expect(await screen.findByText("First synchronous chunk")).toBeVisible();
    expect(commandStarted).toBe(true);
    settleCommand({
      commandId: "command.test",
      accepted: false,
      currentVersion: 1,
      reason: "test cleanup",
    });
  });

  it("renders pushed events while snapshot polling is paused for a running command", async () => {
    const user = userEvent.setup();
    let snapshotCalls = 0;
    let eventListener: ((event: CoreEventEnvelope) => void) | undefined;
    let commandId = "";
    const port: ChatCorePort = {
      snapshot() {
        snapshotCalls += 1;
        if (snapshotCalls === 1)
          return Promise.resolve(snapshot(1, "Reactive Chat", [{ sequence: 1 }]));
        return new Promise(() => undefined);
      },
      command(intent) {
        commandId = intent.commandId;
        return new Promise(() => undefined);
      },
      async subscribeEvents(listener) {
        eventListener = listener;
        return () => undefined;
      },
    };
    render(<ChatWorkspaceScreen corePort={port} pollIntervalMs={20} />);
    await screen.findByRole("heading", { name: "Reactive Chat" });
    await user.type(screen.getByRole("textbox", { name: "Chat input" }), "go");
    await user.click(screen.getByRole("button", { name: "Queue" }));
    await waitFor(() => expect(commandId).not.toBe(""));

    eventListener?.(
      canonicalEvent(2, "span.started", {
        requestId: commandId,
        runId: "run.test",
        spanId: `span.model.${commandId}`,
        spanKind: "model_call",
        semanticRole: "model_call",
        title: "Model call 1",
        body: "Arrived while snapshot ownership is unavailable",
        status: "running",
      }),
    );
    expect(
      await screen.findByText("Arrived while snapshot ownership is unavailable"),
    ).toBeVisible();
    await new Promise((resolve) => window.setTimeout(resolve, 80));
    expect(snapshotCalls).toBe(2);
  });

  it("marks only active cards busy and renders settled thinking as terminal", () => {
    const { rerender } = render(
      <TimelineCard
        card={toConversationCard({
          id: "thinking.1",
          kind: "thinking",
          title: "Thinking",
          body: "Done",
          createdAt: "now",
          status: "completed",
        })}
        item={{
          id: "thinking.1",
          kind: "thinking",
          title: "Thinking",
          body: "Done",
          createdAt: "now",
          status: "completed",
        }}
        selected={false}
        onSelect={() => undefined}
        onAction={() => undefined}
      />,
    );
    const settled = screen.getByLabelText("Thinking: Thinking");
    expect(settled).not.toHaveAttribute("aria-busy", "true");
    expect(within(settled).getByText("✓")).toBeVisible();
    expect(settled).toHaveClass("thinking-turn");
    expect(within(settled).getByRole("img", { name: "Main model" })).toBeVisible();
    expect(settled.querySelector(".thinking-bubble")).not.toBeNull();

    const runningItem = {
      id: "step.1",
      kind: "step" as const,
      title: "Plan",
      body: "model_call: running",
      createdAt: "now",
      status: "started",
      metadata: { live: true },
    };
    rerender(
      <TimelineCard
        card={toConversationCard(runningItem)}
        item={runningItem}
        selected={false}
        onSelect={() => undefined}
        onAction={() => undefined}
      />,
    );
    expect(screen.getByLabelText("Step: Plan")).toHaveAttribute(
      "aria-busy",
      "true",
    );
  });

  it("renders assistant Markdown inside a model-owned speech bubble", () => {
    const item = {
      id: "message.assistant.1",
      kind: "message" as const,
      title: "Aworkit",
      body: "**Sunny** with a [forecast](https://example.test/weather).",
      createdAt: "now",
      status: "completed",
    };

    render(
      <TimelineCard
        card={toConversationCard(item)}
        item={item}
        selected={false}
        onSelect={() => undefined}
        onAction={() => undefined}
      />,
    );

    const speech = screen.getByLabelText("Aworkit message");
    expect(speech).toHaveClass("speech-turn");
    expect(within(speech).getByRole("img", { name: "Main model" })).toBeVisible();
    expect(within(speech).getByText("Sunny").tagName).toBe("STRONG");
    expect(within(speech).getByRole("link", { name: "forecast" })).toHaveAttribute(
      "href",
      "https://example.test/weather",
    );
  });

  it("points delegated speech and thinking at the subagent robot", () => {
    const speech = {
      id: "span.subagent.1",
      kind: "subagent" as const,
      actor: "subagent" as const,
      title: "tool.subagent",
      body: "Subagent completed.",
      createdAt: "now",
      status: "completed",
      output: { finalText: "**Delegated result** with evidence." },
    };
    const { rerender } = render(
      <TimelineCard
        card={toConversationCard(speech)}
        item={speech}
        selected={false}
        onSelect={() => undefined}
        onAction={() => undefined}
      />,
    );

    const speechBubble = screen.getByLabelText("Subagent: tool.subagent");
    expect(speechBubble).toHaveClass("speech-turn", "actor-subagent");
    expect(
      within(speechBubble).getByRole("img", { name: "Subagent" }),
    ).toBeVisible();
    expect(within(speechBubble).getByText("Delegated result").tagName).toBe(
      "STRONG",
    );

    const thinking = {
      id: "span.subagent.model.1",
      kind: "thinking" as const,
      actor: "subagent" as const,
      title: "Model call 1",
      body: "Checking the delegated files.",
      createdAt: "now",
      status: "running",
      reasoningCategory: "source_provided" as const,
    };
    rerender(
      <TimelineCard
        card={toConversationCard(thinking)}
        item={thinking}
        selected={false}
        onSelect={() => undefined}
        onAction={() => undefined}
      />,
    );
    const thoughtBubble = screen.getByLabelText("Thinking: Model call 1");
    expect(thoughtBubble).toHaveClass("thinking-turn", "actor-subagent");
    expect(
      within(thoughtBubble).getByRole("img", { name: "Subagent" }),
    ).toBeVisible();
  });

  it("renders wait transitions without duplicating their pass-through data", () => {
    const item = {
      id: "node.wait.1",
      kind: "step" as const,
      title: "Wait for input",
      body: "Ready for another message.",
      createdAt: "now",
      status: "completed",
      input: "answer",
      output: "answer",
      metadata: {
        live: true,
        nodeType: "wait",
        input: "answer",
        output: "answer",
      },
    };

    render(
      <TimelineCard
        card={toConversationCard(item)}
        item={item}
        selected={false}
        onSelect={() => undefined}
        onAction={() => undefined}
      />,
    );

    const card = screen.getByLabelText("Step: Wait for input");
    expect(within(card).getByText("Ready for another message.")).toBeVisible();
    expect(within(card).queryByText("Input")).not.toBeInTheDocument();
    expect(within(card).queryByText("Output")).not.toBeInTheDocument();
  });

  it("does not expose unsupported terminal controls", async () => {
    const failed = {
      ...snapshot(4, "Failed Run", [
        { sequence: 1 },
        { sequence: 2 },
        { sequence: 3 },
        { sequence: 4 },
      ]),
      chat: {
        ...runningChat,
        title: "Failed Run",
        phase: "failed" as const,
        expectedVersion: 4,
      },
    };
    const port: ChatCorePort = {
      async snapshot() {
        return failed;
      },
      async command(intent) {
        return {
          commandId: intent.commandId,
          accepted: false,
          currentVersion: 4,
          reason: "test receipt",
        };
      },
    };
    render(<ChatWorkspaceScreen corePort={port} pollIntervalMs={60_000} />);
    expect(
      await screen.findByRole("heading", { name: "Failed Run" }),
    ).toBeVisible();
    expect(screen.queryByText("More")).toBeNull();
    for (const label of ["Retry", "Fork", "Continue", "Pause", "Resume"])
      expect(screen.queryByRole("button", { name: label })).toBeNull();
  });
});

function snapshot(
  sequence: number,
  title: string,
  events: readonly (TestEvent | RuntimeEvent)[],
): RuntimeSnapshot {
  return {
    version: sequence,
    throughSequence: sequence,
    reducerVersion: "chat.semantic.reducer.v1",
    stateHash: `sha256:${"0".repeat(64)}`,
    chat: { ...runningChat, title, expectedVersion: sequence },
    projects: [],
    evidence: [],
    events: events.map(canonicalTestEvent),
  };
}

type TestEvent = {
  readonly sequence: number;
  readonly [key: string]: unknown;
};

function canonicalTestEvent(event: TestEvent | RuntimeEvent): RuntimeEvent {
  if (
    typeof event.schemaVersion === "number" &&
    typeof event.streamId === "string" &&
    typeof event.branchId === "string" &&
    typeof event.eventId === "string" &&
    typeof event.kind === "string" &&
    Object.hasOwn(event, "payload")
  )
    return event as unknown as CoreEventEnvelope;
  return {
    schemaVersion: 1,
    streamId: "chat.test",
    branchId: "main",
    sequence: event.sequence,
    eventId: `event.chat.${event.sequence}`,
    kind: typeof event.kind === "string" ? event.kind : "test.event",
    payload: Object.hasOwn(event, "payload") ? event.payload : {},
  };
}

function canonicalEvent(
  sequence: number,
  kind: string,
  payload: Record<string, unknown>,
): CoreEventEnvelope {
  return {
    schemaVersion: 1,
    streamId: "chat.test",
    branchId: "main",
    sequence,
    eventId: `event.chat.${sequence}`,
    kind,
    spanId:
      typeof payload.spanId === "string" ? payload.spanId : undefined,
    payload,
  };
}

function simpleChatWorkflow(bindsProjectRead: boolean): WorkflowDocument {
  return {
    schemaVersion: 1,
    id: "workflow.simple-chat",
    nodes: [
      { id: "input.1", type: "input" },
      {
        id: "agent.1",
        type: "agent",
        configuration: {
          modelTierId: "tier:balanced",
          toolIds: bindsProjectRead ? ["tool.files.read"] : [],
          maxTurns: bindsProjectRead ? 2 : 1,
        },
      },
      { id: "output.1", type: "output" },
      { id: "wait.1", type: "wait" },
    ],
    edges: [
      { id: "one", source: "input.1", target: "agent.1" },
      { id: "two", source: "agent.1", target: "output.1" },
      { id: "three", source: "output.1", target: "wait.1" },
    ],
  };
}
