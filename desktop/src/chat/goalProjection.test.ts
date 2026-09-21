import { describe, expect, it } from "vitest";
import { currentGoal, isLiveGoal } from "./goalProjection";
import type { RuntimeEvent } from "./corePort";

const event = (sequence: number, payload: unknown): RuntimeEvent => ({
  schemaVersion: 1,
  sequence,
  eventId: `e.${sequence}`,
  streamId: "chat.goal",
  branchId: "main",
  kind: "tool.goal",
  payload,
});

describe("chat goal projection", () => {
  it("folds the latest durable goal in sequence order and keeps its note", () => {
    expect(currentGoal([])).toBeNull();
    const goal = currentGoal([
      event(2, { goal: { status: "active", goal: "Ship 108", note: "blocked on tests" } }),
      event(1, { goal: { status: "active", goal: "Old objective" } }),
    ]);
    expect(goal).toEqual({
      status: "active",
      objective: "Ship 108",
      note: "blocked on tests",
    });
    expect(isLiveGoal(goal)).toBe(true);
  });

  it("treats a cleared goal as not live and ignores malformed facts", () => {
    const cleared = currentGoal([
      event(1, { goal: { status: "active", goal: "Ship" } }),
      event(2, { goal: { status: "cleared" } }),
      event(3, { goal: { status: "bogus" } }),
      event(4, { wrong: true }),
    ]);
    expect(cleared).toEqual({ status: "cleared" });
    expect(isLiveGoal(cleared)).toBe(false);
    expect(isLiveGoal(null)).toBe(false);
  });
});
