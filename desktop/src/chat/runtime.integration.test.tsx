// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import { ChatComposer } from "./ChatComposer";
import {
  ChatWorkspaceScreen,
  timelineActionIntent,
} from "./ChatWorkspaceScreen";
import { toConversationCard } from "./conversation";
import { TimelineCard } from "./ConversationTimeline";
import type { ChatCorePort, RuntimeSnapshot } from "./corePort";
import type { ChatIntent, ChatProjection } from "./types";

afterEach(cleanup);

const runningChat: ChatProjection = {
  chatId: "chat.test",
  runId: "run.test",
  title: "Contiguous projection",
  scope: "Test project",
  workflowName: "Test workflow",
  branch: "main",
  phase: "running",
  lockedWorkflow: true,
  queuedInputs: [],
  expectedVersion: 1,
};

describe("Chat native-port recovery contracts", () => {
  it("reuses the stable command ID after an uncertain result and clears only after confirmation", async () => {
    const user = userEvent.setup();
    const intents: ChatIntent[] = [];
    render(
      <ChatComposer
        chat={runningChat}
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

  it.each([
    ["Retry", "retry"],
    ["Fork", "fork"],
    ["Continue", "continue"],
  ] as const)(
    "maps the %s terminal control to a typed %s intent",
    async (label, type) => {
      const user = userEvent.setup();
      const commands: ChatIntent[] = [];
      const failed = {
        ...snapshot(4, "Failed Run", [
          { sequence: 1 },
          { sequence: 2 },
          { sequence: 3 },
          { sequence: 4 },
        ]),
        chat: { ...runningChat, phase: "failed" as const, expectedVersion: 4 },
      };
      const port: ChatCorePort = {
        async snapshot() {
          return failed;
        },
        async command(intent) {
          commands.push(intent);
          return {
            commandId: intent.commandId,
            accepted: false,
            currentVersion: 4,
            reason: "test receipt",
          };
        },
      };
      render(<ChatWorkspaceScreen corePort={port} pollIntervalMs={60_000} />);
      await user.click(await screen.findByText("More"));
      await user.click(screen.getByRole("button", { name: label }));
      await waitFor(() => expect(commands[0]?.type).toBe(type));
    },
  );
});

function snapshot(
  sequence: number,
  title: string,
  events: RuntimeSnapshot["events"],
): RuntimeSnapshot {
  return {
    version: sequence,
    lastSequence: sequence,
    chat: { ...runningChat, title, expectedVersion: sequence },
    timeline: [],
    evidence: [],
    events,
  };
}
