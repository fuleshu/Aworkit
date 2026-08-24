import { describe, expect, it } from "vitest";
import {
  nodeRunFactsFromEvents,
  projectNodeRunStatus,
  type NodeRunFact,
} from "./runStatus";
import type { WorkflowDocument } from "./workflow";

const document: WorkflowDocument = {
  schemaVersion: 1,
  nodes: [
    { id: "input.1", type: "input" },
    { id: "agent.1", type: "agent" },
    { id: "output.1", type: "output" },
  ],
  edges: [],
};

describe("per-node run status projection", () => {
  it("defaults every node to idle without facts", () => {
    const statuses = projectNodeRunStatus(document, []);
    expect(statuses.get("input.1")).toBe("idle");
    expect(statuses.get("agent.1")).toBe("idle");
    expect(statuses.get("output.1")).toBe("idle");
  });

  it("projects the newest committed fact per node and ignores unknown ids", () => {
    const facts: readonly NodeRunFact[] = [
      { kind: "node.completed", nodeId: "input.1", status: "completed" },
      { kind: "node.completed", nodeId: "agent.1", status: "completed" },
      { kind: "node.failed", nodeId: "agent.1", status: "failed" },
      { kind: "node.completed", nodeId: "ghost.9", status: "completed" },
    ];
    const statuses = projectNodeRunStatus(document, facts);
    expect(statuses.get("input.1")).toBe("completed");
    expect(statuses.get("agent.1")).toBe("failed");
    expect(statuses.get("output.1")).toBe("idle");
    expect(statuses.has("ghost.9")).toBe(false);
  });

  it("extracts node facts from raw committed events", () => {
    const facts = nodeRunFactsFromEvents([
      {
        kind: "node.completed",
        payload: { nodeId: "agent.1", status: "completed", body: "done" },
      },
      { kind: "message.user", payload: { body: "hi" } },
      { kind: "node.waiting", payload: { nodeId: "approval.1", status: "waiting" } },
    ]);
    expect(facts).toEqual([
      { kind: "node.completed", nodeId: "agent.1", status: "completed" },
      { kind: "node.waiting", nodeId: "approval.1", status: "waiting" },
    ]);
  });
});
