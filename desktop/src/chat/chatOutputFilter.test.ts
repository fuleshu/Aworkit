import { describe, expect, it } from "vitest";
import {
  defaultChatOutputFilter,
  filterTimelineItems,
  isToolEntry,
} from "./chatOutputFilter";
import type { TimelineItem } from "./types";

const item = (id: string, kind: TimelineItem["kind"]): TimelineItem => ({
  id,
  kind,
  title: id,
  createdAt: "now",
});

/** One model-call span: a reasoning-bearing span is projected as kind "thinking". */
const modelCall = (
  id: string,
  reasoning: string,
  answer: string,
): TimelineItem => ({
  id,
  kind: "thinking",
  title: id,
  createdAt: "now",
  metadata: {
    spanKind: "model_call",
    channels: { reasoning, progress: "", assistantOutput: answer },
  },
});

describe("chatOutputFilter", () => {
  const items = [
    item("user", "message"),
    item("answer", "message"),
    item("reason", "thinking"),
    item("tool", "tool"),
    item("mcp", "mcp"),
    item("model", "model"),
  ];

  it("keeps everything by default", () => {
    expect(filterTimelineItems(items, defaultChatOutputFilter)).toHaveLength(6);
  });

  it("leaves only user input, model output and other entries when both boxes are off", () => {
    expect(
      filterTimelineItems(items, { thinking: false, tools: false }).map(
        ({ id }) => id,
      ),
    ).toEqual(["user", "answer", "model"]);
  });

  it("drops each kind independently", () => {
    expect(
      filterTimelineItems(items, { thinking: true, tools: false }).map(
        ({ id }) => id,
      ),
    ).toEqual(["user", "answer", "reason", "model"]);
    expect(
      filterTimelineItems(items, { thinking: false, tools: true }).map(
        ({ id }) => id,
      ),
    ).toEqual(["user", "answer", "tool", "mcp", "model"]);
  });

  it("recognises both tool and MCP entries", () => {
    expect(isToolEntry(item("t", "tool"))).toBe(true);
    expect(isToolEntry(item("m", "mcp"))).toBe(true);
    expect(isToolEntry(item("x", "thinking"))).toBe(false);
  });

  it("keeps a model-call span's answer and drops a reasoning-only one", () => {
    const items = [
      item("user", "message"),
      modelCall("empty", "only reasoning", ""),
      modelCall("answered", "reasoning", "the answer"),
    ];
    // The empty Agent block disappears with the thinking box; the answered one
    // stays because a model-call span is not a standalone thinking entry.
    expect(
      filterTimelineItems(items, { thinking: false, tools: true }).map(
        ({ id }) => id,
      ),
    ).toEqual(["user", "answered"]);
    expect(
      filterTimelineItems(items, { thinking: true, tools: true }),
    ).toHaveLength(3);
  });
});
