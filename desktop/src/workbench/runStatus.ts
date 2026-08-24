import type { WorkflowDocument } from "./workflow";

/** Per-node execution status projected from committed node.* runtime facts. */
export type NodeRunStatus =
  | "idle"
  | "running"
  | "completed"
  | "failed"
  | "waiting"
  | "skipped";

/** One committed node lifecycle fact (`node.completed`, `node.failed`, etc.). */
export interface NodeRunFact {
  readonly kind: string;
  readonly nodeId: string;
  readonly status: string;
}

const STATUS_KINDS: Readonly<Record<string, NodeRunStatus>> = {
  "node.completed": "completed",
  "node.failed": "failed",
  "node.waiting": "waiting",
  "node.skipped": "skipped",
  "node.running": "running",
};

/**
 * Projects each document node to its latest committed run status. Facts arrive
 * in committed order; the newest fact for a node wins. Nodes without a fact are
 * idle. Facts reference unknown node ids and are ignored without error.
 */
export function projectNodeRunStatus(
  document: WorkflowDocument,
  facts: readonly NodeRunFact[],
): ReadonlyMap<string, NodeRunStatus> {
  const statuses = new Map<string, NodeRunStatus>();
  document.nodes.forEach((node, index) => {
    statuses.set(
      typeof node.id === "string" ? node.id : `node-${index}`,
      "idle",
    );
  });
  for (const fact of facts) {
    const status = STATUS_KINDS[fact.kind];
    if (status === undefined || fact.nodeId.trim() === "") continue;
    if (!statuses.has(fact.nodeId)) continue;
    statuses.set(fact.nodeId, status);
  }
  return statuses;
}

/** Extracts run facts from raw committed events (shape: kind + payload.nodeId/status). */
export function nodeRunFactsFromEvents(
  events: readonly {
    readonly kind: string;
    readonly payload?: unknown;
  }[],
): readonly NodeRunFact[] {
  const facts: NodeRunFact[] = [];
  for (const event of events) {
    if (!event.kind.startsWith("node.")) continue;
    const payload =
      typeof event.payload === "object" && event.payload !== null
        ? (event.payload as Record<string, unknown>)
        : {};
    const nodeId = typeof payload.nodeId === "string" ? payload.nodeId : "";
    const status =
      typeof payload.status === "string" ? payload.status : event.kind.slice("node.".length);
    if (nodeId === "") continue;
    facts.push({ kind: event.kind, nodeId, status });
  }
  return facts;
}
