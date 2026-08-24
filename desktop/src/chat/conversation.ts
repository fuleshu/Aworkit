import type { TimelineItem } from "./types";

export interface ConversationCard {
  readonly id: string;
  readonly label: string;
  readonly content: string;
  readonly reasoningLabel?: string;
  readonly action?: TimelineItem["action"];
  readonly inspectable: boolean;
}

const labels: Record<TimelineItem["kind"], string> = {
  message: "Message",
  plan: "Plan",
  model: "Model",
  tool: "Tool",
  mcp: "MCP",
  plugin: "Plugin",
  subagent: "Subagent",
  external_agent: "External agent",
  artifact: "Artifact",
  approval: "Approval",
  route: "Route",
  todo: "Task list",
  error: "Error",
  verification: "Verification",
  repair: "Repair",
  unknown: "Unknown activity",
};

/** Escapes all rich source strings: the timeline never trusts provider-provided markup. */
export function escapedText(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

export function toConversationCard(item: TimelineItem): ConversationCard {
  const unknown = item.kind === "unknown";
  return {
    id: item.id,
    label: labels[item.kind],
    content: escapedText(item.body ?? item.title),
    reasoningLabel:
      item.reasoningCategory === undefined
        ? undefined
        : `Source-provided ${item.reasoningCategory.replaceAll("_", " ")}`,
    action: item.action,
    inspectable: unknown || item.raw !== undefined,
  };
}

/** A small windowing helper used by adapters; domain timeline data stays complete. */
export function visibleTimeline(
  items: readonly TimelineItem[],
  start: number,
  count: number,
): readonly TimelineItem[] {
  return items.slice(
    Math.max(0, start),
    Math.max(0, start) + Math.max(0, count),
  );
}
