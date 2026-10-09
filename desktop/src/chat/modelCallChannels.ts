import type { TimelineItem } from "./types";

/** The streamed text channels of one model-call span. */
export interface ModelCallChannelText {
  readonly reasoning: string;
  readonly progress: string;
  readonly assistantOutput: string;
}

/** True only for the provider-call span, not its workflow-node container. */
export function isModelCallSpan(item: TimelineItem): boolean {
  return record(item.metadata).spanKind === "model_call";
}

/** Exact accumulated assistant text emitted by the model-call stream. */
export function modelCallAssistantOutput(item: TimelineItem): string {
  return modelCallChannelText(item).assistantOutput;
}

/**
 * Reasoning plus progress: the human-readable "thinking" of one call.
 *
 * This is what the Chat output filter hides, so a caller can tell whether a
 * model-call card would be left with nothing to show.
 */
export function modelCallReasoning(item: TimelineItem): string {
  const channels = modelCallChannelText(item);
  return [channels.reasoning, channels.progress]
    .filter((value) => value.length > 0)
    .join("\n");
}

function modelCallChannelText(item: TimelineItem): ModelCallChannelText {
  const channels = record(record(item.metadata).channels);
  return {
    reasoning: text(channels.reasoning),
    progress: text(channels.progress),
    assistantOutput: text(channels.assistantOutput),
  };
}

function record(value: unknown): Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

function text(value: unknown): string {
  return typeof value === "string" ? value : "";
}
