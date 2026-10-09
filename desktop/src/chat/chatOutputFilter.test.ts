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
});
