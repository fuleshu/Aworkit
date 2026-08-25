import type { RuntimeEvent } from "./corePort";
import type { LiveChatActivity, TimelineItem } from "./types";

type FactPayload = Record<string, unknown>;

/**
 * Frontend timeline projection for runtime facts the native timeline projection
 * does not yet surface. The native `timeline` covers messages, approvals,
 * node/route/model cards, and generic tool cards; this module derives the
 * remaining cards (todo lists, subagent runs, MCP calls) from the raw committed
 * fact stream so the Chat timeline stays complete without touching Rust.
 */
export function deriveActivityCards(
  events: readonly RuntimeEvent[],
): TimelineItem[] {
  const cards: TimelineItem[] = [];
  let lastTodo: TimelineItem | undefined;
  for (const event of events) {
    if (event.kind === "tool.todo") {
      lastTodo = todoCard(event);
    } else if (
      event.kind === "subagent.completed" ||
      event.kind === "subagent.failed"
    ) {
      cards.push(subagentCard(event));
    } else if (
      event.kind === "mcp.completed" ||
      event.kind === "mcp.failed"
    ) {
      cards.push(mcpCard(event));
    }
  }
  // "Newest wins": the live task list renders only the most recent todo fact.
  if (lastTodo !== undefined) cards.push(lastTodo);
  return cards;
}

/**
 * Merges native-projected cards with derived cards in committed event order.
 * Both kinds of card use the native `event.chat.{sequence}` event id, so a
 * stable numeric sort reconstructs the exact fact order.
 */
export function mergeTimeline(
  native: readonly TimelineItem[],
  derived: readonly TimelineItem[],
): TimelineItem[] {
  return [...native, ...derived].sort(
    (left, right) => sequenceKey(left) - sequenceKey(right),
  );
}

/** Maps noncanonical in-flight updates to cards that disappear at settle. */
export function deriveLiveActivityCards(
  activities: readonly LiveChatActivity[],
): TimelineItem[] {
  return activities.map((activity) => ({
    id: `live.${activity.activityId}`,
    kind:
      activity.kind === "reasoning"
        ? "thinking"
        : activity.kind === "response"
          ? "model"
          : activity.kind === "step"
            ? "plan"
            : activity.kind,
    title: activity.title,
    body: activity.body,
    createdAt: "now",
    status: activity.status,
    reasoningCategory: activity.reasoningCategory,
    metadata: {
      live: true,
      requestId: activity.requestId,
      runId: activity.runId,
      capabilityId: activity.capabilityId,
    },
  }));
}

function sequenceKey(item: TimelineItem): number {
  const match = /^event\.chat\.(\d+)$/u.exec(item.id);
  return match === null ? Number.MAX_SAFE_INTEGER : Number(match[1]);
}

function payload(event: RuntimeEvent): FactPayload {
  const value = event.payload;
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as FactPayload)
    : {};
}

function eventId(event: RuntimeEvent): string {
  return typeof event.eventId === "string"
    ? event.eventId
    : `event.chat.${event.sequence}`;
}

function createdAt(event: RuntimeEvent): string {
  const value = payload(event).createdAt;
  return typeof value === "string" ? value : "";
}

function todoCard(event: RuntimeEvent): TimelineItem {
  const fact = payload(event);
  const todos = Array.isArray(fact.todos) ? fact.todos : [];
  return {
    id: eventId(event),
    kind: "todo",
    title: "Task list",
    body: todoSummary(todos),
    createdAt: createdAt(event),
    status: "completed",
    raw: fact,
    metadata: fact,
  };
}

function todoSummary(todos: readonly unknown[]): string {
  return todos
    .map((todo) => {
      if (typeof todo !== "object" || todo === null || Array.isArray(todo))
        return String(todo);
      const record = todo as Record<string, unknown>;
      const content =
        typeof record.content === "string" ? record.content : String(record.content ?? "");
      const status = typeof record.status === "string" ? record.status : "";
      return status === "" ? content : `[${status}] ${content}`;
    })
    .join("\n");
}

function subagentCard(event: RuntimeEvent): TimelineItem {
  const fact = payload(event);
  const capability = typeof fact.capabilityId === "string" ? fact.capabilityId : "";
  return {
    id: eventId(event),
    kind: "subagent",
    title: capability === "" ? "Subagent" : "Subagent run",
    body: typeof fact.body === "string" ? fact.body : "",
    createdAt: createdAt(event),
    status: event.kind === "subagent.completed" ? "completed" : "failed",
    raw: fact,
    metadata: fact,
  };
}

function mcpCard(event: RuntimeEvent): TimelineItem {
  const fact = payload(event);
  const capability = typeof fact.capabilityId === "string" ? fact.capabilityId : "mcp";
  return {
    id: eventId(event),
    kind: "mcp",
    title: capability,
    body: typeof fact.body === "string" ? fact.body : "",
    createdAt: createdAt(event),
    status: event.kind === "mcp.completed" ? "completed" : "failed",
    raw: fact,
    metadata: fact,
  };
}
