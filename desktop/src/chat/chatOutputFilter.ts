import {
  isModelCallSpan,
  modelCallAssistantOutput,
  modelCallReasoning,
} from "./modelCallChannels";
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
 * unchecked. A model-call span is normally kept even though it carries
 * reasoning, because it also carries the model's final answer; its reasoning is
 * hidden inside the card through `ModelCallBlock.hideThinking`. A model-call
 * span whose only human-readable content is reasoning, however, would render as
 * an empty Agent block, so it is dropped with the reasoning it was showing.
 */
export function filterTimelineItems(
  items: readonly TimelineItem[],
  filter: ChatOutputFilterState,
): TimelineItem[] {
  return items.filter((item) => {
    const modelCall = isModelCallSpan(item);
    if (!filter.thinking) {
      // Standalone reasoning entries disappear with the box. A model-call span
      // is not one: it carries the model's final answer too, so its reasoning is
      // hidden inside the card instead. It is dropped whole only when the
      // reasoning was its entire human-readable content.
      if (!modelCall && item.kind === "thinking") return false;
      if (
        modelCall &&
        modelCallReasoning(item).length > 0 &&
        modelCallAssistantOutput(item).length === 0
      ) {
        return false;
      }
    }
    if (!filter.tools && isToolEntry(item)) return false;
    return true;
  });
}
