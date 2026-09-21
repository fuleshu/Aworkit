import type { RuntimeEvent } from "./corePort";

/** The Chat's durable goal as carried by the canonical tool.goal fact. */
export interface ChatGoal {
  readonly status: "active" | "completed" | "cleared";
  readonly objective?: string;
  readonly note?: string;
}

const STATUSES: readonly ChatGoal["status"][] = ["active", "completed", "cleared"];

/**
 * Latest durable goal in the committed stream. Later facts supersede earlier
 * ones, so the fold keeps the last valid snapshot in sequence order; a
 * malformed fact is ignored rather than replacing a good goal.
 */
export function currentGoal(events: readonly RuntimeEvent[]): ChatGoal | null {
  let latest: ChatGoal | null = null;
  for (const event of [...events].sort((left, right) => left.sequence - right.sequence)) {
    if (event.kind !== "tool.goal") continue;
    const goal = record(record(event.payload).goal);
    const status = STATUSES.find((candidate) => candidate === goal.status);
    if (status === undefined) continue;
    latest = { status, objective: text(goal.goal), note: text(goal.note) };
  }
  return latest;
}

/** Whether a goal still carries an objective the Chat is working toward. */
export function isLiveGoal(goal: ChatGoal | null): boolean {
  return goal !== null && goal.status !== "cleared";
}

function record(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

function text(value: unknown): string | undefined {
  return typeof value === "string" && value.length > 0 ? value : undefined;
}
