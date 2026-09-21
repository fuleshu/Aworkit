import { describe, expect, it } from "vitest";
import {
  hasRunningSubagent,
  orderedSubagents,
  subagentCatalog,
} from "./subagentCatalog";
import type { RuntimeEvent } from "./corePort";

const child = (sequence: number, payload: unknown): RuntimeEvent => ({
  schemaVersion: 1,
  sequence,
  eventId: `e.${sequence}`,
  streamId: "chat.subagents",
  branchId: "main",
  kind: "context.subagent-child",
  payload,
});

describe("subagent catalog projection", () => {
  it("folds the newest lifecycle fact per child and ignores malformed facts", () => {
    const catalog = subagentCatalog([
      child(1, { childId: "child.a", kind: "fresh", status: "running", task: "Research VR", modelTurns: 0 }),
      child(2, { childId: "child.b", kind: "fork", status: "running", task: "Fork task" }),
      child(3, { childId: "child.a", status: "completed", modelTurns: 3, toolCalls: 2, inputTokens: 40, outputTokens: 9 }),
      child(4, { childId: "", status: "running" }),
      child(5, { childId: "child.c", status: "bogus" }),
      child(6, { wrong: true }),
    ]);
    expect(catalog.map((entry) => entry.childId)).toEqual(["child.a", "child.b"]);
    expect(catalog[0]).toMatchObject({
      childId: "child.a",
      kind: "fresh",
      status: "completed",
      task: "Research VR",
      modelTurns: 3,
      toolCalls: 2,
      inputTokens: 40,
      outputTokens: 9,
    });
    expect(catalog[1]).toMatchObject({ childId: "child.b", kind: "fork", status: "running" });
  });

  it("keeps the fold order-independent by committed sequence", () => {
    const catalog = subagentCatalog([
      child(2, { childId: "child.a", status: "cancelled" }),
      child(1, { childId: "child.a", status: "running" }),
    ]);
    expect(catalog).toHaveLength(1);
    expect(catalog[0].status).toBe("cancelled");
    expect(hasRunningSubagent(catalog)).toBe(false);
  });

  it("orders running children first and then settled children newest first", () => {
    const entries = subagentCatalog([
      child(1, { childId: "child.old", status: "completed", updatedAt: "2026-09-21T10:00:00Z" }),
      child(2, { childId: "child.new", status: "completed", updatedAt: "2026-09-21T12:00:00Z" }),
      child(3, { childId: "child.live", status: "running", updatedAt: "2026-09-21T09:00:00Z" }),
    ]);
    expect(orderedSubagents(entries).map((entry) => entry.childId)).toEqual([
      "child.live",
      "child.new",
      "child.old",
    ]);
    expect(hasRunningSubagent(entries)).toBe(true);
  });
});
