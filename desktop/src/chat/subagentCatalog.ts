import type { RuntimeEvent, SubagentChildSummary } from "./corePort";

/** Terminal and live states of one delegated child, as committed by the core. */
export type SubagentStatus =
  | "running"
  | "completed"
  | "parent_approval_required"
  | "failed"
  | "interrupted"
  | "cancelled";

/** One delegated child of the current Chat. */
export interface SubagentCatalogEntry {
  readonly childId: string;
  readonly kind: "fresh" | "fork";
  readonly status: SubagentStatus;
  readonly task: string;
  /** The extra context the delegating Agent supplied with the task. */
  readonly contextText: string;
  /** The child's settled answer, empty while it runs. */
  readonly finalText: string;
  readonly nodeId?: string;
  readonly parentInvocationId?: string;
  /** The delegating tool call that spawned this child, when known. */
  readonly parentCallId?: string;
  readonly parentChildId?: string;
  readonly depth: number;
  readonly headRevision: number;
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

/** A child status is terminal once it can no longer make progress by itself. */
export function isTerminalSubagentStatus(status: SubagentStatus): boolean {
  return status !== "running";
}

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
      contextText: text(fact.contextText) ?? previous?.contextText ?? "",
      finalText: text(fact.finalText) ?? previous?.finalText ?? "",
      nodeId: text(fact.nodeId) ?? previous?.nodeId,
      parentInvocationId:
        text(fact.parentInvocationId) ?? previous?.parentInvocationId,
      parentCallId: text(fact.parentCallId) ?? previous?.parentCallId,
      parentChildId: text(fact.parentChildId) ?? previous?.parentChildId,
      depth: number(fact.depth, previous?.depth),
      headRevision: number(fact.headRevision, previous?.headRevision),
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

/** One summary from the authoritative core catalog as a presentation entry. */
export function fromSubagentSummary(
  summary: SubagentChildSummary,
): SubagentCatalogEntry {
  return {
    childId: summary.childId,
    kind: summary.kind,
    status: summary.status,
    task: summary.task,
    contextText: summary.contextText,
    finalText: summary.finalText,
    nodeId: summary.nodeId,
    parentInvocationId: summary.parentInvocationId,
    parentCallId: summary.parentCallId === "" ? undefined : summary.parentCallId,
    parentChildId: summary.parentChildId ?? undefined,
    depth: summary.depth,
    headRevision: summary.headRevision,
    modelTurns: summary.modelTurns,
    toolCalls: summary.toolCalls,
    inputTokens: summary.inputTokens,
    outputTokens: summary.outputTokens,
    createdAt: summary.createdAt,
    updatedAt: summary.updatedAt,
  };
}

/**
 * Merges the authoritative snapshot catalog with the live committed fold.
 *
 * Durable frames are the source of truth for what a child *is*: the snapshot
 * reports a child whose job is no longer live as `interrupted` even though its
 * last committed fact still said `running`. A later committed fact therefore
 * supersedes the snapshot only when it advanced the child's revision, and a
 * `running` fact can never overwrite an authoritative `interrupted` one.
 */
export function mergeSubagentCatalog(
  snapshot: readonly SubagentChildSummary[] | undefined,
  folded: readonly SubagentCatalogEntry[],
): SubagentCatalogEntry[] {
  const byId = new Map<string, SubagentCatalogEntry>();
  for (const summary of snapshot ?? []) {
    byId.set(summary.childId, fromSubagentSummary(summary));
  }
  for (const entry of folded) {
    const known = byId.get(entry.childId);
    if (known === undefined) {
      byId.set(entry.childId, entry);
      continue;
    }
    if (entry.headRevision > known.headRevision) {
      byId.set(entry.childId, entry);
      continue;
    }
    if (entry.headRevision < known.headRevision) continue;
    // Same revision: the durable snapshot is richer (assigned context, settled
    // answer), so only a genuinely newer live status replaces it. A `running`
    // fact can never overwrite an authoritative `interrupted` frame.
    if (known.status === "interrupted" && entry.status === "running") continue;
    if (entry.status !== known.status) {
      byId.set(entry.childId, {
        ...known,
        status: entry.status,
        updatedAt: entry.updatedAt ?? known.updatedAt,
      });
    }
  }
  return [...byId.values()];
}

/** The whole catalog for one Chat: authoritative projection plus live facts. */
export function chatSubagentCatalog(
  snapshot: readonly SubagentChildSummary[] | undefined,
  events: readonly RuntimeEvent[],
): SubagentCatalogEntry[] {
  return orderedSubagents(
    mergeSubagentCatalog(snapshot, subagentCatalog(events)),
  );
}

/** Whether any child of this Chat is still running. */
export function hasRunningSubagent(
  entries: readonly SubagentCatalogEntry[],
): boolean {
  return entries.some((entry) => entry.status === "running");
}

/** One child by its durable identity, if this Chat owns it. */
export function subagentEntry(
  entries: readonly SubagentCatalogEntry[],
  childId: string,
): SubagentCatalogEntry | undefined {
  return entries.find((entry) => entry.childId === childId);
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
