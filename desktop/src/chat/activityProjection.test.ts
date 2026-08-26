import { describe, expect, it } from "vitest";
import {
  deriveActivityCards,
  deriveLiveActivityCards,
  mergeTimeline,
  reduceRunEventProjection,
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
        kind: "step",
        status: "started",
      }),
      expect.objectContaining({
        id: "live.tool.call.1",
        kind: "tool",
        status: "running",
      }),
    ]);
  });

  it("hides successful output nodes while retaining wait and failed output states", () => {
    const cards = deriveLiveActivityCards([
      {
        requestId: "command.1",
        runId: "run.1",
        activityId: "node.output.1",
        kind: "step",
        nodeType: "output",
        title: "Output",
        body: "Response prepared.",
        status: "completed",
        input: "answer",
        output: "answer",
      },
      {
        requestId: "command.1",
        runId: "run.1",
        activityId: "node.wait.1",
        kind: "step",
        nodeType: "wait",
        title: "Wait for input",
        body: "Ready for another message.",
        status: "completed",
        input: "answer",
        output: "answer",
      },
      {
        requestId: "command.1",
        runId: "run.1",
        activityId: "node.output.failed",
        kind: "step",
        nodeType: "output",
        title: "Output",
        body: "Output failed.",
        status: "failed",
      },
    ]);

    expect(cards.map((card) => card.id)).toEqual([
      "live.node.wait.1",
      "live.node.output.failed",
    ]);
  });

  it("reduces sequenced deltas without moving activities or losing exact tool data", () => {
    let activities = reduceRunEventProjection([], {
      schemaVersion: 1,
      requestId: "command.1",
      runId: "run.1",
      sequence: 1,
      eventId: "run.event.1",
      activityId: "model.reasoning.command.1.turn.1",
      kind: "reasoning",
      title: "Thinking",
      body: "Need ",
      status: "running",
      dataMode: "append",
      output: "Need ",
    });
    activities = reduceRunEventProjection(activities, {
      schemaVersion: 1,
      requestId: "command.1",
      runId: "run.1",
      sequence: 2,
      eventId: "run.event.2",
      activityId: "model.reasoning.command.1.turn.1",
      kind: "reasoning",
      title: "Thinking",
      body: "files",
      status: "running",
      dataMode: "append",
      output: "files",
    });
    activities = reduceRunEventProjection(activities, {
      schemaVersion: 1,
      requestId: "command.1",
      runId: "run.1",
      sequence: 3,
      eventId: "run.event.3",
      activityId: "tool.call.1",
      kind: "tool",
      title: "tool.files.list",
      body: "Tool invocation started.",
      status: "running",
      dataMode: "replace",
      input: { callId: "call.1", arguments: { path: "." } },
    });
    activities = reduceRunEventProjection(activities, {
      schemaVersion: 1,
      requestId: "command.1",
      runId: "run.1",
      sequence: 4,
      eventId: "run.event.4",
      activityId: "tool.call.1",
      kind: "tool",
      title: "tool.files.list",
      body: "Listed 1 file.",
      status: "completed",
      dataMode: "replace",
      output: { callId: "call.1", content: { files: ["a.txt"] }, isError: false },
    });

    expect(activities.map((activity) => activity.activityId)).toEqual([
      "model.reasoning.command.1.turn.1",
      "tool.call.1",
    ]);
    expect(activities[0]).toMatchObject({
      body: "Need files",
      output: "Need files",
      firstSequence: 1,
    });
    expect(activities[1]).toMatchObject({
      status: "completed",
      firstSequence: 3,
      input: { callId: "call.1", arguments: { path: "." } },
      output: { callId: "call.1", content: { files: ["a.txt"] }, isError: false },
    });

    const stale = reduceRunEventProjection(activities, {
      ...activities[1]!,
      sequence: 3,
      eventId: "run.event.stale",
      body: "stale",
      status: "running",
    });
    expect(stale).toEqual(activities);
    expect(() =>
      reduceRunEventProjection(activities, {
        schemaVersion: 1,
        requestId: "command.1",
        runId: "run.1",
        sequence: 7,
        eventId: "run.event.gap",
        activityId: "model.turn.command.1.2",
        kind: "model_turn",
        title: "Model turn 2",
        body: "started",
        status: "running",
        dataMode: "replace",
      }),
    ).toThrow("expected sequence 5, received 7");
  });
});
