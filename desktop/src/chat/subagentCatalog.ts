import type { RuntimeEvent } from "./corePort";

/** Terminal and live states of one delegated child, as committed by the core. */
export type SubagentStatus =
  | "running"
  | "completed"
  | "parent_approval_required"
  | "failed"
  | "interrupted"
  | "cancelled";

/** One delegated child of the current Chat, folded from committed facts. */
export interface SubagentCatalogEntry {
  readonly childId: string;
  readonly kind: "fresh" | "fork";
  readonly status: SubagentStatus;
  readonly task: string;
  readonly nodeId?: string;
  readonly modelTurns: number;
  readonly toolCalls: number;
  readonly inputTokens: number;
  readonly outputTokens: number;
  readonly createdAt?: string;
  readonly updatedAt?: string;
}

const STATUSES: readonly SubagentStatus[] = [
  "running",
  "completed",
  "parent_approval_required",
  "failed",
  "interrupted",
  "cancelled",
];

/**
 * Folds the Chat's committed `context.subagent-child` facts into the child
 * catalog. Later facts for one child supersede earlier ones, so the newest
 * status per childId wins; a malformed fact is ignored rather than replacing a
 * good entry. Facts are bounded metadata: a child's conversation and evidence
 * stay operational and are read from its own span scope.
 */
export function subagentCatalog(
  events: readonly RuntimeEvent[],
): SubagentCatalogEntry[] {
  const byId = new Map<string, SubagentCatalogEntry>();
  for (const event of [...events].sort(
    (left, right) => left.sequence - right.sequence,
  )) {
    if (event.kind !== "context.subagent-child") continue;
    const fact = record(event.payload);
    const childId = text(fact.childId);
    const status = STATUSES.find((candidate) => candidate === fact.status);
    if (childId === undefined || status === undefined) continue;
    const previous = byId.get(childId);
    byId.set(childId, {
      childId,
      kind: fact.kind === "fork" ? "fork" : (previous?.kind ?? "fresh"),
      status,
      task: text(fact.task) ?? previous?.task ?? "",
      nodeId: text(fact.nodeId) ?? previous?.nodeId,
      modelTurns: number(fact.modelTurns, previous?.modelTurns),
      toolCalls: number(fact.toolCalls, previous?.toolCalls),
      inputTokens: number(fact.inputTokens, previous?.inputTokens),
      outputTokens: number(fact.outputTokens, previous?.outputTokens),
      createdAt: text(fact.createdAt) ?? previous?.createdAt,
      updatedAt: text(fact.updatedAt) ?? previous?.updatedAt,
    });
  }
  return [...byId.values()];
}

/** Whether any child of this Chat is still running. */
export function hasRunningSubagent(
  entries: readonly SubagentCatalogEntry[],
): boolean {
  return entries.some((entry) => entry.status === "running");
}

/**
 * Dialog and tab order: running children first, then settled ones from newest
 * to oldest. The order is stable for equal timestamps by childId.
 */
export function orderedSubagents(
  entries: readonly SubagentCatalogEntry[],
): SubagentCatalogEntry[] {
  return [...entries].sort((left, right) => {
    const running = Number(right.status === "running") - Number(left.status === "running");
    if (running !== 0) return running;
    const recent = stamp(right) - stamp(left);
    if (recent !== 0) return recent;
    return left.childId.localeCompare(right.childId);
  });
}

/** Epoch milliseconds of the newest known timestamp; 0 when none is valid. */
function stamp(entry: SubagentCatalogEntry): number {
  for (const value of [entry.updatedAt, entry.createdAt]) {
    if (value === undefined) continue;
    const parsed = Date.parse(value);
    if (!Number.isNaN(parsed)) return parsed;
  }
  return 0;
}

function record(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

function text(value: unknown): string | undefined {
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

function number(value: unknown, previous = 0): number {
  return typeof value === "number" && Number.isFinite(value) ? value : previous;
}
