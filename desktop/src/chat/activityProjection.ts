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
  return activities
    .filter(shouldRenderLiveActivity)
    .map((activity) => ({
    id: `live.${activity.activityId}`,
    kind:
      activity.kind === "reasoning"
        ? "thinking"
        : activity.kind === "progress"
          ? "thinking"
        : activity.kind === "response"
          ? "model"
          : activity.kind === "model_turn"
            ? "model"
          : activity.kind === "step"
            ? "step"
            : activity.kind,
    title: activity.title,
    body: activity.body,
    createdAt: "now",
    status: activity.status,
    reasoningCategory: activity.reasoningCategory,
    input: activity.input,
    output: activity.output,
    metadata: {
      live: true,
      sequence: activity.sequence,
      firstSequence: activity.firstSequence,
      eventId: activity.eventId,
      activityId: activity.activityId,
      requestId: activity.requestId,
      runId: activity.runId,
      input: activity.input,
      output: activity.output,
      turn: activity.turn,
      nodeId: activity.nodeId,
      nodeType: activity.nodeType,
      callId: activity.callId,
      capabilityId: activity.capabilityId,
    },
    }));
}

/**
 * A successful output node only transfers the already-streamed assistant
 * response into the graph result. Failures remain visible as useful evidence.
 */
function shouldRenderLiveActivity(activity: LiveChatActivity): boolean {
  return activity.nodeType !== "output" || activity.status === "failed";
}

/**
 * Pure Run-event reducer. It removes the optimistic busy placeholder on the
 * first native event, ignores stale transitions, folds streamed deltas into
 * one activity, and preserves the activity's first-observed sequence order.
 */
export function reduceRunEventProjection(
  activities: readonly LiveChatActivity[],
  incoming: LiveChatActivity,
): LiveChatActivity[] {
  const current = incoming.activityId.startsWith("busy.")
    ? [...activities]
    : activities.filter(
        (activity) =>
          !activity.activityId.startsWith("busy.") ||
          (activity.requestId !== incoming.requestId &&
            activity.runId !== incoming.runId),
      );
  if (incoming.sequence !== undefined) {
    const priorSequences = current
      .filter(
        (activity) =>
          activity.requestId === incoming.requestId &&
          activity.runId === incoming.runId &&
          activity.sequence !== undefined,
      )
      .map((activity) => activity.sequence!);
    const latestSequence =
      priorSequences.length === 0 ? undefined : Math.max(...priorSequences);
    if (
      latestSequence !== undefined &&
      incoming.sequence > latestSequence + 1
    )
      throw new Error(
        `Run-event gap: expected sequence ${latestSequence + 1}, received ${incoming.sequence}`,
      );
    if (
      latestSequence !== undefined &&
      incoming.sequence <= latestSequence &&
      !current.some(
        (activity) =>
          activity.activityId === incoming.activityId &&
          activity.sequence !== undefined &&
          incoming.sequence! > activity.sequence,
      )
    )
      return current;
  }
  const index = current.findIndex(
    (activity) => activity.activityId === incoming.activityId,
  );
  if (index < 0) {
    return [...current, { ...incoming, firstSequence: incoming.sequence }].sort(
      compareLiveActivityOrder,
    );
  }
  const previous = current[index];
  if (
    incoming.sequence !== undefined &&
    previous.sequence !== undefined &&
    incoming.sequence <= previous.sequence
  )
    return current;
  const next = [...current];
  next[index] = {
    ...previous,
    ...incoming,
    firstSequence: previous.firstSequence ?? previous.sequence ?? incoming.sequence,
    body:
      incoming.dataMode === "append"
        ? `${previous.body}${incoming.body}`
        : incoming.dataMode === "retain"
          ? previous.body
          : incoming.body,
    input: incoming.input ?? previous.input,
    output: reduceLiveOutput(previous.output, incoming),
  };
  return next.sort(compareLiveActivityOrder);
}

function reduceLiveOutput(
  previous: unknown,
  incoming: LiveChatActivity,
): unknown {
  if (incoming.dataMode === "retain") return previous;
  if (
    incoming.dataMode === "append" &&
    typeof previous === "string" &&
    typeof incoming.output === "string"
  )
    return previous + incoming.output;
  return incoming.output ?? previous;
}

function compareLiveActivityOrder(
  left: LiveChatActivity,
  right: LiveChatActivity,
): number {
  return (
    (left.firstSequence ?? left.sequence ?? Number.MAX_SAFE_INTEGER) -
    (right.firstSequence ?? right.sequence ?? Number.MAX_SAFE_INTEGER)
  );
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
