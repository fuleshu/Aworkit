import { describe, expect, it } from "vitest";
import {
  chatSubagentCatalog,
  fromSubagentSummary,
  hasRunningSubagent,
  isTerminalSubagentStatus,
  mergeSubagentCatalog,
  orderedSubagents,
  subagentCatalog,
  subagentEntry,
} from "./subagentCatalog";
import type { RuntimeEvent, SubagentChildSummary } from "./corePort";

const child = (sequence: number, payload: unknown): RuntimeEvent => ({
  schemaVersion: 1,
  sequence,
  eventId: `e.${sequence}`,
  streamId: "chat.subagents",
  branchId: "main",
  kind: "context.subagent-child",
  payload,
});

const summary = (
  overrides: Partial<SubagentChildSummary> = {},
): SubagentChildSummary => ({
  childId: "child.a",
  kind: "fresh",
  status: "running",
  running: true,
  depth: 1,
  nodeId: "node.agent",
  parentInvocationId: "invoke.1",
  parentCallId: "call.1",
  task: "Research VR",
  contextText: "Use the web",
  finalText: "",
  modelTurns: 1,
  toolCalls: 0,
  inputTokens: 10,
  outputTokens: 2,
  headRevision: 0,
  createdAt: "2026-09-21T10:00:00Z",
  updatedAt: "2026-09-21T10:00:00Z",
  ...overrides,
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

  it("never lets a stale running fact overwrite an authoritative interruption", () => {
    const durable = summary({ headRevision: 4 });
    const catalog = mergeSubagentCatalog(
      [durable],
      subagentCatalog([
        child(1, {
          childId: "child.a",
          status: "running",
          headRevision: 4,
          task: "Research VR",
        }),
      ]),
    );
    expect(catalog).toHaveLength(1);
    expect(catalog[0]).toMatchObject({
      status: "running",
      task: "Research VR",
      contextText: "Use the web",
    });
    const interrupted = mergeSubagentCatalog(
      [summary({ status: "interrupted", running: false, headRevision: 4 })],
      subagentCatalog([
        child(1, { childId: "child.a", status: "running", headRevision: 4 }),
      ]),
    );
    expect(interrupted[0].status).toBe("interrupted");
  });

  it("lets a genuinely newer committed fact supersede the durable snapshot", () => {
    const catalog = mergeSubagentCatalog(
      [summary({ status: "running", running: true, headRevision: 1 })],
      subagentCatalog([
        child(2, {
          childId: "child.a",
          status: "completed",
          finalText: "Done",
          headRevision: 2,
        }),
      ]),
    );
    expect(catalog[0]).toMatchObject({ status: "completed", finalText: "Done" });
  });

  it("keeps the richer snapshot fields while applying a same-revision status", () => {
    const catalog = mergeSubagentCatalog(
      [summary({ status: "completed", running: false, headRevision: 3, finalText: "Answer" })],
      subagentCatalog([
        child(1, { childId: "child.a", status: "cancelled", headRevision: 3 }),
      ]),
    );
    expect(catalog[0]).toMatchObject({ status: "cancelled", finalText: "Answer" });
  });

  it("builds the Chat catalog from the durable catalog plus live facts", () => {
    const catalog = chatSubagentCatalog(
      [summary({ childId: "child.durable", status: "interrupted", running: false })],
      [child(1, { childId: "child.live", status: "running", task: "New work" })],
    );
    expect(catalog.map((entry) => entry.childId)).toEqual([
      "child.live",
      "child.durable",
    ]);
    expect(subagentEntry(catalog, "child.live")?.task).toBe("New work");
    expect(subagentEntry(catalog, "child.absent")).toBeUndefined();
    expect(isTerminalSubagentStatus("running")).toBe(false);
    expect(isTerminalSubagentStatus("interrupted")).toBe(true);
    expect(fromSubagentSummary(summary()).parentCallId).toBe("call.1");
  });
});
