import { describe, expect, it } from "vitest";
import {
  deriveActivityCards,
  deriveLiveActivityCards,
  mergeTimeline,
} from "./activityProjection";
import type { RuntimeEvent } from "./corePort";
import type { TimelineItem } from "./types";

describe("frontend activity projection", () => {
  it("derives a todo card from the newest tool.todo fact only", () => {
    const events: RuntimeEvent[] = [
      {
        sequence: 2,
        eventId: "event.chat.2",
        kind: "tool.todo",
        payload: {
          createdAt: "t",
          todos: [{ content: "first", status: "pending" }],
        },
      },
      {
        sequence: 5,
        eventId: "event.chat.5",
        kind: "tool.todo",
        payload: {
          createdAt: "t",
          todos: [
            { content: "plan", status: "completed" },
            { content: "agent", status: "in_progress" },
          ],
        },
      },
    ];
    const cards = deriveActivityCards(events);
    expect(cards).toHaveLength(1);
    expect(cards[0]).toMatchObject({
      id: "event.chat.5",
      kind: "todo",
      title: "Task list",
      status: "completed",
    });
    expect(cards[0]?.body).toContain("[completed] plan");
    expect(cards[0]?.body).toContain("[in_progress] agent");
  });

  it("derives subagent and MCP cards from their terminal facts", () => {
    const events: RuntimeEvent[] = [
      {
        sequence: 3,
        eventId: "event.chat.3",
        kind: "subagent.completed",
        payload: {
          createdAt: "t",
          capabilityId: "tool.subagent",
          body: "researched the codebase",
          status: "completed",
        },
      },
      {
        sequence: 4,
        eventId: "event.chat.4",
        kind: "mcp.failed",
        payload: {
          createdAt: "t",
          capabilityId: "mcp://memory/recall",
          body: "connection refused",
          status: "failed",
        },
      },
    ];
    const cards = deriveActivityCards(events);
    expect(cards).toHaveLength(2);
    expect(cards[0]).toMatchObject({
      id: "event.chat.3",
      kind: "subagent",
      status: "completed",
    });
    expect(cards[1]).toMatchObject({
      id: "event.chat.4",
      kind: "mcp",
      title: "mcp://memory/recall",
      status: "failed",
    });
  });

  it("merges native and derived cards in committed event order", () => {
    const native: TimelineItem[] = [
      { id: "event.chat.1", kind: "message", title: "You", createdAt: "t" },
      { id: "event.chat.6", kind: "message", title: "Aworkit", createdAt: "t" },
    ];
    const derived: TimelineItem[] = [
      { id: "event.chat.3", kind: "subagent", title: "Subagent run", createdAt: "t" },
      { id: "event.chat.5", kind: "todo", title: "Task list", createdAt: "t" },
    ];
    expect(mergeTimeline(native, derived).map((item) => item.id)).toEqual([
      "event.chat.1",
      "event.chat.3",
      "event.chat.5",
      "event.chat.6",
    ]);
  });

  it("projects transient reasoning and running tools as live cards", () => {
    const cards = deriveLiveActivityCards([
      {
        requestId: "command.1",
        runId: "run.1",
        activityId: "model.reasoning.command.1",
        kind: "reasoning",
        title: "Thinking",
        body: "Checking the project structure",
        status: "running",
        reasoningCategory: "source_provided",
      },
      {
        requestId: "command.1",
        runId: "run.1",
        activityId: "model.response.command.1",
        kind: "response",
        title: "Response",
        body: "The answer is streaming",
        status: "running",
      },
      {
        requestId: "command.1",
        runId: "run.1",
        activityId: "node.command.1.agent.1",
        kind: "step",
        title: "Agent",
        body: "agent: running",
        status: "started",
      },
      {
        requestId: "command.1",
        runId: "run.1",
        activityId: "tool.call.1",
        kind: "tool",
        title: "tool.files.list",
        body: "{}",
        status: "running",
        capabilityId: "tool.files.list",
      },
    ]);
    expect(cards).toEqual([
      expect.objectContaining({
        id: "live.model.reasoning.command.1",
        kind: "thinking",
        reasoningCategory: "source_provided",
      }),
      expect.objectContaining({
        id: "live.model.response.command.1",
        kind: "model",
        status: "running",
      }),
      expect.objectContaining({
        id: "live.node.command.1.agent.1",
        kind: "plan",
        status: "started",
      }),
      expect.objectContaining({
        id: "live.tool.call.1",
        kind: "tool",
        status: "running",
      }),
    ]);
  });
});
