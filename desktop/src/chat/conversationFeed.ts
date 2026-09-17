import type { RuntimeEvent } from "./corePort";
import type { TimelineItem } from "./types";

const record = (value: unknown): Record<string, unknown> =>
  value !== null && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};

/** Parent execution spans supply inspection context, not a header repeated
 * before every page. Their terminal notices belong at the time they occurred. */
export function conversationFeed(items: readonly TimelineItem[], firstSequence = 1): TimelineItem[] {
  const byId = new Map(items.map(item => [item.id, item]));
  const containers = new Set(items.filter(isContainer).map(item => item.id));
  const hasChildren = new Set<string>();
  const ancestors = (item: TimelineItem): TimelineItem[] => {
    const result: TimelineItem[] = [], visited = new Set<string>();
    let parent = item.parentSpanId;
    while (parent && !visited.has(parent)) {
      visited.add(parent);
      const owner = byId.get(parent);
      if (!owner) break;
      result.push(owner); parent = owner.parentSpanId;
    }
    return result;
  };
  for (const item of items) {
    if (!containers.has(item.id)) for (const parent of ancestors(item)) hasChildren.add(parent.id);
  }
  const feed = items.flatMap((item): TimelineItem[] => {
    const source = Array.isArray(item.raw) ? item.raw as RuntimeEvent[] : [];
    if (!containers.has(item.id)) {
      const belongs = source.length ? source.some(event => event.sequence >= firstSequence) : (item.sequence ?? firstSequence) >= firstSequence;
      return belongs ? [item] : [];
    }
    // The workflow node owns the same lifecycle as its nested agent loop.
    if (record(item.metadata).spanKind === "agent_loop" && ancestors(item).some(parent => containers.has(parent.id))) return [];
    const terminal = source.filter(event => ["span.completed", "span.failed", "span.cancelled"].includes(event.kind)).at(-1);
    if (hasChildren.has(item.id) && (!terminal || terminal.kind === "span.completed")) return [];
    const sequence = terminal?.sequence ?? item.sequence ?? firstSequence;
    if (sequence < firstSequence) return [];
    const fact = record(terminal?.payload);
    return [{ ...item, sequence, createdAt: typeof fact.createdAt === "string" ? fact.createdAt : item.createdAt,
      kind: item.status === "failed" ? "error" : "step",
      metadata: { ...record(item.metadata), feedStatus: true },
    }];
  });
  const visible = new Set(feed.filter(item => !containers.has(item.id)).map(item => item.id));
  return feed.map(item => ({ ...item, depth: ancestors(item).filter(parent => visible.has(parent.id)).length }))
    .sort((left, right) => (left.sequence ?? 0) - (right.sequence ?? 0));
}

function isContainer(item: TimelineItem): boolean {
  const metadata = record(item.metadata);
  return metadata.spanKind === "agent_loop" || (metadata.spanKind === "graph_node" && ["agent", "model_call"].includes(String(metadata.semanticRole)));
}

/** Supporting records can span several raw pages without adding an activity. */
export function hasEarlierActivity(before: readonly TimelineItem[], after: readonly TimelineItem[]): boolean {
  const known = new Set(before.map(item => item.id));
  const first = before[0]?.sequence ?? Number.POSITIVE_INFINITY;
  return after.some(item => !known.has(item.id) && (item.sequence ?? 0) < first);
}
