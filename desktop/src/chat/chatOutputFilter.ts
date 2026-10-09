import type { TimelineItem } from "./types";

/** Which optional Chat output entries the operator wants to see. */
export interface ChatOutputFilterState {
  /** Provider/agent reasoning entries. */
  readonly thinking: boolean;
  /** Tool and MCP call entries. */
  readonly tools: boolean;
}

/** Both kinds are shown until the operator hides one. */
export const defaultChatOutputFilter: ChatOutputFilterState = {
  thinking: true,
  tools: true,
};

/** True for the tool and MCP entries the `tools` box controls. */
export function isToolEntry(item: TimelineItem): boolean {
  return item.kind === "tool" || item.kind === "mcp";
}

/**
 * The timeline entries the output filter keeps.
 *
 * Standalone reasoning entries (`kind: "thinking"`, e.g. legacy reasoning and
 * folded child activity) and tool/MCP entries are dropped when their box is
 * unchecked. Model-call spans are never dropped here even though they carry
 * reasoning: they also carry the model's final answer, so their reasoning is
 * hidden inside the card through `ModelCallBlock.hideThinking` instead.
 */
export function filterTimelineItems(
  items: readonly TimelineItem[],
  filter: ChatOutputFilterState,
): TimelineItem[] {
  return items.filter(
    (item) =>
      (filter.thinking || item.kind !== "thinking") &&
      (filter.tools || !isToolEntry(item)),
  );
}
