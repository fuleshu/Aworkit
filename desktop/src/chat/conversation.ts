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
  thinking: "Thinking",
  plan: "Plan",
  step: "Step",
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

/** Escapes source strings for non-React serialization surfaces. */
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
    // React escapes text nodes itself. Pre-escaping here would render entities
    // such as &quot; literally and corrupt streamed model output.
    content: item.body ?? item.title,
    reasoningLabel: reasoningLabel(item.reasoningCategory),
    action: item.action,
    inspectable: unknown || item.raw !== undefined,
  };
}

function reasoningLabel(
  category: TimelineItem["reasoningCategory"],
): string | undefined {
  if (category === "summary") return "Provider reasoning summary";
  if (category === "progress") return "Provider progress";
  if (category === "source_provided") return "Provider-supplied reasoning";
  return undefined;
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
