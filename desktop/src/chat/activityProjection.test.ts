import { describe, expect, it } from "vitest";
import { deriveActivityCards, mergeTimeline } from "./activityProjection";
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
});
